//! dispatch: gives queued jobs a seat on the account with the most room, and
//! reaps sessions whose runner died. Emits: dispatched, reaped.
//!
//! It is the only system that acts on its own. `serve` takes the dispatcher
//! role with flock (a second one exits at once), wakes on the bus doorbell or
//! at the next moment the plan could change on its own (a limit lifting, a
//! meter resetting, a reading going stale: `next_due`), re-reads every 5
//! minutes as the backstop for runners that die unannounced, and exits after
//! a minute with nothing queued or running.
//! Every system that emits wakes it, so it never has to be started by hand.

mod room;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{STALE_MICROS, harness_of};
use crate::bus::{DispatchEvent, now_micros};
use crate::config::{Config, Limits, Profile};
use crate::ctx::Ctx;
use crate::harness::{Harness, Home};
use crate::os::{self, Role};
use crate::world::{AccountState, DAY_MICROS, Job, Status, Wait, World};
pub use room::{Hold, Room, Start};

/// The longest sleep. Only a runner that dies without a word (one started by
/// an earlier dispatcher, or killed outright) waits this long to be reaped.
const BACKSTOP: Duration = Duration::from_mins(5);
/// The least time between gc passes the disk gauge starts.
const LOW_DISK_GAP: Duration = Duration::from_mins(5);
/// How long the dispatcher stays with nothing queued or running.
const IDLE_EXIT: Duration = Duration::from_secs(60);

/// The capability to write dispatch's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> std::path::PathBuf {
        crate::bus::system_file("dispatch")
    }
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Cmd {
    /// Start the dispatcher if it is not running. Systems do this themselves.
    Wake,
    /// The dispatcher itself.
    Serve,
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Wake => ctx.wake_dispatch(),
        Cmd::Serve => serve(ctx),
    }
}

/// Why nothing more starts this pass, for the log (printed only when it
/// changes); when that could change; and the jobs a limit holds.
#[derive(Debug, Default, PartialEq)]
struct Held(Vec<String>, Option<u64>, Vec<String>);

fn serve(ctx: &Ctx) -> Result<()> {
    let Some(role) = Role::try_take(&ctx.state.join("dispatch.lock"))? else { return Ok(()) };
    // Every project's bus: a seat freed or a limit hit in another project changes our plan too.
    let bell = ctx.doorbell()?;
    let version = os::binary_version();
    let mut sources = Sources::watch(&ctx.cfg_path);
    let mut idle_since: Option<Instant> = None;
    let mut last_held = Held::default();
    loop {
        if os::binary_version() != version {
            // A rebuilt binary takes over: drop the role, then start the new one.
            drop(role);
            return ctx.wake_dispatch();
        }
        if sources.changed() {
            // The policy or the project config was edited: a new dispatcher reads it.
            // One that does not load yet (a half-saved file) changes nothing; it is
            // looked at again when the file changes again.
            match Config::load(Some(&ctx.cfg_path)) {
                Ok((cfg, _)) => {
                    drop(role);
                    return ctx.wake_dispatch_under(&cfg);
                }
                Err(e) => {
                    eprintln!("{} config changed but does not load; keeping the old: {e:#}", crate::os::stamp());
                }
            }
        }
        let (busy, held, due_us) = tick(ctx)?;
        let gc_in = gc_pass(ctx);
        if held != last_held {
            for h in &held.0 {
                eprintln!("{} held: {h}", crate::os::stamp());
            }
            last_held = held;
        }
        idle_since = if busy { None } else { Some(idle_since.unwrap_or_else(Instant::now)) };
        if idle_since.is_some_and(|t| t.elapsed() > IDLE_EXIT) {
            return Ok(());
        }
        let mut sleep = BACKSTOP.min(gc_in);
        if let Some(due) = due_us {
            sleep = sleep.min(Duration::from_micros(due.saturating_sub(now_micros())));
        }
        if let Some(t) = idle_since {
            // Wake just after the idle minute to exit.
            sleep = sleep.min(IDLE_EXIT.saturating_sub(t.elapsed()) + Duration::from_millis(100));
        }
        if bell.wait(sleep) {
            // Let a burst of events land, then fold them in one pass.
            std::thread::sleep(Duration::from_millis(200));
            bell.wait(Duration::ZERO);
        }
    }
}

