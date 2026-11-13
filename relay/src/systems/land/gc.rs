//! land gc: gives back disk that finished jobs no longer need. Never by glob:
//! only the places the rules name, in the clones of jobs nothing holds, and
//! each one a line. Emits (through land's port): collected.
//!
//! The rules (`[gc]`, all on by default):
//! - a job that is not live, and that no queued job waits for or forks, loses
//!   the paths in `build_outputs` from its clone after `outputs_after_hours`;
//!   the source and the branch stay, so a resume only rebuilds;
//! - the same job's scratch directory (its `/tmp`, on disk) goes whole, and so does one whose job is gone;
//!   a read-only job has no clone but has this;
//! - a `done` job's whole clone goes after `clones_after_days`, a failed or
//!   stopped one's after `waiting_retain_days`, once every commit of its
//!   branch is in the repository (else rescued into `refs/relay/rescue/<job>`);
//! - a log (`<job>.log.ndjson`) older than `logs_gzip_after_days` is gzipped;
//! - the ref a review read its branch by (`refs/relay/review/<job>`) goes once the review is gone,
//!   or has ended `outputs_after_hours` ago: a resumed review fetches it again;
//! - an idle copy of the linked caches (`sandbox.linked_caches`) in a pool goes once no run has taken
//!   it for `cache_copies_idle_days`, and so does every idle copy past the `limits.max_running` most
//!   recently used: no more runs than that hold one at once, and every retired one (`gone-*`). A copy is
//!   a whole cache of its own, and frees what it holds;
//! - a clone a killed `relay probe` left (`probe-<pid>-<start>`) goes once no process is both.
//!
//! A clone with a live runner, a symlink, or a path outside relay's own
//! directories is left alone.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::bus::{LandEvent, now_micros};
use crate::config::Gc;
use crate::ctx::{Ctx, SessionRepo};
use crate::os::{self, size_str};
use crate::world::{Job, World};

/// What a pass does to one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// One of `build_outputs`, removed from a clone.
    Outputs,
    /// A whole clone.
    Clone,
    /// A job's scratch directory (its `/tmp`), removed whole.
    Scratch,
    /// A log, gzipped.
    Log,
    /// The ref in the repository a review read its branch by, deleted.
    ReviewRef,
    /// An idle copy of the linked caches, in a pool; its job is "".
    CacheCopy,
    /// A clone `relay probe` made (`probe-<pid>-<start>`) whose process is gone; its job is "".
    Probe,
}

/// One place a pass acts on, for one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub job: String,
    pub kind: Kind,
    pub path: PathBuf,
}

/// What a job's clone is due for.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub outputs: bool,
    pub clone: bool,
}

fn micros(secs: f64) -> u64 {
    u64::try_from(Duration::from_secs_f64(secs.max(0.0)).as_micros()).unwrap_or(u64::MAX)
}

/// Whether a queued job still needs `name`'s branch: it waits for it, or forks it.
fn waited_for(world: &World, name: &str) -> bool {
    world.jobs().values().any(|k| {
        k.status == crate::world::Status::Queued
            && (k.after.iter().any(|a| a == name) || k.fork.as_ref().is_some_and(|f| f.job == name))
    })
}

/// What `j`'s clone is due for at `now_us` (bus time). Nothing for a live job, a
/// job something waits for, or one that has not ended.
pub fn due(world: &World, j: &Job, gc: &Gc, now_us: u64) -> Due {
    let Some(ended) =
        j.ended_at.filter(|_| !j.readonly() && !j.status.is_live() && !j.held && !waited_for(world, &j.name))
    else {
        return Due::default();
    };
    let age = now_us.saturating_sub(ended);
    let keep = if j.status == crate::world::Status::Done { gc.clones_after_days } else { gc.waiting_retain_days };
    Due { outputs: age >= micros(gc.outputs_after_hours * 3600.0), clone: age >= micros(keep * 86_400.0) }
}