/// The files the merged config is read from (the project's `relay.toml` and the
/// owner's policy, even one that does not exist yet), and their state when last
/// looked at: a change means the dispatcher's config is out of date.
#[derive(Debug)]
struct Sources {
    files: Vec<std::path::PathBuf>,
    seen: Vec<Option<(std::time::SystemTime, u64)>>,
}

impl Sources {
    fn watch(cfg_path: &Path) -> Self {
        let mut files = vec![cfg_path.to_path_buf()];
        files.extend(crate::config::Policy::path().map(|(p, _)| p));
        Self::of(files)
    }

    fn of(files: Vec<std::path::PathBuf>) -> Self {
        let mut s = Self { files, seen: Vec::new() };
        s.seen = s.stamp();
        s
    }

    fn stamp(&self) -> Vec<Option<(std::time::SystemTime, u64)>> {
        let state = |p: &Path| std::fs::metadata(p).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        self.files.iter().map(|p| state(p)).collect()
    }

    /// Whether any source differs from when last asked (and remembers it).
    fn changed(&mut self) -> bool {
        let now = self.stamp();
        std::mem::replace(&mut self.seen, now.clone()) != now
    }
}

/// Starts a gc pass (`relay land gc`, in a process of its own) when one is due:
/// `gc.interval_minutes` after the last, or at once when free disk is under
/// `limits.min_free_disk_gb` (but not twice within `LOW_DISK_GAP`, so a disk
/// that gc cannot help does not start one every pass). Returns how long until
/// the interval brings the next one.
fn gc_pass(ctx: &Ctx) -> Duration {
    let interval = Duration::from_secs_f64(ctx.cfg.gc.interval_minutes * 60.0);
    let stamp = ctx.state.join("gc.last");
    let since = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    let min = ctx.cfg.limits.min_free_disk_gb;
    let low = min > 0.0 && os::free_disk_gb(&ctx.clones).is_some_and(|gb| gb < min);
    if since.is_none_or(|s| s >= interval || low && s >= LOW_DISK_GAP) {
        // Stamped now as well as when the pass ends, so a slow one is not started again.
        let _ = std::fs::write(&stamp, "");
        if let Err(e) = ctx.spawn_self(&["land", "gc"], &ctx.state.join("gc.log")) {
            eprintln!("{} gc did not start: {e:#}", crate::os::stamp());
        }
        return interval;
    }
    interval.saturating_sub(since.unwrap_or_default())
}

/// One pass over the world. Returns whether anything is still queued or
/// running, why jobs are held, and when (bus time) the plan could next change.
fn tick(ctx: &Ctx) -> Result<(bool, Held, Option<u64>)> {
    let _lock = ctx.jobs_lock()?;
    // Seats are the machine's: no other project's dispatcher plans while this one does.
    let _seats = Role::take(&ctx.state_root.join("seats.lock"))?;
    let port = ctx.port(&Source(()));
    let mut world = ctx.world()?;
    for j in world.jobs_mut().values_mut().filter(|j| j.status.has_runner()) {
        if !j.runner.is_some_and(|(pid, started)| os::same_process(pid, started)) {
            // Nothing is signalled: its sessions and handlers ended with it, each by its supervisor.
            port.emit(DispatchEvent::Reaped { job: j.name.clone() })?;
            j.status = Status::Failed;
        }
    }
    // Accounts belong to the machine: what other projects learned of them, and
    // the seats their sessions hold, count here too. Their jobs stay theirs.
    let neighbours = ctx.neighbours();
    let others: Vec<&World> = neighbours.iter().collect();
    let mut room = Room::new(&ctx.cfg, &world, &others, disk_ok(&ctx.cfg.limits, &ctx.clones));
    crate::systems::quota::start_refresh(ctx, &room, &world, true)?;
    let mut held = Held::default();
    for Start { job, account, model, provider } in plan(&ctx.cfg, &mut room, &world, now_micros(), &mut held) {
        let (pid, gate) = ctx.spawn_gated(&["session", "run", &job], &ctx.job_file(&job, "runner.log"))?;
        let started = os::start_time(pid).unwrap_or(0);
        // The runner reads its account and model from this event, so it waits at the gate until it is written.
        port.emit(DispatchEvent::Dispatched {
            job,
            account,
            model,
            provider,
            runner_pid: pid,
            runner_started: started,
        })?;
        drop(gate);
    }
    let held_due = ask_held(ctx, &world, &held)?;
    // A job blocked on a dependency that ended badly waits for the operator, not for the dispatcher;
    // one held by a runner (checking too) is watched until it ends or is reaped.
    let busy = world.jobs().values().any(|j| world.moves_on_its_own(j));
    let waiting = world.jobs().values().any(|j| world.startable(j));
    let states: Vec<_> = room.found().iter().map(|a| (harness_of(a), room.account(&a.key()))).collect();
    let due = next_due(&states, waiting, now_micros());
    Ok((busy, Held(held.0, None, Vec::new()), [due, held.1, held_due].into_iter().flatten().min()))
}

/// Starts the `held` chain for each job in `held` that has waited past
/// `hooks.held_after_minutes` and may be asked about again (`next_held_ask`);
/// returns when (bus time) the next one will have waited that long.
fn ask_held(ctx: &Ctx, world: &World, held: &Held) -> Result<Option<u64>> {
    if ctx.cfg.hooks.held.is_none() {
        return Ok(None);
    }
    let after = Duration::from_secs_f64(ctx.cfg.hooks.held_after_minutes * 60.0);
    let after = u64::try_from(after.as_micros()).unwrap_or(u64::MAX);
    let now = now_micros();
    let mut next = None;
    for j in held.2.iter().filter_map(|n| world.jobs().get(n)) {
        let at = crate::systems::decide::next_held_ask(world, j, after);
        if at == u64::MAX {
            continue;
        }
        if at <= now {
            let prefix = format!("{}: ", j.name);
            let why = held.0.iter().find_map(|l| l.strip_prefix(&prefix)).unwrap_or("a limit holds it");
            ctx.spawn_self(&["decide", "held", &j.name, "--why", why], &ctx.job_file(&j.name, "decide.log"))?;
        } else {
            next = [next, Some(at)].into_iter().flatten().min();
        }
    }
    Ok(next)
}

/// The next moment (bus time, after `now_us`) at which the plan could change
/// with no event to announce it: a limit lifts, a meter resets, or, while a
/// job waits, a reading goes stale or a failed read stops backing off (so
/// `quota::start_refresh` starts a new one). `None` if nothing is pending.
fn next_due(accounts: &[(&dyn Harness, Option<&AccountState>)], waiting: bool, now_us: u64) -> Option<u64> {
    let from_secs = |s: i64| u64::try_from(s).ok().map(|s| s.saturating_mul(1_000_000));
    let mut times = Vec::new();
    for &(h, state) in accounts {
        let Some(a) = state else { continue };
        // `fit` compares whole seconds: at `limited_until` and at a reset the account is free again.
        times.extend(a.limited_until.and_then(from_secs));
        times.extend(a.meters.values().filter_map(|m| m.resets_at).filter_map(from_secs));
        if waiting && h.caps().quota {
            // `is_stale` and `due` compare with `>`, so one microsecond past.
            times.extend(a.read_at.map(|t| t.saturating_add(STALE_MICROS + 1)));
            times.extend(a.unreadable_at.map(|t| t.saturating_add(STALE_MICROS + 1)));
        }
    }
    times.into_iter().filter(|&t| t > now_us).min()
}

/// Whether free disk under the clones (`clones`) lets a build job start: unmeasured is not enough.
pub fn disk_ok(limits: &Limits, clones: &Path) -> bool {
    limits.min_free_disk_gb <= 0.0 || os::free_disk_gb(clones).is_some_and(|gb| gb >= limits.min_free_disk_gb)
}