/// Whether `j`'s scratch directory is due to go: it has ended `outputs_after_hours` ago and nothing
/// holds it or waits for it. Unlike a clone's outputs, a read-only job has one too.
fn scratch_due(world: &World, j: &Job, gc: &Gc, now_us: u64) -> bool {
    j.ended_at.is_some_and(|ended| {
        !j.status.is_live()
            && !j.held
            && !waited_for(world, &j.name)
            && now_us.saturating_sub(ended) >= micros(gc.outputs_after_hours * 3600.0)
    })
}

/// The scratch directories gc may act on: real directories directly inside relay's own, of a job that is
/// due (see `scratch_due`) or no longer exists.
fn scratches(ctx: &Ctx, world: &World, gc: &Gc, now_us: u64) -> Vec<Item> {
    let mut items = Vec::new();
    for e in std::fs::read_dir(&ctx.scratch).into_iter().flatten().flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let go = match world.jobs().get(&name) {
            Some(j) => !j.runner_alive() && scratch_due(world, j, gc, now_us),
            None => true,
        };
        if go {
            items.push(Item { job: name, kind: Kind::Scratch, path: e.path() });
        }
    }
    items.sort_by(|a, b| a.job.cmp(&b.job));
    items
}

/// `rel` under `base`, when it exists and no part of it is a symlink.
fn inside(base: &Path, rel: &str) -> Option<PathBuf> {
    // The clone itself is not one of its parts.
    Path::new(rel).components().next()?;
    let mut p = base.to_path_buf();
    for c in Path::new(rel).components() {
        let std::path::Component::Normal(c) = c else { return None };
        p.push(c);
        if p.symlink_metadata().ok()?.file_type().is_symlink() {
            return None;
        }
    }
    Some(p)
}

/// Whether `dir` is a real directory (not a symlink) one level inside relay's clones.
fn is_clone(ctx: &Ctx, dir: &Path) -> bool {
    dir.starts_with(&ctx.clones)
        && dir != ctx.clones
        && dir.symlink_metadata().is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

/// Every place a pass would act on at `now_us`, in job order.
pub fn plan(ctx: &Ctx, world: &World, now_us: u64) -> Vec<Item> {
    let gc = &ctx.cfg.gc;
    let log_age = micros(gc.logs_gzip_after_days * 86_400.0);
    let mut items = Vec::new();
    for j in world.jobs().values().filter(|j| !j.status.is_live() && !j.held && !j.runner_alive()) {
        let log = ctx.job_file(&j.name, "log.ndjson");
        let old =
            log.symlink_metadata().ok().filter(std::fs::Metadata::is_file).and_then(|m| m.modified().ok()).is_some_and(
                |t| {
                    let at = t
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0));
                    now_us.saturating_sub(at) >= log_age
                },
            );
        if old {
            items.push(Item { job: j.name.clone(), kind: Kind::Log, path: log });
        }
        let due = due(world, j, gc, now_us);
        let Some(dir) = j.workdir.as_ref().map(SessionRepo::path).filter(|d| is_clone(ctx, d)) else { continue };
        if due.clone && j.branch.is_some() {
            items.push(Item { job: j.name.clone(), kind: Kind::Clone, path: dir.to_path_buf() });
        } else if due.outputs {
            for rel in &gc.build_outputs {
                if let Some(path) = inside(dir, rel) {
                    items.push(Item { job: j.name.clone(), kind: Kind::Outputs, path });
                }
            }
        }
    }
    items.extend(scratches(ctx, world, gc, now_us));
    items.extend(review_refs(ctx, world, gc, now_us));
    items.extend(cache_copies(ctx, gc, now_us));
    items.extend(probe_clones(ctx));
    items
}