/// Why a job of profile `name` may not start at `now_us`, with when (bus time) that
/// could change: `running` of the profile's jobs have a runner, and `starts` are
/// the times its sessions started. `None` when neither `max_running` nor
/// `max_per_day` holds it.
pub fn profile_hold(
    name: &str,
    p: &Profile,
    running: usize,
    starts: &[u64],
    now_us: u64,
) -> Option<(String, Option<u64>)> {
    if let Some(max) = p.max_running.filter(|&max| running >= max) {
        return Some((format!("profile {name}: {running} running (max {max})"), None));
    }
    let max = p.max_per_day?;
    let mut recent: Vec<u64> = starts.iter().copied().filter(|&s| s + DAY_MICROS > now_us).collect();
    recent.sort_unstable();
    let count = recent.len();
    if count < max {
        return None;
    }
    let slot = recent[count - max] + DAY_MICROS;
    let at = i64::try_from(slot)
        .ok()
        .and_then(|us| jiff::Timestamp::from_microsecond(us).ok())
        .map_or_else(String::new, |t| format!(", next slot {}", crate::os::local(t).strftime("%H:%M %Z")));
    Some((format!("profile {name}: {count} started in 24h (max {max}){at}"), Some(slot)))
}

/// Which queued jobs start now, where, and on what model. Pure apart from reading the load
/// average. Each job is judged by `room.judge` (the same judgment the board's reasons show), and
/// each start is booked in `room` before the next job is judged. The seats other projects hold
/// count against `limits.per_account`, but not against this project's `max_running`. `now_us`
/// is bus time, so a profile's daily window can be judged at any moment.
fn plan(cfg: &Config, room: &mut Room, world: &World, now_us: u64, held: &mut Held) -> Vec<Start> {
    let limits = &cfg.limits;
    let mut total = world.jobs().values().filter(|j| j.status == Status::Running).count();
    if let (Some(max), Some(now)) = (limits.max_load, os::load_average())
        && now > max
    {
        held.0.push(format!("load {now:.1} is above {max}"));
        return Vec::new();
    }
    let mut queued: Vec<&Job> = world.jobs().values().filter(|j| j.status == Status::Queued).collect();
    queued.sort_by_key(|j| j.created_at);
    let mut out = Vec::new();
    for j in queued {
        if total >= limits.max_running {
            break;
        }
        let waits = world.waits(j);
        if !waits.is_empty() {
            if world.blocked(j) {
                held.0.push(format!("{}: {}", j.name, Wait::summary(&waits).unwrap_or_default()));
            }
            continue;
        }
        let home = match room.judge(cfg, j, now_us) {
            Ok(fits) => choose(j, &fits).cloned(),
            Err(Hold { why, until }) => {
                held.0.push(format!("{}: {why}", j.name));
                held.1 = [held.1, until].into_iter().flatten().min();
                held.2.push(j.name.clone());
                continue;
            }
        };
        if let Some(home) = home {
            total += 1;
            out.push(room.take(cfg, j, &home, now_us));
        }
    }
    out
}

/// Whether `job` may run on `account`: the job allows its harness, and any pin names it.
pub fn may_use(job: &Job, account: &Home) -> bool {
    (job.harnesses.is_empty() || job.harnesses.iter().any(|h| h == account.harness))
        && job.pinned.as_ref().is_none_or(|p| *p == account.key())
}

/// The model `job` asks for on `harness`: its own choice there, else the harness's configured one.
pub fn model_for(cfg: &Config, job: &Job, harness: &str) -> Option<String> {
    job.models.get(harness).cloned().or_else(|| cfg.harness(harness).model)
}