/// The clones a killed `relay probe` left (`probe-<pid>-<start>`, `probe-other-…`): no process is that pid
/// with that start time any more. A clone named before probes said their start time goes once its pid is.
fn probe_clones(ctx: &Ctx) -> Vec<Item> {
    let mut items = Vec::new();
    for e in std::fs::read_dir(&ctx.clones).into_iter().flatten().flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        let Some(tag) = name.strip_prefix("probe-other-").or_else(|| name.strip_prefix("probe-")) else { continue };
        let alive = match tag.split_once('-') {
            Some((pid, start)) => match (pid.parse::<u32>(), start.parse::<u64>()) {
                (Ok(pid), Ok(start)) => os::start_time(pid) == Some(start),
                _ => continue,
            },
            None => match tag.parse::<u32>() {
                Ok(pid) => os::start_time(pid).is_some(),
                Err(_) => continue,
            },
        };
        if !alive && e.file_type().is_ok_and(|t| t.is_dir()) {
            items.push(Item { job: String::new(), kind: Kind::Probe, path: e.path() });
        }
    }
    items
}

/// The pools of copies of the linked caches: a session's, beside the clones, and a check's, beside its
/// directory in the jobs directory.
pub fn pools(ctx: &Ctx) -> [PathBuf; 2] {
    [ctx.session_copies.clone(), ctx.check_copies.clone()]
}

/// The idle copies of the linked caches no run needs: unused for `cache_copies_idle_days`, or past the
/// `limits.max_running` used last in their pool.
fn cache_copies(ctx: &Ctx, gc: &Gc, now_us: u64) -> Vec<Item> {
    let stale = micros(gc.cache_copies_idle_days * 86_400.0);
    let mut items = Vec::new();
    for pool in pools(ctx) {
        // One whose removal began and did not finish is removed again.
        for name in crate::sandbox::farm::abandoned(&pool) {
            items.push(Item { job: String::new(), kind: Kind::CacheCopy, path: pool.join(name) });
        }
        let idle = crate::sandbox::farm::idle(&pool);
        let kept = ctx.cfg.limits.max_running;
        for (i, (name, used)) in idle.iter().rev().enumerate() {
            if i >= kept || now_us.saturating_sub(*used) >= stale {
                items.push(Item { job: String::new(), kind: Kind::CacheCopy, path: pool.join(name) });
            }
        }
    }
    items
}

/// The refs reviews read their branches by that no review needs any more: their job is gone, or ended
/// `outputs_after_hours` ago and nothing holds it.
fn review_refs(ctx: &Ctx, world: &World, gc: &Gc, now_us: u64) -> Vec<Item> {
    const PREFIX: &str = "refs/relay/review/";
    let listed = Ctx::git(&ctx.cfg.repo).args(["for-each-ref", "--format=%(refname)", PREFIX]).output();
    let Ok(listed) = listed.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()) else { return Vec::new() };
    let wait = micros(gc.outputs_after_hours * 3600.0);
    listed
        .lines()
        .filter_map(|r| r.strip_prefix(PREFIX).map(|job| (job, r)))
        .filter(|(job, _)| {
            world.jobs().get(*job).is_none_or(|j| {
                !j.status.is_live()
                    && !j.held
                    && !j.runner_alive()
                    && j.ended_at.is_some_and(|t| now_us.saturating_sub(t) >= wait)
            })
        })
        .map(|(job, r)| Item { job: job.to_owned(), kind: Kind::ReviewRef, path: PathBuf::from(r) })
        .collect()
}

/// The branch tip of `branch` in the clone `dir`.
fn tip(dir: &SessionRepo, branch: &str) -> Result<String> {
    dir.rev(&format!("refs/heads/{branch}^{{commit}}")).with_context(|| format!("branch {branch} is not in the clone"))
}