/// Of the accounts that fit, the one a resumable session already ran on, so
/// the provider's prompt cache still holds its transcript (moving it means
/// re-reading all of it at full price); otherwise the best-scoring one.
/// A fork that has no session of its own yet prefers the account of the session it forks.
fn choose<'a>(job: &Job, fits: &[(&'a Home, f64)]) -> Option<&'a Home> {
    let fork = job.fork.as_ref().filter(|f| job.session_id.as_deref().is_none_or(|s| s == f.session_id));
    let own = job.session_id.as_ref().and(job.transcript_account.as_deref());
    let home = fork.map(|f| f.account.as_str()).or(own).and_then(|last| fits.iter().find(|(a, _)| a.key() == last));
    home.or_else(|| fits.iter().max_by(|(_, x), (_, y)| x.total_cmp(y))).map(|&(a, _)| a)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::accounts;
    use crate::harness::Meter;

    fn home(harness: &'static str, label: &str) -> Home {
        Home { harness, label: label.into(), dir: PathBuf::from("/x") }
    }

    fn job(session: Option<&str>, transcript: Option<&str>) -> Job {
        Job {
            name: "j".into(),
            session_id: session.map(Into::into),
            transcript_account: transcript.map(Into::into),
            ..Job::default()
        }
    }

    fn profile(max_running: Option<usize>, max_per_day: Option<usize>) -> Profile {
        Profile { max_running, max_per_day, ..Profile::default() }
    }

    #[test]
    fn max_running_holds_the_next_job_with_a_reason() {
        let p = profile(Some(1), None);
        assert_eq!(profile_hold("bug-hunt", &p, 0, &[], 5), None);
        let (why, until) = profile_hold("bug-hunt", &p, 1, &[], 5).unwrap();
        assert_eq!((why.as_str(), until), ("profile bug-hunt: 1 running (max 1)", None));
        assert_eq!(profile_hold("other", &profile(None, None), 9, &[1, 2, 3], 5), None, "no limit, no hold");
    }

    #[test]
    fn max_per_day_holds_the_fourth_start_until_the_window_passes() {
        let p = profile(None, Some(3));
        // 2026-11-02 12:00:00Z, and starts at 13:00, 14:02 and 15:30 the day before.
        let noon = 1_793_620_800 * 1_000_000_u64;
        let at = |h: u64, m: u64| noon - DAY_MICROS + (h * 60 + m) * 60 * 1_000_000 - 12 * 3600 * 1_000_000;
        let starts = [at(14, 2), at(13, 0), at(15, 30)];
        let (why, until) = profile_hold("bug-hunt", &p, 0, &starts, noon).unwrap();
        let slot = jiff::Timestamp::from_microsecond(i64::try_from(at(13, 0) + DAY_MICROS).unwrap()).unwrap();
        let local = crate::os::local(slot).strftime("%H:%M %Z");
        assert_eq!(why, format!("profile bug-hunt: 3 started in 24h (max 3), next slot {local}"), "local time");
        assert_eq!(until, Some(at(13, 0) + DAY_MICROS));
        let later = at(13, 0) + DAY_MICROS;
        assert!(profile_hold("bug-hunt", &p, 0, &starts, later - 1).is_some(), "the window is still full");
        assert_eq!(profile_hold("bug-hunt", &p, 0, &starts, later), None, "the oldest start has left the window");
        let (why, _) = profile_hold("bug-hunt", &p, 0, &starts, later - 1).unwrap();
        assert!(why.contains("3 started in 24h"), "{why}");
        let next = at(14, 2) + DAY_MICROS;
        assert_eq!(
            profile_hold("bug-hunt", &p, 0, &[at(14, 2), at(13, 0), at(15, 30), later], next - 1).map(|h| h.1),
            Some(Some(next))
        );
    }

    #[test]
    fn a_running_limit_is_named_before_a_daily_one() {
        let p = profile(Some(1), Some(1));
        let (why, _) = profile_hold("p", &p, 1, &[10], 20).unwrap();
        assert!(why.contains("running"), "{why}");
    }

    #[test]
    fn a_session_stays_on_its_account_while_it_fits() {
        let (a, b) = (home("claude", "a"), home("claude", "b"));
        let fits = [(&a, 0.1), (&b, 0.9)];
        let key = |h: Option<&Home>| h.map(Home::key);
        assert_eq!(key(choose(&job(None, None), &fits)), Some("claude:b".into()), "best score");
        assert_eq!(key(choose(&job(Some("s"), Some("claude:a")), &fits)), Some("claude:a".into()), "its own");
        assert_eq!(key(choose(&job(None, Some("claude:a")), &fits)), Some("claude:b".into()), "no session to keep");
        let only_b = [(&b, 0.9)];
        assert_eq!(key(choose(&job(Some("s"), Some("claude:a")), &only_b)), Some("claude:b".into()), "a does not fit");
        assert_eq!(choose(&job(Some("s"), Some("claude:a")), &[]).map(Home::key), None);
    }

    #[test]
    fn only_live_runners_hold_seats_in_another_project() {
        let me = std::process::id();
        let started = os::start_time(me).unwrap();
        let running = |name: &str, account: &str, runner| Job {
            name: name.into(),
            status: Status::Running,
            account: Some(account.into()),
            runner,
            ..Job::default()
        };
        let mut one = World::default();
        for j in [
            running("a", "claude:x", Some((me, started))),
            running("b", "claude:x", Some((me, started + 1))),
            running("c", "claude:y", None),
            Job { status: Status::Done, ..running("d", "claude:y", Some((me, started))) },
        ] {
            one.jobs_mut().insert(j.name.clone(), j);
        }
        let mut two = World::default();
        two.jobs_mut().insert("a".into(), running("a", "claude:x", Some((me, started))));
        let found = vec![home("claude", "x"), home("claude", "y")];
        let room = Room::of(found, BTreeMap::new(), &World::default(), &[&one, &two], true);
        let seats = (room.running_on("claude:x"), room.running_on("claude:y"));
        assert_eq!(seats, (2, 0), "a dead runner or a finished job holds none");
    }

    #[test]
    fn next_due_is_the_earliest_pending_change() {
        let claude = crate::harness::by_id("claude").unwrap();
        let now_us = 1_000 * 1_000_000;
        assert_eq!(next_due(&[], true, now_us), None);
        assert_eq!(next_due(&[(claude, None)], true, now_us), None, "unread is refreshed now and announced");

        let mut a = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        let stale_at = now_us - 1 + STALE_MICROS + 1;
        assert_eq!(next_due(&[(claude, Some(&a))], true, now_us), Some(stale_at));
        assert_eq!(next_due(&[(claude, Some(&a))], false, now_us), None, "staleness matters only to a waiting job");

        a.meters.insert("five_hour".into(), Meter { utilization: 1.0, resets_at: Some(1_100) });
        a.meters.insert("old".into(), Meter { utilization: 1.0, resets_at: Some(900) });
        assert_eq!(next_due(&[(claude, Some(&a))], false, now_us), Some(1_100 * 1_000_000), "a future reset");

        let b = AccountState { limited_until: Some(1_050), unreadable_at: Some(now_us - 5), ..Default::default() };
        let both = [(claude, Some(&a)), (claude, Some(&b))];
        assert_eq!(next_due(&both, false, now_us), Some(1_050 * 1_000_000), "a limit lifting");
        let lifted = AccountState { limited_until: Some(999), ..b.clone() };
        assert_eq!(
            next_due(&[(claude, Some(&lifted))], true, now_us),
            Some(now_us - 5 + STALE_MICROS + 1),
            "a failed read's backoff ending; a past limit is ignored"
        );
        assert!(accounts::due(claude, Some(&lifted), now_us - 5 + STALE_MICROS + 1));
        assert!(!accounts::due(claude, Some(&lifted), now_us - 5 + STALE_MICROS));
        let fresh = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        assert!(accounts::is_stale(claude, Some(&fresh), stale_at));
        assert!(!accounts::is_stale(claude, Some(&fresh), stale_at - 1), "the moment matches `is_stale`");
    }

    const NOW_US: u64 = 1_800_000_000 * 1_000_000;

    fn omp_config(extra: &str) -> (crate::config::tempdir::Dir, Config) {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = format!("name = \"demo\"\n[limits]\nmin_free_disk_gb = 0\nper_account = 1\n{extra}");
        let cfg = Config::parse(&text, d.path()).unwrap();
        (d, cfg)
    }

    /// One omp account with both providers read just now at `codex` and `minimax` utilization.
    fn omp_world(jobs: &[(&str, Status, &str)], codex: f64, minimax: f64) -> (World, Vec<Home>) {
        let reset = i64::try_from(NOW_US / 1_000_000).unwrap() + 3600;
        let meter = |utilization| Meter { utilization, resets_at: Some(reset) };
        let mut world = World::default();
        world.accounts.insert(
            "omp:default".into(),
            AccountState {
                read_at: Some(NOW_US - 1),
                meters: [
                    ("openai-codex/5 hours".to_owned(), meter(codex)),
                    ("minimax-code/5 hours".to_owned(), meter(minimax)),
                ]
                .into(),
                ..Default::default()
            },
        );
        for (i, (name, status, model)) in jobs.iter().enumerate() {
            let j = Job {
                name: (*name).into(),
                status: *status,
                account: (*status == Status::Running).then(|| "omp:default".to_owned()),
                models: BTreeMap::from([("omp".to_owned(), (*model).to_owned())]),
                created_at: i as u64,
                ..Job::default()
            };
            world.jobs_mut().insert(j.name.clone(), j);
        }
        (world, vec![home("omp", "default")])
    }

    /// The plan for `world` among `others`, and why the rest are held.
    fn plan_among(cfg: &Config, world: &World, others: &[&World], found: &[Home]) -> (Vec<(String, String)>, Held) {
        let mut held = Held::default();
        let mut room = Room::of(found.to_vec(), world.accounts.clone(), world, others, true);
        let out = plan(cfg, &mut room, world, NOW_US, &mut held);
        (out.into_iter().map(|s| (s.job, s.account)).collect(), held)
    }

    fn plan_omp(cfg: &Config, world: &World, found: &[Home]) -> (Vec<(String, String)>, Held) {
        plan_among(cfg, world, &[], found)
    }

    /// Another project's job, running on `omp:default` with a live runner (this process), as its
    /// own dispatch recorded it: `model` and its provider.
    fn theirs(name: &str, model: Option<&str>) -> World {
        let me = std::process::id();
        let mut w = World::default();
        let j = Job {
            name: name.into(),
            status: Status::Running,
            account: Some("omp:default".into()),
            model: model.map(Into::into),
            provider: crate::accounts::provider_of(model, "omp").to_owned(),
            runner: Some((me, os::start_time(me).unwrap())),
            ..Job::default()
        };
        w.jobs_mut().insert(j.name.clone(), j);
        w
    }

    #[test]
    fn an_omp_account_has_a_seat_per_provider() {
        let (_d, cfg) = omp_config("");
        let started = [("review", Status::Running, "minimax-code/M3"), ("sol", Status::Queued, "openai-codex/gpt-6.1")];
        let (world, found) = omp_world(&started, 0.1, 0.1);
        let (out, held) = plan_omp(&cfg, &world, &found);
        assert_eq!(out, [("sol".to_owned(), "omp:default".to_owned())], "{:?}", held.0);

        let same = [("a", Status::Running, "openai-codex/gpt-6.1"), ("b", Status::Queued, "openai-codex/gpt-6.2")];
        let (world, found) = omp_world(&same, 0.1, 0.1);
        let (out, held) = plan_omp(&cfg, &world, &found);
        assert_eq!(out.len(), 0, "one seat per provider");
        assert_eq!(held.0, ["b: no account has room (omp:default busy (1 of 1 seat on openai-codex))"]);
    }

    #[test]
    fn a_seat_held_by_another_project_counts_against_its_provider() {
        let (_d, cfg) = omp_config("");
        let (world, found) = omp_world(&[("sol", Status::Queued, "openai-codex/gpt-6.1")], 0.1, 0.1);
        let (out, _) = plan_among(&cfg, &world, &[&theirs("x", Some("minimax-code/M3"))], &found);
        assert_eq!(out.len(), 1, "another provider's seat is no obstacle");
        let (out, _) = plan_among(&cfg, &world, &[&theirs("x", Some("openai-codex/gpt-6.2"))], &found);
        assert_eq!(out.len(), 0);
    }

    /// Audit finding 10: another project's job that names no model runs on its own config's model,
    /// which this project cannot know. Its seat is the provider its dispatch recorded, not this
    /// project's default, so this project does not start a second session on that provider.
    #[test]
    fn another_projects_seat_is_the_provider_its_dispatch_recorded_not_our_default() {
        let (_d, cfg) = omp_config("[harness.omp]\nenabled = true\nmodel = \"minimax-code/M3\"\n");
        let (world, found) = omp_world(&[("sol", Status::Queued, "openai-codex/gpt-6.1")], 0.1, 0.1);
        // Their job names no model of its own; their config ran it on openai-codex.
        let mut other = theirs("x", Some("openai-codex/gpt-6.2"));
        other.jobs_mut().get_mut("x").unwrap().models.clear();
        let (out, held) = plan_among(&cfg, &world, &[&other], &found);
        assert_eq!(out, [], "per_account = 1 on openai-codex is taken");
        assert_eq!(held.0, ["sol: no account has room (omp:default busy (1 of 1 seat on openai-codex))"]);
        // And a job of ours on our default provider still has its own seat there.
        let (world, found) = omp_world(&[("mm", Status::Queued, "minimax-code/M3")], 0.1, 0.1);
        let (out, _) = plan_among(&cfg, &world, &[&other], &found);
        assert_eq!(out.len(), 1, "their seat is not booked under our default's provider");
    }

    /// The dispatch records the model and provider it resolved, for the session, the seats and the boards.
    #[test]
    fn a_start_records_the_model_and_provider_it_was_judged_on() {
        let (_d, cfg) = omp_config("[harness.omp]\nenabled = true\nmodel = \"minimax-code/M3\"\n");
        let (mut world, found) = omp_world(&[("a", Status::Queued, "x")], 0.1, 0.1);
        world.jobs_mut().get_mut("a").unwrap().models.clear();
        let mut room = Room::of(found, world.accounts.clone(), &world, &[], true);
        let starts = plan(&cfg, &mut room, &world, NOW_US, &mut Held::default());
        let want = Start {
            job: "a".into(),
            account: "omp:default".into(),
            model: Some("minimax-code/M3".into()),
            provider: "minimax-code".into(),
        };
        assert_eq!(starts, [want]);
    }

    #[test]
    fn a_held_reason_names_a_busy_account_and_every_other_cause() {
        let (_d, cfg) = omp_config("");
        let jobs = [("a", Status::Running, "openai-codex/gpt-6.1"), ("b", Status::Queued, "openai-codex/gpt-6.1")];
        let (world, found) = omp_world(&jobs, 1.0, 0.1);
        let (out, held) = plan_omp(&cfg, &world, &found);
        assert_eq!(out.len(), 0);
        let [line] = held.0.as_slice() else { panic!("{:?}", held.0) };
        let why = line.strip_prefix("b: no account has room (").and_then(|l| l.strip_suffix(')')).unwrap();
        assert!(why.starts_with("omp:default busy (1 of 1 seat on openai-codex), at floor: "), "{why}");
        assert!(why.contains("openai-codex/5 hours"), "{why}");
    }

    #[test]
    fn a_changed_config_source_is_noticed_once() {
        let d = crate::config::tempdir::Dir::new();
        let (project, policy) = (d.path().join("relay.toml"), d.path().join("policy.toml"));
        std::fs::write(&project, "name = \"demo\"\n").unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let policy_path = policy.clone();
        let mut sources = Sources::of(vec![project.clone(), policy.clone()]);
        assert!(!sources.changed(), "nothing moved; a policy file that is not there yet is fine");

        std::fs::write(&policy, "[profiles.bug-hunt]\nmax_per_day = 3\n").unwrap();
        assert!(sources.changed(), "a policy file appeared");
        assert!(!sources.changed(), "seen once");

        let first = std::fs::metadata(&policy).unwrap().modified().unwrap();
        std::fs::write(&policy, "[profiles.bug-hunt]\nmax_per_day = 10\n").unwrap();
        let file = std::fs::File::options().write(true).open(&policy).unwrap();
        file.set_modified(first + Duration::from_secs(5)).unwrap();
        assert!(sources.changed(), "the policy was edited");
        let text = std::fs::read_to_string(&policy).unwrap();
        let policy = crate::config::Policy::parse(&text).unwrap();
        let project_text = std::fs::read_to_string(&project).unwrap();
        let cfg = Config::parse_with(&project_text, d.path(), Some((&policy, &policy_path))).unwrap();
        assert_eq!(cfg.profiles["bug-hunt"].max_per_day, Some(10), "a new pass reads the new value");

        std::fs::write(&project, "name = \"other\"\n").unwrap();
        assert!(sources.changed(), "the project config was edited");
    }
}