/// Whether the repository holds `commit` under some ref.
fn in_repo(ctx: &Ctx, commit: &str) -> bool {
    let has = Ctx::git(&ctx.cfg.repo).args(["cat-file", "-e", &format!("{commit}^{{commit}}")]).output();
    if !has.is_ok_and(|o| o.status.success()) {
        return false;
    }
    let refs = Ctx::git(&ctx.cfg.repo)
        .args(["for-each-ref", "--contains", commit, "--count=1", "--format=%(refname)"])
        .output();
    refs.is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Makes sure every commit of the job's branch is in the repository before
/// its clone goes: `Some(ref)` when it had to fetch it into a rescue ref.
/// Refuses a clone whose work is not all committed (build outputs aside), or that git could not read
/// whole (any warning from `git status`).
fn secure(ctx: &Ctx, j: &Job, dir: &SessionRepo) -> Result<Option<String>> {
    let branch = j.branch.as_deref().context("the job has no branch")?;
    let tip = tip(dir, branch)?;
    // The session wrote this clone's configuration: plain git here would run its `core.fsmonitor` on the host.
    let mut status = dir.git();
    status.args(["status", "--porcelain", "--", "."]);
    status.args(ctx.cfg.gc.build_outputs.iter().map(|o| format!(":(exclude){o}")));
    let dirty = status.output().context("git status")?;
    if !dirty.status.success() || !dirty.stdout.is_empty() {
        bail!("uncommitted changes in the clone");
    }
    // A warning is git saying it did not see everything (`could not open directory`, a directory a test
    // left at mode 0): what it could not read may be work nobody committed or rescued, and a clone is the
    // session's, not relay's, to repair and delete. Kept, as dirty.
    if !dirty.stderr.iter().all(u8::is_ascii_whitespace) {
        bail!(
            "uncommitted changes in the clone (git status warned: {})",
            String::from_utf8_lossy(&dirty.stderr).trim()
        );
    }
    if in_repo(ctx, &tip) {
        return Ok(None);
    }
    let rescue = format!("refs/relay/rescue/{}", j.name);
    let out = Ctx::git(&ctx.cfg.repo)
        .args(["fetch", "--quiet"])
        .arg(dir.path())
        .arg(format!("+refs/heads/{branch}:{rescue}"))
        .output()?;
    if !out.status.success() {
        bail!("could not rescue the branch: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(Some(rescue))
}

/// Gzips `log` beside itself and removes it. A log that already has a `.gz`
/// (the job ran again after an earlier pass) is appended to it as a new member,
/// which `gzip -d` reads as one stream.
fn gzip_log(log: &Path) -> Result<()> {
    let mut gz = log.as_os_str().to_owned();
    gz.push(".gz");
    let (gz, tmp) = (
        PathBuf::from(&gz),
        PathBuf::from({
            let mut t = gz.clone();
            t.push(".tmp");
            t
        }),
    );
    let done = (|| -> Result<()> {
        let status = std::process::Command::new("gzip")
            .arg("-c")
            .arg(log)
            .stdout(std::fs::File::create(&tmp)?)
            .status()
            .context("running gzip")?;
        if !status.success() {
            bail!("gzip failed");
        }
        if gz.exists() {
            let mut into = Ctx::append_private(&gz)?;
            std::io::copy(&mut std::fs::File::open(&tmp)?, &mut into)?;
            std::fs::remove_file(&tmp)?;
        } else {
            std::fs::rename(&tmp, &gz)?;
        }
        Ok(())
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done?;
    std::fs::remove_file(log)?;
    Ok(())
}

/// One thing a pass did, or would do.
#[derive(Debug, Serialize)]
struct Action {
    job: String,
    kind: Kind,
    path: PathBuf,
    /// Disk the place held (a lower bound when `partial`).
    bytes: u64,
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    rescued: Option<String>,
    /// Why a place was left: its clone holds work that is not committed, or the rescue failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    kept: Option<String>,
}

impl Action {
    fn line(&self, dry: bool) -> String {
        let size = format!("{}{}", size_str(self.bytes), if self.partial { "+" } else { "" });
        let path = self.path.display();
        match (&self.kept, self.kind, dry) {
            (Some(why), ..) => format!("gc: kept {path} ({why})"),
            (None, Kind::ReviewRef, false) => format!("gc: removed {path}"),
            (None, Kind::ReviewRef, true) => format!("gc: would remove {path}"),
            (None, Kind::Log, false) => format!("gc: gzipped {path} ({size})"),
            (None, Kind::Log, true) => format!("gc: would gzip {path} ({size})"),
            (None, _, false) => format!("gc: removed {path} ({size})"),
            (None, _, true) => format!("gc: would remove {path} ({size})"),
        }
    }
}

/// A pass: `relay land gc`. With `dry_run` it changes nothing. It stops once
/// `gc.pass_seconds` have passed, and says what is left for the next pass.
pub fn run(ctx: &Ctx, dry_run: bool) -> Result<()> {
    // One pass at a time; the dispatcher may ask again while one is slow. A pass that did nothing
    // is not a success: the caller is told, and may try again once the other is done.
    let Some(_role) = os::Role::try_take(&ctx.state.join("gc.lock"))? else {
        fail!(Conflict, "gc: another pass is running; this one did nothing");
    };
    let end = Instant::now() + Duration::from_secs(ctx.cfg.gc.pass_seconds);
    let items = plan(ctx, &ctx.world()?, now_micros());
    let mut actions: Vec<Action> = Vec::new();
    let mut left = 0;
    let say = |line: &str| {
        if !ctx.json {
            println!("{line}");
        }
    };
    for (i, item) in items.iter().enumerate() {
        let Some(within) = end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
            left = items.len() - i;
            break;
        };
        let (bytes, complete) = if matches!(item.kind, Kind::Log | Kind::ReviewRef) {
            (item.path.metadata().map_or(0, |m| m.len()), true)
        } else {
            os::dir_size(&item.path, within)
        };
        let mut a = Action {
            job: item.job.clone(),
            kind: item.kind,
            path: item.path.clone(),
            bytes,
            partial: !complete,
            rescued: None,
            kept: None,
        };
        if !dry_run && let Err(e) = act(ctx, item, &mut a) {
            a.kept = Some(format!("{e:#}"));
        }
        if dry_run && item.kind == Kind::Clone {
            a.rescued = rescue_needed(ctx, item).map(|r| format!("{r} would be made"));
        }
        say(&a.line(dry_run));
        actions.push(a);
    }
    if left > 0 {
        say(&format!("gc: out of time ({}s); {left} place(s) left for the next pass", ctx.cfg.gc.pass_seconds));
    }
    let done: Vec<&Action> = actions.iter().filter(|a| a.kept.is_none()).collect();
    let freed: u64 = done.iter().filter(|a| !matches!(a.kind, Kind::Log | Kind::ReviewRef)).map(|a| a.bytes).sum();
    let verb = if dry_run { "would remove" } else { "removed" };
    let totals = format!(
        "gc: {verb} {} place(s), {} freed",
        done.iter().filter(|a| !matches!(a.kind, Kind::Log | Kind::ReviewRef)).count(),
        size_str(freed)
    );
    say(&totals);
    if !dry_run {
        let _ = std::fs::write(ctx.state.join("gc.last"), "");
    }
    ctx.reply(&serde_json::json!({ "dry_run": dry_run, "actions": actions, "freed_bytes": freed, "left": left }), "")
}

/// The rescue ref a dry run says a clone's removal would make, when its commits are not all in the repository.
fn rescue_needed(ctx: &Ctx, item: &Item) -> Option<String> {
    let world = ctx.world().ok()?;
    let j = world.jobs().get(&item.job)?;
    let tip = tip(j.workdir.as_ref()?, j.branch.as_deref()?).ok()?;
    (!in_repo(ctx, &tip)).then(|| format!("refs/relay/rescue/{}", j.name))
}

/// Does one item, under the jobs lock and after checking again that the job is
/// still one gc may touch (a resume or a dispatch may have come in since the plan).
fn act(ctx: &Ctx, item: &Item, a: &mut Action) -> Result<()> {
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    if !plan(ctx, &world, now_micros()).contains(item) {
        bail!("the job changed since the plan");
    }
    if item.kind == Kind::ReviewRef {
        let name = item.path.to_string_lossy();
        let out = Ctx::git(&ctx.cfg.repo).args(["update-ref", "-d", &name]).output()?;
        anyhow::ensure!(out.status.success(), "could not delete {name}");
        return Ok(());
    }
    if item.kind == Kind::Probe {
        return crate::hostdir::remove_tree(&item.path).with_context(|| format!("removing {}", item.path.display()));
    }
    if item.kind == Kind::CacheCopy {
        let (Some(pool), Some(name)) = (item.path.parent(), item.path.file_name()) else { bail!("not in a pool") };
        return crate::sandbox::farm::remove_idle(pool, Path::new(name))
            .with_context(|| format!("removing {}", item.path.display()));
    }
    if item.kind == Kind::Scratch {
        crate::hostdir::remove_tree(&item.path).with_context(|| format!("removing {}", item.path.display()))?;
        return collected(ctx, &item.job, "scratch", a.bytes);
    }
    let j = world.jobs().get(&item.job).context("the job is gone")?;
    match item.kind {
        Kind::ReviewRef | Kind::Scratch | Kind::CacheCopy | Kind::Probe => unreachable!("handled above"),
        Kind::Log => gzip_log(&item.path),
        Kind::Outputs => {
            let meta = item.path.symlink_metadata()?;
            if meta.is_dir() { crate::hostdir::remove_tree(&item.path) } else { std::fs::remove_file(&item.path) }
                .with_context(|| format!("removing {}", item.path.display()))?;
            collected(ctx, &item.job, "outputs", a.bytes)
        }
        Kind::Clone => {
            let clone = j.workdir.as_ref().filter(|d| d.path() == item.path).context("the job's clone moved")?;
            a.rescued = secure(ctx, j, clone)?;
            if let Some(r) = a.rescued.as_ref().filter(|_| !ctx.json) {
                println!("gc: rescued {}'s unfetched commits into {r}", item.job);
            }
            crate::hostdir::remove_tree(&item.path).with_context(|| format!("removing {}", item.path.display()))?;
            collected(ctx, &item.job, "clone", a.bytes)
        }
    }
}

fn collected(ctx: &Ctx, job: &str, what: &str, bytes: u64) -> Result<()> {
    let event = LandEvent::Collected { job: job.to_owned(), what: what.to_owned(), bytes };
    ctx.port(&super::Source(())).emit(event)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Status;

    const HOUR: u64 = 3_600_000_000;
    const DAY: u64 = 24 * HOUR;

    fn job(name: &str, status: Status, ended_at: Option<u64>) -> Job {
        Job { name: name.into(), status, ended_at, ..Job::default() }
    }

    fn world(jobs: Vec<Job>) -> World {
        let mut w = World::default();
        w.jobs_mut().extend(jobs.into_iter().map(|j| (j.name.clone(), j)));
        w
    }

    #[test]
    fn outputs_then_clones_fall_due_by_the_age_of_the_end() {
        let gc = Gc::default();
        let (done, failed) = (job("d", Status::Done, Some(1_000)), job("f", Status::Failed, Some(1_000)));
        let w = world(vec![done.clone(), failed.clone()]);
        let at = |t: u64| (due(&w, &done, &gc, t), due(&w, &failed, &gc, t));
        let nothing = Due::default();
        assert_eq!(at(1_000 + 6 * HOUR - 1), (nothing, nothing), "under six hours");
        let outputs = Due { outputs: true, clone: false };
        assert_eq!(at(1_000 + 6 * HOUR), (outputs, outputs), "six hours: the outputs");
        assert_eq!(at(1_000 + 7 * DAY - 1), (outputs, outputs));
        let all = Due { outputs: true, clone: true };
        assert_eq!(at(1_000 + 7 * DAY), (all, all), "seven days: the whole clone");
        let short = Gc { waiting_retain_days: 30.0, ..Gc::default() };
        assert!(
            !due(&w, &failed, &short, 1_000 + 8 * DAY).clone,
            "a failed job keeps its clone for waiting_retain_days"
        );
        assert!(due(&w, &done, &short, 1_000 + 8 * DAY).clone, "a done one does not wait for it");
    }

    #[test]
    fn nothing_is_due_for_a_live_job_a_waited_for_job_or_a_job_that_has_not_ended() {
        let gc = Gc { outputs_after_hours: 0.0, clones_after_days: 0.0, waiting_retain_days: 0.0, ..Gc::default() };
        let late = 10 * DAY;
        let mut waiter = job("w", Status::Queued, None);
        waiter.after = vec!["held".into()];
        let fork = crate::bus::Fork { job: "forked".into(), session_id: "s".into(), account: "claude:a".into() };
        let forker = Job { fork: Some(fork), ..job("k", Status::Queued, None) };
        let jobs = vec![
            job("running", Status::Running, Some(1)),
            job("checking", Status::Checking, Some(1)),
            job("requeued", Status::Queued, Some(1)),
            job("fresh", Status::Done, None),
            job("held", Status::Done, Some(1)),
            job("forked", Status::Done, Some(1)),
            job("free", Status::Done, Some(1)),
            Job { kind: crate::bus::Kind::Read, ..job("look", Status::Done, Some(1)) },
            waiter,
            forker,
        ];
        let w = world(jobs);
        let due_of = |n: &str| due(&w, &w.jobs()[n], &gc, late);
        for n in ["running", "checking", "requeued", "fresh", "held", "forked", "look"] {
            assert_eq!(due_of(n), Due::default(), "{n}");
        }
        assert_eq!(due_of("free"), Due { outputs: true, clone: true });
    }

    #[test]
    fn a_job_the_disk_floor_holds_keeps_its_clone_and_scratch_for_its_resume() {
        let gc = Gc { outputs_after_hours: 0.0, clones_after_days: 0.0, waiting_retain_days: 0.0, ..Gc::default() };
        let held = Job { held: true, ..job("held", Status::Stopped, Some(1)) };
        let stopped = job("stopped", Status::Stopped, Some(1));
        let w = world(vec![held.clone(), stopped.clone()]);
        assert_eq!(due(&w, &held, &gc, 10 * DAY), Due::default());
        assert!(!scratch_due(&w, &held, &gc, 10 * DAY));
        assert_eq!(due(&w, &stopped, &gc, 10 * DAY), Due { outputs: true, clone: true }, "a person's stop is not kept");
    }

    #[test]
    fn a_scratch_directory_is_due_by_the_age_of_the_end_even_for_a_read_only_job() {
        let gc = Gc::default();
        let look = Job { kind: crate::bus::Kind::Read, ..job("look", Status::Done, Some(1_000)) };
        let w = world(vec![look.clone(), job("run", Status::Running, Some(1_000)), job("fresh", Status::Done, None)]);
        assert!(!scratch_due(&w, &look, &gc, 1_000 + 6 * HOUR - 1), "under six hours");
        assert!(scratch_due(&w, &look, &gc, 1_000 + 6 * HOUR), "a read-only job has no clone, but has a /tmp");
        assert!(!scratch_due(&w, &w.jobs()["run"], &gc, 10 * DAY), "a live job keeps it");
        assert!(!scratch_due(&w, &w.jobs()["fresh"], &gc, 10 * DAY), "so does one that has not ended");
    }

    #[test]
    fn a_path_through_a_symlink_or_out_of_the_clone_is_never_named() {
        let d = crate::config::tempdir::Dir::new();
        let clone = d.path().join("clone");
        std::fs::create_dir_all(clone.join("target")).unwrap();
        std::fs::create_dir_all(d.path().join("elsewhere/inner")).unwrap();
        std::os::unix::fs::symlink(d.path().join("elsewhere"), clone.join("node_modules")).unwrap();
        assert_eq!(inside(&clone, "target"), Some(clone.join("target")));
        assert_eq!(inside(&clone, "node_modules"), None, "a link is skipped whole");
        assert_eq!(inside(&clone, "node_modules/inner"), None, "and what is behind one");
        assert_eq!(inside(&clone, ".venv"), None, "absent");
        assert_eq!(inside(&clone, "../elsewhere"), None);
        assert_eq!(inside(&clone, "/etc"), None);
        assert_eq!(inside(&clone, ""), None);
    }

    /// An idle copy of the linked caches goes once unused for `cache_copies_idle_days`, or when more are
    /// idle in its pool than `limits.max_running`; the ones used last stay.
    /// Copilot on (e): with the data and the state directories one (`XDG_DATA_HOME` = `XDG_STATE_HOME`), the
    /// sessions' pool and the checks' pool were one directory. Each pool is named for its purpose.
    #[test]
    fn the_session_and_check_pools_never_coincide() {
        let d = crate::config::tempdir::Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = crate::config::Config::parse("name = \"t\"\n", &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("one"), d.path().join("one"));
        let [sessions, checks] = pools(&ctx);
        assert_ne!(sessions, checks);
    }

    /// Copilot on (e): a probe that was killed left its clone, with a warmed copy of the linked caches in
    /// its `.git`, where gc never looked. A probe's clone names its process and when it started, and gc
    /// removes it once that process is gone.
    #[test]
    fn a_dead_probes_clone_is_collected_and_a_live_ones_is_not() {
        let d = crate::config::tempdir::Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = crate::config::Config::parse("name = \"t\"\n", &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        let dead = ctx.clones.join("probe-999999999-1");
        std::fs::create_dir_all(dead.join(".git/relay-caches/.cache")).unwrap();
        let me = std::process::id();
        let live = ctx.clones.join(format!("probe-{me}-{}", crate::os::start_time(me).unwrap()));
        std::fs::create_dir_all(&live).unwrap();
        let named: Vec<PathBuf> = plan(&ctx, &World::default(), 0).into_iter().map(|i| i.path).collect();
        assert!(named.contains(&dead), "{named:?}");
        assert!(!named.contains(&live), "{named:?}");
    }

    #[test]
    fn idle_cache_copies_go_by_age_and_past_the_most_runs_at_once() {
        let d = crate::config::tempdir::Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = crate::config::Config::parse("name = \"t\"\n[limits]\nmax_running = 2\n", &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        let now = 100 * DAY;
        let copy = |pool: &Path, hours: u64| {
            let at = (now - hours * HOUR) / 1_000_000;
            let id = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, at, 0));
            let p = pool.join(format!("idle-{id}"));
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let [sessions, checks] = pools(&ctx);
        let (h0, h1, h2, h30) = (copy(&sessions, 0), copy(&sessions, 1), copy(&sessions, 2), copy(&sessions, 30));
        let fresh = copy(&checks, 3);
        // One whose removal began and did not finish is named again, whatever its age.
        let gone = checks.join("gone-left");
        std::fs::create_dir_all(&gone).unwrap();
        let named: Vec<PathBuf> = cache_copies(&ctx, &ctx.cfg.gc, now).into_iter().map(|i| i.path).collect();
        assert!(named.contains(&h2) && named.contains(&h30) && named.contains(&gone), "{named:?}");
        assert!(!named.contains(&h0) && !named.contains(&h1) && !named.contains(&fresh), "{named:?}");
    }
}
