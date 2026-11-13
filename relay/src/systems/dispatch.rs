//! dispatch: gives queued jobs a seat on the account with the most room, and
//! reaps sessions whose runner died. Emits: dispatched, reaped, unheld, hold-ended,
//! in-line, out-of-line, runner-exited.
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
use crate::config::{Config, Floor, Limits, Profile};
use crate::ctx::Ctx;
use crate::harness::{Harness, Home};
use crate::os::{self, Role};
use crate::world::{AccountState, DAY_MICROS, Job, Status, Wait, World};
pub use room::{Constraint, Hold, Pool, READING_WAIT_MICROS, Room, Start, memory_hold};

/// The longest sleep. Only a runner that dies without a word (one started by
/// an earlier dispatcher, or killed outright) waits this long to be reaped.
const BACKSTOP: Duration = Duration::from_mins(5);
/// The least time between gc passes the disk gauge starts.
const LOW_DISK_GAP: Duration = Duration::from_mins(5);
/// How often the dispatcher looks at free disk while a job the floor holds waits for it: no event
/// announces space coming back.
const HELD_POLL: Duration = Duration::from_mins(1);
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
/// changes); when that could change; the jobs a limit holds; and those of them
/// that would start but for the machine's compiling pool, so are in line for it.
#[derive(Debug, Default, PartialEq)]
struct Held(Vec<String>, Option<u64>, Vec<String>, Vec<String>);

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
    let floor = ctx.cfg.disk_floor();
    let low = floor.on() && os::free_disk_gb(&ctx.clones).is_some_and(|gb| gb < floor.gb);
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
    unhold(ctx, &port)?;
    let mut world = ctx.world()?;
    for j in world.jobs_mut().values_mut().filter(|j| j.status.has_runner()) {
        if !j.runner.is_some_and(|(pid, started)| os::same_process(pid, started)) {
            // Nothing is signalled: its sessions and handlers ended with it, each by its supervisor.
            port.emit(DispatchEvent::Reaped { job: j.name.clone() })?;
            j.status = Status::Failed;
        }
    }
    // A claim ends when its runner is seen gone with nothing of its tree left (its lock is free),
    // and only by this record of it: a dead process stays dead, so nothing is given back too soon.
    for c in world.claims.iter().filter(|c| !c.live()) {
        port.emit(DispatchEvent::RunnerExited { runner_pid: c.runner.0, runner_started: c.runner.1 })?;
        let _ = std::fs::remove_file(crate::world::claim_lock(c.runner));
    }
    // Accounts belong to the machine: what other projects learned of them, and
    // the seats their sessions hold, count here too. Their jobs stay theirs.
    let neighbours = ctx.neighbours();
    let dev_shell = crate::systems::devshell::Gate::of(ctx, &world);
    let mut room = Room::new(&ctx.cfg, &world, &neighbours.seen(), disk_ok(&ctx.cfg, &ctx.clones))
        .with_load(&ctx.cfg)
        .with_dev_shell(dev_shell.clone());
    crate::systems::quota::start_refresh(ctx, &room, &world, true)?;
    let mut held = Held::default();
    let starts = plan(&ctx.cfg, &mut room, &world, now_micros(), &mut held);
    let started: Vec<&str> = starts.iter().map(|s| s.job.as_str()).collect();
    line_up(&port, &world, &held.3, &started)?;
    for Start { job, account, model, provider } in starts {
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
    // One warm-up of the development environment for every job it holds, never one per job.
    if let Err(e) = crate::systems::devshell::tend(ctx, &world, dev_shell.as_ref()) {
        eprintln!("{} the dev shell's warm-up did not start: {e:#}", crate::os::stamp());
    }
    let held_due = ask_held(ctx, &world, &held)?;
    // A job blocked on a dependency that ended badly waits for the operator, not for the dispatcher;
    // one held by a runner (checking too) is watched until it ends or is reaped.
    let busy = world.jobs().values().any(|j| world.moves_on_its_own(j));
    let waiting = world.jobs().values().any(|j| world.startable(j));
    let states: Vec<_> = room.found().iter().map(|a| (harness_of(a), room.account(&a.key()))).collect();
    let due = next_due(&states, waiting, now_micros());
    // Free space coming back is no event: while the floor holds a job, look again soon.
    let poll = world
        .jobs()
        .values()
        .any(|j| j.held && world.moves_on_its_own(j))
        .then(|| now_micros() + u64::try_from(HELD_POLL.as_micros()).unwrap_or(0));
    Ok((busy, Held(held.0, None, Vec::new(), Vec::new()), [due, held.1, held_due, poll].into_iter().flatten().min()))
}

/// Records which of this project's queued jobs are in line for the machine's compiling pool
/// (`in_line`: those this pass held by it alone), so every other project's dispatcher leaves a unit
/// for each one that has waited longer than its own job. Written under the machine's seats lock,
/// as a dispatch is, and only when it differs from what the fold has: a job newly in line, or put
/// there by an earlier dispatcher, is recorded with this one; one in line no longer, and not
/// `started` this pass (its dispatch says so itself), is taken out.
fn line_up(port: &crate::bus::Port<DispatchEvent>, world: &World, in_line: &[String], started: &[&str]) -> Result<()> {
    let me = std::process::id();
    let planner = (me, os::start_time(me).unwrap_or(0));
    let pool = Pool::Compiling.key();
    for j in world.jobs().values().filter(|j| j.status == Status::Queued && !started.contains(&j.name.as_str())) {
        let wanted = in_line.contains(&j.name);
        let recorded = j.in_line.as_ref().is_some_and(|l| l.pool == pool && l.planner == planner);
        if wanted && !recorded {
            port.emit(DispatchEvent::InLine {
                job: j.name.clone(),
                pool: pool.clone(),
                planner_pid: planner.0,
                planner_started: planner.1,
            })?;
        } else if !wanted && j.in_line.is_some() {
            port.emit(DispatchEvent::OutOfLine { job: j.name.clone() })?;
        }
    }
    Ok(())
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

/// Whether free disk under the clones (`clones`) lets a build job start now, by the floor as it
/// stands now ([`Config::disk_floor`]): unmeasured is not enough.
pub fn disk_ok(cfg: &Config, clones: &Path) -> bool {
    cfg.disk_floor().enough(os::free_disk_gb(clones))
}

/// Resumes the job the floor has held longest, when free disk is back over the floor and its margin, or the
/// floor has been turned off ([`resume_due`]). Called with the machine's seats lock held, so the other
/// projects' jobs it waits on are the ones as they stand.
fn unhold(ctx: &Ctx, port: &crate::bus::Port<DispatchEvent>) -> Result<()> {
    let limits = &ctx.cfg.limits;
    let free = os::free_disk_gb(&ctx.clones);
    let neighbours = ctx.neighbours();
    let others: Vec<&World> = neighbours.iter().collect();
    match resume_due(limits, ctx.cfg.disk_floor(), &ctx.world()?, &others, free, now_micros()) {
        Some(Due::Resume { job, message, settle_us, budget_usd }) => {
            eprintln!(
                "{} resumed {job}: disk is back ({} GB free)",
                crate::os::stamp(),
                free.unwrap_or_default().round()
            );
            port.emit(DispatchEvent::Unheld { job, message, settle_us, budget_usd })?;
        }
        Some(Due::Release { job, why }) => {
            eprintln!("{} {job} stays stopped: {why}", crate::os::stamp());
            port.emit(DispatchEvent::HoldEnded { job, why })?;
        }
        None => {}
    }
    Ok(())
}

/// How long (bus time) a resumed job is given to show what it does to the disk: one sample of its clone.
fn settle_micros(limits: &Limits) -> u64 {
    u64::try_from(Duration::from_secs_f64(limits.disk_sample_minutes * 60.0).as_micros()).unwrap_or(u64::MAX)
}

/// What the dispatcher does about the floor's held jobs now.
#[derive(Debug, Clone, PartialEq)]
enum Due {
    /// Resume `job`, telling its session `message`; `budget_usd` caps the run to what the job's ceiling leaves.
    Resume { job: String, message: String, settle_us: u64, budget_usd: Option<f64> },
    /// Another run would pass the job's ceiling: it stays stopped, for `why`, until a person acts.
    Release { job: String, why: String },
}

/// The job to resume now, with the message its session is told: the one held longest whose dependencies are
/// done (a child held before its parent waits for it), when `free_gb` is over the floor and its margin or the
/// floor is off, and no job resumed from a hold, in this project or another on the machine (`others`), is
/// still settling (queued, or started less than a disk sample ago), so the space is not eaten again by two
/// at once.
fn resume_due(
    limits: &Limits,
    floor: Floor,
    world: &World,
    others: &[&World],
    free_gb: Option<f64>,
    now_us: u64,
) -> Option<Due> {
    if !floor.enough(free_gb) {
        return None;
    }
    let settle = settle_micros(limits);
    let settling = std::iter::once(world).chain(others.iter().copied()).flat_map(|w| w.jobs().values()).any(|j| {
        j.unheld_at.is_some_and(|at| match j.status {
            // Still queued, again after an automatic requeue too: it has not spent its space yet.
            Status::Queued => true,
            // Each against the interval of the project that resumed it (recorded with the resume).
            s if s.has_runner() => j.started_at.is_some_and(|t| {
                t >= at && now_us.saturating_sub(t) < if j.settle_us > 0 { j.settle_us } else { settle }
            }),
            _ => false,
        })
    });
    if settling {
        return None;
    }
    let j = world
        .jobs()
        .values()
        .filter(|j| j.held && j.status == Status::Stopped && world.waits(j).is_empty())
        .min_by_key(|j| (j.ended_at, &j.name))?;
    // A resume is another run: it may not pass the job's ceiling, so the run is capped to what is left of it.
    let mut budget_usd = None;
    if let Some(ceiling) = limits.job_ceiling_usd.filter(|c| j.cost_usd + j.budget_usd > *c) {
        let left = ceiling - j.cost_usd;
        if left <= 0.0 {
            let why = format!(
                "held by the disk floor, but another run would pass limits.job_ceiling_usd (${ceiling:.2}, ${:.2} spent): \
                 raise it, then `relay queue resume {}`",
                j.cost_usd, j.name
            );
            return Some(Due::Release { job: j.name.clone(), why });
        }
        budget_usd = Some(left);
    }
    let seen = j.reason.as_deref().unwrap_or("free disk was under the floor");
    let back = if floor.on() {
        format!("Free disk is back ({} GB free)", free_gb.unwrap_or_default().round())
    } else {
        "The disk floor has since been turned off".to_owned()
    };
    let message = format!(
        "Relay's disk floor paused this session ({seen}): the machine's free disk was under its floor, so \
         relay stopped the job rather than let the disk fill. Nothing in your work caused it, and your clone and \
         session are as you left them. {back}, so carry on from where you left off."
    );
    Some(Due::Resume { job: j.name.clone(), message, settle_us: settle, budget_usd })
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

/// Which queued jobs start now, where, and on what model. Pure: it reads only `room`. Each job is
/// judged by `room.judge` (the same judgment the board's reasons show), and each start is booked in
/// `room` before the next job is judged. Every job that does not start is held with its reason, by
/// whichever constraint binds (the project's `max_running` and the load as any other), so each
/// reaches the `held` hook. What other projects run counts against the machine's pools
/// (`limits.per_account`, `machine.max_compiling`), never against this project's `max_running`.
/// `now_us` is bus time, so a profile's daily window can be judged at any moment.
fn plan(cfg: &Config, room: &mut Room, world: &World, now_us: u64, held: &mut Held) -> Vec<Start> {
    let mut out = Vec::new();
    for j in queue(world) {
        let waits = world.waits(j);
        if !waits.is_empty() {
            if world.blocked(j) {
                held.0.push(format!("{}: {}", j.name, Wait::summary(&waits).unwrap_or_default()));
            }
            continue;
        }
        match place(cfg, room, j, now_us) {
            Ok(Some(start)) => out.push(start),
            Ok(None) => {}
            Err(Hold { why, until, by }) => {
                held.0.push(format!("{}: {why}", j.name));
                held.1 = [held.1, until].into_iter().flatten().min();
                held.2.push(j.name.clone());
                if by == Constraint::Pool(Pool::Compiling) {
                    held.3.push(j.name.clone());
                }
            }
        }
    }
    out
}

/// The one choice of a seat for a queued job: `j` judged by `room.judge`, the account chosen by
/// [`choose`], and the start booked in `room` ([`Room::take`]) so whatever is judged next sees it.
fn place(cfg: &Config, room: &mut Room, j: &Job, now_us: u64) -> Result<Option<Start>, Hold> {
    let fits = room.judge(cfg, j, now_us)?;
    let Some(home) = choose(j, &fits, None).cloned() else { return Ok(None) };
    Ok(Some(room.take(cfg, j, &home, now_us)))
}

/// What a decision's `agent:` session for `worker` is judged as: a read-only job on `harness` alone,
/// on `model` there (the harness's configured one when `None`), of no profile, queued at `since`
/// (bus time: when it began to wait for a seat, which bounds its wait for a reading as a job's).
fn session_job(worker: &Job, harness: &str, model: Option<&str>, since: u64) -> Job {
    Job {
        name: format!("{}.check", worker.name),
        kind: crate::bus::Kind::Read,
        harnesses: vec![harness.to_owned()],
        models: model.map(|m| [(harness.to_owned(), m.to_owned())].into()).unwrap_or_default(),
        queued_at: since,
        ..Job::default()
    }
}

/// A seat in `room` for a decision's `agent:` session for `worker` ([`session_job`]), judged as a
/// session ([`Room::judge_session`]: the memory gate and the accounts) and chosen as a queued job's
/// is ([`choose`]): another account than the worker's where one fits, so no worker grades
/// itself; else the worker's own. The worker's seat is as the fold has it: a run that asked at `cap`
/// or `ended` is `checking` and holds none, while one at `disk` still runs and keeps its seat.
pub fn place_session(
    cfg: &Config,
    room: &mut Room,
    worker: &Job,
    harness: &str,
    model: Option<&str>,
    since: u64,
    now_us: u64,
) -> Result<Start, Hold> {
    let j = session_job(worker, harness, model, since);
    let fits = room.judge_session(cfg, &j, now_us)?;
    let Some(home) = choose(&j, &fits, worker.account.as_deref()).cloned() else {
        return Err(Hold { why: format!("no {harness} account has room"), until: None, by: Constraint::Accounts });
    };
    Ok(room.take_session(cfg, &j, &home))
}

/// Books a seat for decision `id`'s `agent:` session for `worker` ([`place_session`]) under the
/// machine's seat lock, as a dispatch books a job's: `record` writes the booking to the bus before the
/// lock is let go, so every dispatcher (and every other decision) that plans next counts it. A
/// booking the fold did not take, since the decision is no longer on record (its job was cleaned
/// away meanwhile), is an error: the session must not start. The lock is never waited for: held by
/// another, it is [`Booking::Busy`], for the caller's wait to try again within its own bound.
pub fn book_session(
    ctx: &Ctx,
    id: &str,
    worker: &Job,
    harness: &str,
    model: Option<&str>,
    since: u64,
    record: impl FnOnce(&Start) -> Result<()>,
) -> Result<Booking> {
    let Some(_seats) = Role::try_take(&ctx.state_root.join("seats.lock"))? else { return Ok(Booking::Busy) };
    let world = ctx.world()?;
    let neighbours = ctx.neighbours();
    // A decision's session is read-only: free disk does not hold it, nor does the dev shell.
    let mut room = Room::new(&ctx.cfg, &world, &neighbours.seen(), true);
    let start = match place_session(&ctx.cfg, &mut room, worker, harness, model, since, now_micros()) {
        Ok(start) => start,
        Err(hold) => return Ok(Booking::Held(hold)),
    };
    let Some(home) = room.found().iter().find(|h| h.key() == start.account).cloned() else {
        anyhow::bail!("{} was chosen but is not found", start.account);
    };
    record(&start)?;
    if !ctx.world()?.seated(id) {
        anyhow::bail!("decision {id} is no longer on record, so its seat was not booked");
    }
    Ok(Booking::Booked(home))
}

/// What a try at booking a decision's seat came to ([`book_session`]).
#[derive(Debug)]
pub enum Booking {
    /// Booked and recorded: the home of the account it is on.
    Booked(Home),
    /// The session does not start now: no account takes it, or the machine's memory gate holds it.
    Held(Hold),
    /// The machine's seat lock is held by another planner for now.
    Busy,
}

/// The queued jobs in the order [`plan`] judges them in: the one that has waited longest first
/// ([`Job::waiting_since`]). It is the one order of every line: within the project here, and
/// across projects for a unit of the machine's compiling pool, so a job is never first in one and
/// behind in the other.
fn queue(world: &World) -> Vec<&Job> {
    let mut queued: Vec<&Job> = world.jobs().values().filter(|j| j.status == Status::Queued).collect();
    queued.sort_by_key(|j| j.waiting_since());
    queued
}

/// The account the next start would take, by [`plan`]'s own judgment and choice, with the job it
/// would start: the first queued job free of waits that `room` admits somewhere. With no such job
/// queued, the account a job that asks for nothing would get. `None` when every queued job free
/// of waits is held. The load, the memory, `max_running` and the machine's compiling pool aside:
/// they say when, not where ([`Room::judge_where`]).
pub fn next_pick(cfg: &Config, room: &Room, world: &World, now_us: u64) -> Option<(Option<String>, String)> {
    let pick = |j: &Job| room.judge_where(cfg, j, now_us).ok().and_then(|fits| choose(j, &fits, None).map(Home::key));
    let free: Vec<&Job> = queue(world).into_iter().filter(|j| world.waits(j).is_empty()).collect();
    if free.is_empty() {
        return pick(&Job::default()).map(|a| (None, a));
    }
    free.into_iter().find_map(|j| pick(j).map(|a| (Some(j.name.clone()), a)))
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

/// The account whose transcript `job` would resume, if any: its own resumable session's, or, for
/// a fork that has no session of its own yet, the account of the session it forks.
fn kept(job: &Job) -> Option<&str> {
    let fork = job.fork.as_ref().filter(|f| job.session_id.as_deref().is_none_or(|s| s == f.session_id));
    let own = job.session_id.as_ref().and(job.transcript_account.as_deref());
    fork.map(|f| f.account.as_str()).or(own)
}

/// Of the accounts that fit, the one a resumable session already ran on ([`kept`]), so
/// the provider's prompt cache still holds its transcript (moving it means
/// re-reading all of it at full price); otherwise the best-scoring one, its room shared among the
/// sessions it runs ([`Room::admit`]). `avoid` is an account to take only when no other fits: a
/// decision's session keeps off its worker's.
fn choose<'a>(job: &Job, fits: &[(&'a Home, f64)], avoid: Option<&str>) -> Option<&'a Home> {
    let home = kept(job).and_then(|last| fits.iter().find(|(a, _)| a.key() == last));
    let best = |fits: &mut dyn Iterator<Item = &(&'a Home, f64)>| fits.max_by(|(_, x), (_, y)| x.total_cmp(y)).copied();
    let elsewhere = || best(&mut fits.iter().filter(|(a, _)| avoid.is_none_or(|w| a.key() != w)));
    home.copied().or_else(elsewhere).or_else(|| best(&mut fits.iter())).map(|(a, _)| a)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::room::{Gauges, LINGER_MICROS, LOOK_AGAIN_MICROS};
    use super::*;
    use crate::accounts;
    use crate::config::{Machine, Quantity};
    use crate::harness::Meter;
    use crate::world::Claim;

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
        assert_eq!(key(choose(&job(None, None), &fits, None)), Some("claude:b".into()), "best score");
        assert_eq!(key(choose(&job(Some("s"), Some("claude:a")), &fits, None)), Some("claude:a".into()), "its own");
        assert_eq!(
            key(choose(&job(None, Some("claude:a")), &fits, None)),
            Some("claude:b".into()),
            "no session to keep"
        );
        let only_b = [(&b, 0.9)];
        assert_eq!(
            key(choose(&job(Some("s"), Some("claude:a")), &only_b, None)),
            Some("claude:b".into()),
            "a does not fit"
        );
        assert_eq!(choose(&job(Some("s"), Some("claude:a")), &[], None).map(Home::key), None);
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

        a.meters.insert("five_hour".into(), Meter { utilization: 1.0, resets_at: Some(1_100), ..Meter::default() });
        a.meters.insert("old".into(), Meter { utilization: 1.0, resets_at: Some(900), ..Meter::default() });
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
        let meter = |name: &str, utilization| Meter::named(name, utilization, Some(reset));
        let mut world = World::default();
        world.accounts.insert(
            "omp:default".into(),
            AccountState {
                read_at: Some(NOW_US - 1),
                meters: [
                    ("openai-codex/5 hours".to_owned(), meter("openai-codex/5 hours", codex)),
                    ("minimax-code/5 hours".to_owned(), meter("minimax-code/5 hours", minimax)),
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

    /// The board's next pick is the first start a pass would make: the first job not held, on
    /// the account [`choose`] gives it; with nothing queued, the best-scoring account.
    #[test]
    fn the_next_pick_is_the_start_plan_would_make() {
        let (_d, cfg) = omp_config("[harness.omp]\nenabled = false\n");
        let reset = |hours: i64| Some(i64::try_from(NOW_US / 1_000_000).unwrap() + hours * 3600);
        let mut world = World::default();
        for (label, utilization, hours) in [("a", 0.5, 24), ("b", 0.2, 144)] {
            let meters =
                [("seven_day".to_owned(), Meter { utilization, resets_at: reset(hours), ..Meter::default() })].into();
            let st = AccountState { read_at: Some(NOW_US - 1), meters, ..Default::default() };
            world.accounts.insert(format!("claude:{label}"), st);
        }
        let found = vec![home("claude", "a"), home("claude", "b")];
        let pick = |w: &World| next_pick(&cfg, &Room::of(found.clone(), w.accounts.clone(), w, &[], true), w, NOW_US);
        assert_eq!(pick(&world), Some((None, "claude:a".into())), "room that would be lost at a reset first");
        // The oldest job may use no account found, so it is held; the next resumes a session on b.
        let held = Job { name: "held".into(), pinned: Some("claude:gone".into()), ..Job::default() };
        let resume = Job {
            name: "resume".into(),
            created_at: 1,
            session_id: Some("s".into()),
            transcript_account: Some("claude:b".into()),
            ..Job::default()
        };
        for j in [held, resume] {
            world.jobs_mut().insert(j.name.clone(), Job { status: Status::Queued, ..j });
        }
        let (starts, _) = plan_omp(&cfg, &world, &found);
        assert_eq!(starts[0], ("resume".to_owned(), "claude:b".to_owned()));
        assert_eq!(pick(&world), Some((Some(starts[0].0.clone()), starts[0].1.clone())));
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

    /// puku accounts are judged by the one [`Room::admit`] as claude's are: one seat budget per account
    /// whatever the model (never per provider, as omp's), a fresh reading below the floor, and every
    /// meter governing whatever model the job names.
    #[test]
    fn a_puku_account_is_admitted_as_a_claude_one_is() {
        let (_d, cfg) = omp_config("");
        let reset = i64::try_from(NOW_US / 1_000_000).unwrap() + 3600;
        let state = |utilization| AccountState {
            read_at: Some(NOW_US - 1),
            meters: [("seven_day".to_owned(), Meter { utilization, resets_at: Some(reset), ..Meter::default() })]
                .into(),
            ..Default::default()
        };
        let room = |accounts: &[(&str, AccountState)], world: &World| {
            let found = vec![home("puku", "p"), home("claude", "c")];
            let heard = accounts.iter().map(|(k, a)| ((*k).to_owned(), a.clone())).collect();
            Room::of(found, heard, world, &[], true)
        };
        let both = [("puku:p", state(0.1)), ("claude:c", state(0.1))];
        let r = room(&both, &World::default());
        assert!(r.admit(&cfg.limits, &home("puku", "p"), Some("puku-ai-2.8"), NOW_US).is_ok());
        assert!(r.admit(&cfg.limits, &home("claude", "c"), Some("claude-opus-5-5"), NOW_US).is_ok());
        // One running session takes the account's one seat, on any model, as on claude.
        let mut world = World::default();
        let j = Job {
            name: "a".into(),
            status: Status::Running,
            account: Some("puku:p".into()),
            models: BTreeMap::from([("puku".to_owned(), "puku-ai-2.7".to_owned())]),
            ..Job::default()
        };
        world.jobs_mut().insert(j.name.clone(), j);
        let busy = room(&both, &world).admit(&cfg.limits, &home("puku", "p"), Some("puku-ai-2.8"), NOW_US);
        assert_eq!(busy.unwrap_err().busy.as_deref(), Some("busy (1 of 1 seat)"), "no provider in it");
        // At its floor, or never read, it takes nothing.
        let full = room(&[("puku:p", state(0.95))], &World::default());
        assert!(full.admit(&cfg.limits, &home("puku", "p"), Some("puku-ai-2.8"), NOW_US).unwrap_err().fit.is_some());
        let unread = room(&[], &World::default());
        assert!(unread.admit(&cfg.limits, &home("puku", "p"), Some("puku-ai-2.8"), NOW_US).is_err());
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

    /// Copilot review of PR #41, finding 1: a run dispatched before providers were recorded, on its
    /// project's configured default, names no model and no provider. Its seat may be on any
    /// provider, so it counts against every one: with `per_account = 1` nothing more starts on that
    /// omp account until it ends, whichever provider the next job asks for.
    #[test]
    fn a_seat_whose_provider_is_unknown_counts_against_every_provider() {
        let (_d, cfg) = omp_config("");
        let mut old = theirs("x", None);
        let x = old.jobs_mut().get_mut("x").unwrap();
        assert_eq!((x.model.as_deref(), x.provider.as_str(), x.models.len()), (None, "", 0), "nothing recorded");
        for model in ["openai-codex/gpt-6.1", "minimax-code/M3"] {
            let (world, found) = omp_world(&[("next", Status::Queued, model)], 0.1, 0.1);
            let (out, held) = plan_among(&cfg, &world, &[&old], &found);
            assert_eq!(out, [], "{model}: {:?}", held.0);
            let provider = model.split_once('/').unwrap().0;
            assert_eq!(held.0, [format!("next: no account has room (omp:default busy (1 of 1 seat on {provider}))")]);
        }
        // Once it ends, its seat is free again.
        old.jobs_mut().get_mut("x").unwrap().status = Status::Done;
        let (world, found) = omp_world(&[("next", Status::Queued, "openai-codex/gpt-6.1")], 0.1, 0.1);
        assert_eq!(plan_among(&cfg, &world, &[&old], &found).0.len(), 1);
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

    /// A config whose seats and running limit leave a burst of 12 to the accounts' room.
    fn burst_config() -> (crate::config::tempdir::Dir, Config) {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = "name = \"demo\"\n[limits]\nmin_free_disk_gb = 0\nper_account = 16\nmax_running = 20\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        (d, cfg)
    }

    /// Claude accounts, each `(label, utilization, hours to its reset)` read at `read_at`, and
    /// `jobs` read-only jobs queued at `NOW_US`.
    fn burst(accounts: &[(&str, f64, i64)], read_at: u64, jobs: usize) -> (World, Vec<Home>) {
        let now_s = i64::try_from(NOW_US / 1_000_000).unwrap();
        let mut world = World::default();
        for &(label, utilization, hours) in accounts {
            let meters = [(
                "seven_day".to_owned(),
                Meter { utilization, resets_at: Some(now_s + hours * 3600), ..Meter::default() },
            )];
            let st = AccountState { read_at: Some(read_at), meters: meters.into(), ..Default::default() };
            world.accounts.insert(format!("claude:{label}"), st);
        }
        for i in 0..jobs {
            let j = Job {
                name: format!("j{i:02}"),
                status: Status::Queued,
                kind: crate::bus::Kind::Read,
                created_at: i as u64,
                queued_at: NOW_US,
                ..Job::default()
            };
            world.jobs_mut().insert(j.name.clone(), j);
        }
        (world, accounts.iter().map(|(label, ..)| home("claude", label)).collect())
    }

    /// Bug of 2026-11-12 (`verify.md` of the pools review): with `per_account = 1` on accounts a and b,
    /// job x on a ends and its decision frees a. Its `agent:` session went to b, chosen with no lock and
    /// never recorded, and the dispatcher then started queued job q on b too: two sessions on b. A
    /// decision's seat is now placed by the dispatcher's own choice and booked on the bus, so the fold
    /// counts it: q takes a, and two decisions at once take different accounts or one waits.
    #[test]
    fn a_decisions_session_holds_its_seat_as_a_job_does() {
        use crate::bus::{DecideEvent, Event, Point, Stamped};
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg =
            Config::parse("name = \"demo\"\n[limits]\nmin_free_disk_gb = 0\nper_account = 1\n", d.path()).unwrap();
        let me = std::process::id();
        let waiter = Some((me, os::start_time(me).unwrap()));
        let apply =
            |w: &mut World, e: DecideEvent| w.apply(&Stamped { t: NOW_US, event: Event::Decide(e), from: None });
        let running = |w: &mut World, name: &str, account: &str| {
            let j = Job {
                name: name.into(),
                status: Status::Running,
                kind: crate::bus::Kind::Read,
                account: Some(account.into()),
                created_at: 100,
                attempts: 1,
                ..Job::default()
            };
            w.jobs_mut().insert(j.name.clone(), j);
        };
        // The run of `job` ends and asks: its seat is given back.
        let ask = |w: &mut World, job: &str| {
            let asked = DecideEvent::DecisionRequested {
                id: format!("{job}-ended-1"),
                job: job.into(),
                point: Point::Ended,
                created: 100,
                attempt: 1,
                waiter,
            };
            apply(w, asked);
        };
        // Its `agent:` session is placed and booked as a dispatch places and books a job.
        let seat = |w: &mut World, job: &str| -> Result<Start, Hold> {
            let worker = w.jobs()[job].clone();
            let mut room = Room::of(found_ab(), w.accounts.clone(), w, &[], true);
            let start = place_session(&cfg, &mut room, &worker, "claude", None, 0, NOW_US)?;
            let seated = DecideEvent::Seated {
                id: format!("{job}-ended-1"),
                account: start.account.clone(),
                model: start.model.clone(),
                provider: start.provider.clone(),
                lock: None,
            };
            apply(w, seated);
            Ok(start)
        };
        let decide = |w: &mut World, job: &str| {
            ask(w, job);
            seat(w, job)
        };
        let sessions = |w: &World| {
            let room = Room::of(found_ab(), w.accounts.clone(), w, &[], true);
            ["claude:a", "claude:b"].map(|a| room.running_on(a))
        };
        let plan_q = |w: &World| plan_among(&cfg, w, &[], &found_ab()).0;

        // One decision: its session keeps off its worker's account, and q takes the seat left.
        let (mut w, _) = burst(&[("a", 0.3, 72), ("b", 0.3, 72)], NOW_US - 1, 1);
        running(&mut w, "x", "claude:a");
        assert_eq!(decide(&mut w, "x").map(|s| s.account), Ok("claude:b".into()), "not the worker's own account");
        assert_eq!(sessions(&w), [0, 1], "the decision's session holds b; x's ended run holds nothing");
        assert_eq!(plan_q(&w), [("j00".into(), "claude:a".into())], "q never joins the decision on b");
        // Its seat is free once the session has ended.
        apply(&mut w, DecideEvent::Unseated { id: "x-ended-1".into() });
        assert_eq!(sessions(&w), [0, 0]);

        // Two decisions at once: each takes the other's freed account, and q waits.
        let (mut w, _) = burst(&[("a", 0.3, 72), ("b", 0.3, 72)], NOW_US - 1, 1);
        running(&mut w, "x", "claude:a");
        running(&mut w, "y", "claude:b");
        ask(&mut w, "x");
        ask(&mut w, "y");
        assert_eq!(seat(&mut w, "x").map(|s| s.account), Ok("claude:b".into()));
        assert_eq!(seat(&mut w, "y").map(|s| s.account), Ok("claude:a".into()), "b is the first decision's");
        assert_eq!(sessions(&w), [1, 1], "never two sessions on one account");
        let (starts, held) = plan_among(&cfg, &w, &[], &found_ab());
        assert!(starts.is_empty(), "{starts:?}");
        assert!(held.0.iter().any(|h| h.contains("busy (1 of 1 seat)")), "{held:?}");
        // A third decision at once (q's, held) finds no seat: it waits, or, unable to, defers.
        let mut room = Room::of(found_ab(), w.accounts.clone(), &w, &[], true);
        let hold = place_session(&cfg, &mut room, &w.jobs()["j00"].clone(), "claude", None, 0, NOW_US).unwrap_err();
        assert!(hold.why.contains("busy"), "{hold:?}");

        // A seat whose session's process has died holds nothing: nobody is left to give it back.
        let (mut w, _) = burst(&[("a", 0.3, 72), ("b", 0.3, 72)], NOW_US - 1, 1);
        running(&mut w, "x", "claude:a");
        decide(&mut w, "x").unwrap();
        w.seats.get_mut("x-ended-1").unwrap().waiter = Some((me, 1));
        assert_eq!(sessions(&w), [0, 0]);
    }

    /// `per_account = 1` on accounts a and b, both read and with room.
    fn one_seat_each() -> (crate::config::tempdir::Dir, Config, World) {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = "name = \"demo\"\n[limits]\nmin_free_disk_gb = 0\nper_account = 1\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        let (w, _) = burst(&[("a", 0.3, 72), ("b", 0.3, 72)], NOW_US - 1, 0);
        (d, cfg, w)
    }

    fn run_on(w: &mut World, name: &str, account: &str) {
        let j = Job {
            name: name.into(),
            status: Status::Running,
            kind: crate::bus::Kind::Read,
            account: Some(account.into()),
            created_at: 100,
            attempts: 1,
            ..Job::default()
        };
        w.jobs_mut().insert(j.name.clone(), j);
    }

    fn decide_event(w: &mut World, t: u64, e: crate::bus::DecideEvent) {
        w.apply(&crate::bus::Stamped { t, event: crate::bus::Event::Decide(e), from: None });
    }

    /// Copilot on PR #63, finding 1: a `disk` decision runs while its worker still runs (its job
    /// stays `running`), so the worker's seat is not the decision's to take. With b busy and a the
    /// worker's, a disk decision finds no seat rather than a second session on a.
    #[test]
    fn a_disk_decision_never_takes_its_running_workers_seat() {
        let (_d, cfg, mut w) = one_seat_each();
        run_on(&mut w, "x", "claude:a");
        run_on(&mut w, "y", "claude:b");
        let me = std::process::id();
        let asked = crate::bus::DecideEvent::DecisionRequested {
            id: "x-disk-1".into(),
            job: "x".into(),
            point: crate::bus::Point::Disk,
            created: 100,
            attempt: 1,
            waiter: Some((me, os::start_time(me).unwrap())),
        };
        decide_event(&mut w, NOW_US, asked);
        assert_eq!(w.jobs()["x"].status, Status::Running, "a disk decision leaves its worker running");
        let mut room = Room::of(found_ab(), w.accounts.clone(), &w, &[], true);
        let worker = w.jobs()["x"].clone();
        let placed = place_session(&cfg, &mut room, &worker, "claude", None, 0, NOW_US);
        assert!(placed.as_ref().is_err_and(|h| h.why.contains("busy")), "{placed:?}");
        // With b free, it goes there.
        w.jobs_mut().remove("y");
        let mut room = Room::of(found_ab(), w.accounts.clone(), &w, &[], true);
        let placed = place_session(&cfg, &mut room, &worker, "claude", None, 0, NOW_US);
        assert_eq!(placed.map(|s| s.account), Ok("claude:b".into()));
    }

    /// Copilot on PR #63, finding 2: a held job dequeued and cleaned while its decision's session runs
    /// forgets the decision, but the session still runs until its handler hears and stops: its seat
    /// counts until `unseated` or its waiter's death, not until the cleanup. A booking that arrives
    /// once its decision is gone is not recorded, and the booker can tell ([`World::seated`]).
    #[test]
    fn a_seat_outlives_the_cleanup_of_its_decision() {
        use crate::bus::{DecideEvent, Event, LandEvent, Point, Stamped};
        let (_d, _cfg, mut w) = one_seat_each();
        let me = std::process::id();
        let waiter = Some((me, os::start_time(me).unwrap()));
        let asked = |id: &str| DecideEvent::DecisionRequested {
            id: id.into(),
            job: "q".into(),
            point: Point::Held,
            created: 100,
            attempt: 0,
            waiter,
        };
        let seated = |id: &str| DecideEvent::Seated {
            id: id.into(),
            account: "claude:b".into(),
            model: None,
            provider: String::new(),
            lock: None,
        };
        let forgotten = Stamped { t: NOW_US, event: Event::Land(LandEvent::Forgotten { job: "q".into() }), from: None };
        let on_b = |w: &World| Room::of(found_ab(), w.accounts.clone(), w, &[], true).running_on("claude:b");
        decide_event(&mut w, NOW_US, asked("q-held-1"));
        decide_event(&mut w, NOW_US, seated("q-held-1"));
        assert!(w.seated("q-held-1"));
        w.apply(&forgotten);
        assert!(!w.decisions.contains_key("q-held-1"), "the decision went with its job");
        assert_eq!(on_b(&w), 1, "its session still runs on b");
        decide_event(&mut w, NOW_US, DecideEvent::Unseated { id: "q-held-1".into() });
        assert_eq!(on_b(&w), 0);
        // Asked again, and cleaned before the booking lands: the booking is not recorded.
        decide_event(&mut w, NOW_US, asked("q-held-2"));
        w.apply(&forgotten);
        decide_event(&mut w, NOW_US, seated("q-held-2"));
        assert!(!w.seated("q-held-2"), "a booking for a decision no longer on record");
        assert_eq!(on_b(&w), 0);
    }

    /// Copilot on PR #63, third pass, finding 1: a booking waited on the machine's seat lock for as
    /// long as another holder kept it, where nothing checks the wait's bound or a withdrawal. It
    /// returns at once instead, for the wait loop to try again.
    #[test]
    fn booking_a_decisions_seat_never_waits_on_a_busy_seat_lock() {
        let d = crate::config::tempdir::Dir::new();
        let root = d.path().to_owned();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let _held = Role::take(&root.join("state/seats.lock")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cfg = Config::parse("name = \"t\"\n", &root.join("repo")).unwrap();
            let ctx = Ctx::unclaimed(cfg, root.join("repo/relay.toml"), root.join("state"), root.join("data"));
            let booked = book_session(&ctx, "d", &Job::default(), "claude", None, 0, |_| Ok(()));
            let _ = tx.send(matches!(booked, Ok(Booking::Busy)));
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true), "a busy seat lock is a hold, at once");
    }

    /// Copilot on PR #63, third pass, finding 2: the booker of a decision's seat (its waiter) can die
    /// while its session's process tree still runs, until the supervisor sees and ends it. The seat
    /// counts while anything holds its lock (the tree inherits it), not only while the booker lives.
    #[test]
    fn a_seat_counts_while_its_lock_is_held_past_its_bookers_death() {
        use crate::bus::{DecideEvent, Point};
        let (d, _cfg, mut w) = one_seat_each();
        let lock = d.path().join("seat.lock");
        let dead = Some((std::process::id(), 1));
        let asked = DecideEvent::DecisionRequested {
            id: "q-held-1".into(),
            job: "q".into(),
            point: Point::Held,
            created: 100,
            attempt: 0,
            waiter: dead,
        };
        decide_event(&mut w, NOW_US, asked);
        let tree = crate::os::SeatHold::take(&lock).unwrap();
        let seated = DecideEvent::Seated {
            id: "q-held-1".into(),
            account: "claude:b".into(),
            model: None,
            provider: String::new(),
            lock: Some(lock.clone()),
        };
        decide_event(&mut w, NOW_US, seated);
        let on_b = |w: &World| Room::of(found_ab(), w.accounts.clone(), w, &[], true).running_on("claude:b");
        assert_eq!(on_b(&w), 1, "its booker is gone, but its session's tree still holds the lock");
        drop(tree);
        assert_eq!(on_b(&w), 0, "nothing holds it: the tree has ended");
    }

    fn found_ab() -> Vec<Home> {
        vec![home("claude", "a"), home("claude", "b")]
    }

    /// How many of `starts` each account took.
    fn spread(starts: &[(String, String)]) -> BTreeMap<String, usize> {
        let mut n = BTreeMap::new();
        for (_, account) in starts {
            *n.entry(account.clone()).or_default() += 1;
        }
        n
    }

    /// The incident of 2026-11-12: twelve jobs queued at once all went to one account, since its
    /// score did not change as the pass booked sessions on it. Shared among its sessions, it does.
    #[test]
    fn a_burst_spreads_over_accounts_with_like_room() {
        let (_d, cfg) = burst_config();
        let (world, found) = burst(&[("a", 0.30, 72), ("b", 0.32, 72), ("c", 0.31, 72)], NOW_US - 1, 12);
        let (starts, held) = plan_among(&cfg, &world, &[], &found);
        assert_eq!(starts.len(), 12, "{:?}", held.0);
        let n = spread(&starts);
        assert!(n.values().all(|&k| (3..=5).contains(&k)) && n.len() == 3, "{n:?}");
    }

    /// Room that a reset soon would take away still goes first, and gets the larger share.
    #[test]
    fn an_account_resetting_soon_with_more_room_takes_more_of_a_burst() {
        let (_d, cfg) = burst_config();
        // a: 50% left a day before its reset; b and c: 70% left with six days to go.
        let (world, found) = burst(&[("a", 0.5, 24), ("b", 0.3, 144), ("c", 0.3, 144)], NOW_US - 1, 12);
        let (starts, _) = plan_among(&cfg, &world, &[], &found);
        let n = spread(&starts);
        assert_eq!(starts[0].1, "claude:a", "the first start");
        assert!(n["claude:a"] > n["claude:b"] + n["claude:c"], "{n:?}");
        assert!(n["claude:b"] >= 1 && n["claude:c"] >= 1, "{n:?}");
    }

    /// A job that resumes a session stays on its transcript's account, however the burst spreads.
    #[test]
    fn a_resumed_job_in_a_burst_stays_on_its_transcripts_account() {
        let (_d, cfg) = burst_config();
        let (mut world, found) = burst(&[("a", 0.5, 24), ("b", 0.3, 144), ("c", 0.3, 144)], NOW_US - 1, 12);
        for name in ["j00", "j05", "j11"] {
            let j = world.jobs_mut().get_mut(name).unwrap();
            (j.session_id, j.transcript_account) = (Some(format!("s-{name}")), Some("claude:c".into()));
        }
        let (starts, _) = plan_among(&cfg, &world, &[], &found);
        for name in ["j00", "j05", "j11"] {
            assert!(starts.contains(&(name.to_owned(), "claude:c".to_owned())), "{name}: {starts:?}");
        }
    }

    /// An account whose reading is under way, and whose last reading had more room, is waited for
    /// a while rather than letting the account read a moment ago take the burst; never past the
    /// wait, never for an account at its floor when last read, one whose read failed, one with less
    /// room, or when the job resumes on a fresh account.
    #[test]
    fn a_reading_under_way_is_waited_for_a_while() {
        let (_d, cfg) = burst_config();
        let (mut world, found) = burst(&[("a", 0.6, 72), ("b", 0.1, 72), ("c", 0.6, 72)], NOW_US - 1, 1);
        // b was read 11 minutes ago, so it went stale a minute ago and is being read again.
        let b_read = NOW_US - 11 * 60 * 1_000_000;
        world.accounts.get_mut("claude:b").unwrap().read_at = Some(b_read);
        // Queued before that, so its wait runs from when b went stale.
        let job = Job { queued_at: NOW_US - 30 * 60 * 1_000_000, ..world.jobs()["j00"].clone() };
        // The accounts that admit the job, by key, or why it is held.
        let judge = |w: &World, j: &Job, now| {
            let room = Room::of(found.clone(), w.accounts.clone(), w, &[], true);
            room.judge(&cfg, j, now).map(|fits| fits.iter().map(|(a, _)| a.key()).collect::<Vec<_>>())
        };

        let hold = judge(&world, &job, NOW_US).unwrap_err();
        assert!(
            hold.why.starts_with("waiting for a reading of claude:b before starting on claude:a, claude:c"),
            "{}",
            hold.why
        );
        let stale_at = b_read + accounts::STALE_MICROS + 1;
        assert_eq!(hold.until, Some(stale_at + room::READING_WAIT_MICROS), "two minutes from when it went stale");
        // A job queued later waits two minutes from then.
        let later = Job { queued_at: NOW_US + 1, ..job.clone() };
        assert_eq!(judge(&world, &later, NOW_US).unwrap_err().until, Some(NOW_US + 1 + room::READING_WAIT_MICROS));

        let past = stale_at + room::READING_WAIT_MICROS;
        let fits = judge(&world, &job, past).expect("the wait is over");
        assert_eq!(fits, ["claude:a", "claude:c"]);

        let resumes = Job { session_id: Some("s".into()), transcript_account: Some("claude:a".into()), ..job.clone() };
        assert!(judge(&world, &resumes, NOW_US).is_ok(), "its transcript's account fits now");

        let mut failed = world.clone();
        failed.accounts.get_mut("claude:b").unwrap().unreadable_at = Some(NOW_US - 1);
        assert!(judge(&failed, &job, NOW_US).is_ok(), "a failed read backs off: nothing is under way");

        for (utilization, why) in [(0.95, "at its floor when last read"), (0.7, "less room than a fresh account")] {
            let mut w = world.clone();
            w.accounts.get_mut("claude:b").unwrap().meters.get_mut("seven_day").unwrap().utilization = utilization;
            assert!(judge(&w, &job, NOW_US).is_ok(), "{why}");
        }

        let mut unread = world.clone();
        unread.accounts.remove("claude:b");
        let hold = judge(&unread, &later, NOW_US).unwrap_err();
        assert_eq!(hold.until, Some(NOW_US + 1 + room::READING_WAIT_MICROS), "never read: its room is unknown");
        assert!(judge(&unread, &job, NOW_US).is_ok(), "and the wait runs from when the job was queued");
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

    /// A world of jobs by `(name, status, held, ended_at)`.
    fn resumed(due: Due) -> Option<String> {
        match due {
            Due::Resume { job, .. } => Some(job),
            Due::Release { .. } => None,
        }
    }

    fn message(due: Due) -> String {
        match due {
            Due::Resume { message, .. } => message,
            Due::Release { why, .. } => why,
        }
    }

    fn held_world(jobs: &[(&str, Status, bool, u64)]) -> World {
        let mut w = World::default();
        for &(name, status, held, ended) in jobs {
            let j = Job { name: name.into(), status, held, ended_at: Some(ended), ..Job::default() };
            w.jobs_mut().insert(name.into(), j);
        }
        w
    }

    fn floor(min: f64, margin: f64) -> Limits {
        Limits { min_free_disk_gb: min.into(), disk_resume_margin_gb: margin, ..Limits::default() }
    }

    /// The floor of a project with these limits and no machine floor over it.
    fn of(limits: &Limits) -> Floor {
        Floor::of(limits, &Machine::default())
    }

    #[test]
    fn a_held_job_resumes_only_over_the_floor_and_its_margin() {
        let l = floor(60.0, 3.0);
        let w = held_world(&[("a", Status::Stopped, true, 5)]);
        let due = |free| resume_due(&l, of(&l), &w, &[], free, NOW_US).and_then(resumed);
        assert_eq!(due(Some(57.0)), None, "under the floor");
        assert_eq!(due(Some(62.9)), None, "over the floor, not over its margin: no flapping");
        assert_eq!(due(None), None, "unmeasured is not room");
        assert_eq!(due(Some(63.0)).as_deref(), Some("a"));
        let off = floor(0.0, 3.0);
        assert!(!of(&off).on());
        let message = message(resume_due(&l, of(&l), &w, &[], Some(65.2), NOW_US).unwrap());
        assert!(message.contains("disk floor") && message.contains("Nothing in your work"), "{message}");
        assert!(message.contains("65 GB free"), "{message}");
    }

    #[test]
    fn a_person_s_stop_is_never_resumed() {
        let l = floor(60.0, 3.0);
        // Stopped and not held: by a person (or any other end), whatever the disk says.
        let w = held_world(&[("p", Status::Stopped, false, 1), ("f", Status::Failed, false, 1)]);
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(900.0), NOW_US), None);
    }

    #[test]
    fn held_jobs_resume_one_at_a_time_oldest_first() {
        let l = floor(60.0, 3.0);
        let mut w = held_world(&[("late", Status::Stopped, true, 9), ("early", Status::Stopped, true, 4)]);
        let first = resumed(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US).unwrap()).unwrap();
        assert_eq!(first, "early", "the one held longest");
        // It is resumed: queued, and the next waits while it has not started.
        let at = NOW_US - 10;
        let j = w.jobs_mut().get_mut("early").unwrap();
        (j.status, j.held, j.unheld_at, j.queued_at) = (Status::Queued, false, Some(at), at);
        assert_eq!(
            resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US),
            None,
            "still queued: the space is not spent yet"
        );
        // It starts: still settling for a disk sample, then the next one goes.
        let j = w.jobs_mut().get_mut("early").unwrap();
        (j.status, j.started_at) = (Status::Running, Some(NOW_US - 5));
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US), None, "just started: not measured yet");
        let later = NOW_US + settle_micros(&l);
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), later).and_then(resumed).as_deref(), Some("late"));
        // The first ended; nothing settles.
        w.jobs_mut().get_mut("early").unwrap().status = Status::Done;
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US).and_then(resumed).as_deref(), Some("late"));
    }

    /// A held project's neighbour has resumed a job from its own hold, as `unheld_at` says.
    fn settling_neighbour(status: Status) -> World {
        let mut w = held_world(&[("n", status, false, 1)]);
        let j = w.jobs_mut().get_mut("n").unwrap();
        (j.unheld_at, j.queued_at, j.started_at) = (Some(NOW_US - 10), NOW_US - 10, Some(NOW_US - 5));
        w
    }

    #[test]
    fn a_job_another_project_resumed_from_a_hold_settles_for_this_one_too() {
        let l = floor(60.0, 3.0);
        let w = held_world(&[("mine", Status::Stopped, true, 4)]);
        for status in [Status::Queued, Status::Running] {
            let theirs = settling_neighbour(status);
            assert_eq!(
                resume_due(&l, of(&l), &w, &[&theirs], Some(70.0), NOW_US),
                None,
                "{status:?}: the space is not spent yet"
            );
        }
        let theirs = settling_neighbour(Status::Running);
        let later = NOW_US + settle_micros(&l);
        assert!(resume_due(&l, of(&l), &w, &[&theirs], Some(70.0), later).is_some(), "once it has run a sample");
        assert!(
            resume_due(&l, of(&l), &w, &[&settling_neighbour(Status::Done)], Some(70.0), NOW_US).is_some(),
            "it ended"
        );
    }

    #[test]
    fn a_held_job_whose_dependency_is_held_too_waits_for_it() {
        let l = floor(60.0, 3.0);
        // The child was held first (older), the done parent after it was resumed: the parent goes first.
        let mut w = held_world(&[("child", Status::Stopped, true, 3), ("parent", Status::Stopped, true, 8)]);
        w.jobs_mut().get_mut("child").unwrap().after = vec!["parent".into()];
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US).and_then(resumed).as_deref(), Some("parent"));
        // With the parent done, the child is free to go.
        w.jobs_mut().get_mut("parent").unwrap().status = Status::Done;
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US).and_then(resumed).as_deref(), Some("child"));
    }

    #[test]
    fn holds_resume_when_the_floor_is_turned_off_one_at_a_time() {
        let enabled = floor(60.0, 3.0);
        let off = floor(0.0, 3.0);
        let mut w = held_world(&[("a", Status::Stopped, true, 4), ("b", Status::Stopped, true, 9)]);
        assert_eq!(
            resume_due(&enabled, of(&enabled), &w, &[], Some(10.0), NOW_US),
            None,
            "under the floor while it is on"
        );
        for free in [Some(10.0), None] {
            let due = resume_due(&off, of(&off), &w, &[], free, NOW_US).unwrap();
            let (job, message) = (resumed(due.clone()).unwrap(), message(due));
            assert_eq!(job, "a", "the oldest, whatever the disk says: there is no floor");
            assert!(message.contains("turned off") && !message.contains("0 GB floor"), "{message}");
        }
        let j = w.jobs_mut().get_mut("a").unwrap();
        (j.status, j.held, j.unheld_at, j.queued_at) = (Status::Queued, false, Some(NOW_US - 10), NOW_US - 10);
        assert_eq!(resume_due(&off, of(&off), &w, &[], Some(10.0), NOW_US), None, "still one at a time");
    }

    #[test]
    fn an_automatic_requeue_keeps_the_resumed_job_settling() {
        let l = floor(60.0, 3.0);
        let mut w = held_world(&[("early", Status::Stopped, true, 4), ("late", Status::Stopped, true, 9)]);
        let j = w.jobs_mut().get_mut("early").unwrap();
        // Resumed at `at`, started, and sent back to the queue by a retry: `queued_at` moved on, the marker did not.
        (j.status, j.held, j.unheld_at, j.queued_at) = (Status::Queued, false, Some(NOW_US - 10), NOW_US - 5);
        assert_eq!(resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US), None, "queued again: still not started");
    }

    #[test]
    fn a_resumed_job_settles_for_its_own_projects_interval() {
        let l = floor(60.0, 3.0);
        let w = held_world(&[("mine", Status::Stopped, true, 4)]);
        // Another project samples every 30 minutes, we every 3: its job, started 5 minutes ago, still settles.
        let mut theirs = settling_neighbour(Status::Running);
        let (at, started) = (NOW_US - 6 * 60_000_000, NOW_US - 5 * 60_000_000);
        let j = theirs.jobs_mut().get_mut("n").unwrap();
        (j.unheld_at, j.started_at, j.settle_us) = (Some(at), Some(started), 30 * 60_000_000);
        assert_eq!(resume_due(&l, of(&l), &w, &[&theirs], Some(70.0), NOW_US), None);
        let after = started + 30 * 60_000_000;
        assert!(resume_due(&l, of(&l), &w, &[&theirs], Some(70.0), after).is_some(), "its own window has passed");
        // One recorded without a window settles for ours.
        theirs.jobs_mut().get_mut("n").unwrap().settle_us = 0;
        assert!(resume_due(&l, of(&l), &w, &[&theirs], Some(70.0), NOW_US).is_some());
    }

    #[test]
    fn a_resume_never_passes_the_jobs_ceiling() {
        let l = Limits { job_ceiling_usd: Some(10.0), ..floor(60.0, 3.0) };
        let spent = |cost, budget| {
            let mut w = held_world(&[("a", Status::Stopped, true, 4)]);
            let j = w.jobs_mut().get_mut("a").unwrap();
            (j.cost_usd, j.budget_usd) = (cost, budget);
            resume_due(&l, of(&l), &w, &[], Some(70.0), NOW_US)
        };
        let budget = |due| match due {
            Some(Due::Resume { budget_usd, .. }) => Some(budget_usd),
            _ => None,
        };
        assert_eq!(budget(spent(0.0, 10.0)), Some(None), "within the ceiling: its own budget");
        assert_eq!(budget(spent(9.0, 10.0)), Some(Some(1.0)), "capped to what the ceiling leaves");
        let Some(Due::Release { job, why }) = spent(10.0, 10.0) else { panic!("a spent ceiling resumes nothing") };
        assert_eq!(job, "a");
        assert!(why.contains("job_ceiling_usd") && why.contains("queue resume"), "{why}");
        let no_ceiling = Limits { job_ceiling_usd: None, ..l };
        let mut w = held_world(&[("a", Status::Stopped, true, 4)]);
        w.jobs_mut().get_mut("a").unwrap().cost_usd = 99.0;
        assert!(matches!(
            resume_due(&no_ceiling, of(&no_ceiling), &w, &[], Some(70.0), NOW_US),
            Some(Due::Resume { .. })
        ));
    }

    /// A job the project's own `limits.max_running` (or `limits.max_load`) holds is held as any other
    /// is: with a reason, and among the jobs the `held` hook is asked about. It used to be passed
    /// over in silence, so it never reached the hook.
    #[test]
    fn a_job_held_by_max_running_has_a_reason_and_reaches_the_held_hook() {
        let (_d, cfg) = omp_config("max_running = 1\n");
        let jobs = [("a", Status::Running, "minimax-code/M3"), ("b", Status::Queued, "openai-codex/gpt-6.1")];
        let (world, found) = omp_world(&jobs, 0.1, 0.1);
        let (out, held) = plan_omp(&cfg, &world, &found);
        assert_eq!(out, [], "the project runs its one job");
        assert_eq!(held.2, ["b"], "the held hook is asked about it: {:?}", held.0);
        assert_eq!(held.0, ["b: 1 of 1 running in this project (limits.max_running)"]);
    }

    /// The owner's caps with this many compiling jobs at once, and no other.
    fn capped(max_compiling: usize) -> Machine {
        Machine { max_compiling: Some(max_compiling), ..Machine::default() }
    }

    /// One omp account with room ([`omp_world`]) and these jobs of ours, each `(name, status,
    /// read-only, waiting since)`, a running one held by this process as its runner, with the claim
    /// its dispatch recorded.
    fn machine_world(jobs: &[(&str, Status, bool, u64)]) -> (World, Vec<Home>) {
        let (mut world, found) = omp_world(&[], 0.1, 0.1);
        let me = std::process::id();
        for &(name, status, readonly, queued_at) in jobs {
            let running = status == Status::Running;
            let j = Job {
                name: name.into(),
                status,
                kind: if readonly { crate::bus::Kind::Read } else { crate::bus::Kind::Build },
                account: running.then(|| "omp:default".to_owned()),
                runner: running.then(|| (me, os::start_time(me).unwrap())),
                models: BTreeMap::from([("omp".to_owned(), "openai-codex/gpt-6.1".to_owned())]),
                created_at: queued_at,
                queued_at,
                ..Job::default()
            };
            world.claims.extend(j.runner.map(|runner| Claim { job: j.name.clone(), runner, no_compile: false }));
            world.jobs_mut().insert(j.name.clone(), j);
        }
        (world, found)
    }

    /// The room of `world` among `others` under the owner's caps `machine`, with seats for a burst.
    fn room_under(machine: Machine, world: &World, others: &[&World], found: &[Home]) -> Room {
        Room::of(found.to_vec(), world.accounts.clone(), world, others, true).with_machine(machine)
    }

    /// The jobs a pass starts in `room`, in order, and why the rest are held ([`burst_config`]'s limits).
    fn plan_in(mut room: Room, world: &World) -> (Vec<String>, Held) {
        let (_d, cfg) = burst_config();
        let mut held = Held::default();
        let out = plan(&cfg, &mut room, world, NOW_US, &mut held);
        (out.into_iter().map(|s| s.job).collect(), held)
    }

    /// As [`plan_in`], for `world` among `others` under the owner's caps `machine`.
    fn plan_under(machine: Machine, world: &World, others: &[&World], found: &[Home]) -> (Vec<String>, Held) {
        plan_in(room_under(machine, world, others, found), world)
    }

    const CAP: &str = "machine.max_compiling in the owner's policy";

    /// The owner's ruling 1 (revised 2026-11-13): compiling jobs are capped over every project on the
    /// machine, and what compiles is declared, never inferred from the read-only flag. A review
    /// rebuilds its branch and runs its tests, so it counts as a build does; only a job queued as
    /// compiling nothing neither takes a unit nor waits for one.
    #[test]
    fn the_machines_compiling_cap_counts_every_projects_jobs_but_those_that_compile_nothing() {
        let (_d, cfg) = burst_config();
        let jobs = [
            ("build", Status::Queued, false, 5),
            ("review", Status::Queued, true, 6),
            ("design", Status::Queued, true, 7),
        ];
        let (mut world, found) = machine_world(&jobs);
        world.jobs_mut().get_mut("design").unwrap().no_compile = true;
        let (theirs, _) = machine_world(&[("x", Status::Running, false, 1)]);
        let (out, held) = plan_under(capped(1), &world, &[&theirs], &found);
        assert_eq!(out, ["design"], "a job that compiles nothing starts regardless");
        let why = format!("1 of 1 compiling on this machine, over every project ({CAP})");
        assert_eq!(held.0, [format!("build: {why}"), format!("review: {why}")], "the reason names the machine cap");
        assert_eq!(held.2, ["build", "review"], "both reach the held hook");
        assert_eq!(held.3, ["build", "review"], "and both are in line for a unit");
        assert_eq!(held.1, Some(NOW_US + LOOK_AGAIN_MICROS), "a unit a dead runner held comes back with no event");
        let room = room_under(capped(1), &world, &[&theirs], &found);
        for job in ["build", "review"] {
            let hold = room.hold(&cfg, &world.jobs()[job], NOW_US).unwrap();
            assert_eq!(hold.by, Constraint::Pool(Pool::Compiling), "{job}: the binding constraint is named");
        }
        assert_eq!(room.hold(&cfg, &world.jobs()["design"], NOW_US), None);
        assert_eq!(room.taken(&Pool::Compiling), 1);

        // Their one job is a review: read-only, and it holds the unit all the same.
        let (their_review, _) = machine_world(&[("x", Status::Running, true, 1)]);
        assert_eq!(plan_under(capped(1), &world, &[&their_review], &found).0, ["design"]);
        // Their job was queued as compiling nothing: it holds no unit, so our oldest takes it.
        let mut quiet = theirs.clone();
        quiet.claims[0].no_compile = true;
        assert_eq!(plan_under(capped(1), &world, &[&quiet], &found).0, ["build", "design"]);
        // Their runner died: nobody here reaps it, and its unit is free at once.
        let mut dead = theirs.clone();
        dead.claims[0].runner.1 += 1;
        let (out, held) = plan_under(capped(1), &world, &[&dead], &found);
        assert_eq!(out, ["build", "design"], "a dead runner holds no unit");
        assert_eq!(held.3, ["review"], "our own build has the unit now: each start is booked before the next");
        // A job being checked at its cap still has its runner, and its acceptance check to come.
        let mut checking = theirs.clone();
        checking.jobs_mut().get_mut("x").unwrap().status = Status::Checking;
        assert_eq!(plan_under(capped(1), &world, &[&checking], &found).0, ["design"]);

        // With two units, the build and the review both run, and a third compiling job waits.
        assert_eq!(plan_under(capped(2), &world, &[], &found).0.len(), 3);
        assert_eq!(plan_under(capped(2), &world, &[&theirs], &found).0, ["build", "design"]);
        // With no cap in the policy, nothing of this holds anyone: exactly as before.
        assert_eq!(plan_under(Machine::default(), &world, &[&theirs], &found).0.len(), 3);
    }

    /// Copilot review of PR #70, finding 1: a project whose fold could not be read was dropped from
    /// the neighbours in silence, so its compiling jobs were in nobody's count and the cap could be
    /// passed unseen. A room that knows a project is unread admits nothing that compiles while the
    /// owner caps compiling, and the hold names the project.
    #[test]
    fn a_project_that_cannot_be_read_holds_what_compiles_rather_than_being_left_out_of_the_count() {
        let (_d, cfg) = burst_config();
        let (mut world, found) =
            machine_world(&[("build", Status::Queued, false, 5), ("design", Status::Queued, true, 6)]);
        world.jobs_mut().get_mut("design").unwrap().no_compile = true;
        let unread = |machine, names: &[&str]| {
            room_under(machine, &world, &[], &found).with_unread(names.iter().map(|n| (*n).to_owned()).collect())
        };
        let (out, held) = plan_in(unread(capped(2), &["tickify"]), &world);
        assert_eq!(out, ["design"], "nothing is free to count as free; what compiles nothing needs no count");
        let why = format!(
            "build: the compiling jobs on this machine cannot be counted: the state of project tickify cannot be read ({CAP})"
        );
        assert_eq!(held.0, [why]);
        assert_eq!((held.2.clone(), held.3.len()), (vec!["build".to_owned()], 0), "held, for the hook; in no line");
        assert_eq!(held.1, Some(NOW_US + LOOK_AGAIN_MICROS), "looked at again: nothing says when it can be read");
        let hold = unread(capped(2), &["thegrid", "tickify"]).hold(&cfg, &world.jobs()["build"], NOW_US).unwrap();
        assert_eq!(hold.by, Constraint::Uncounted);
        assert!(hold.why.contains("projects thegrid, tickify cannot be read"), "{}", hold.why);
        // Every project read: the count is whole. No cap: there is nothing to count.
        assert_eq!(plan_in(unread(capped(2), &[]), &world).0.len(), 2);
        assert_eq!(plan_in(unread(Machine::default(), &["tickify"]), &world).0.len(), 2);
    }

    /// Copilot review of PR #70, finding 2: the unit followed the job's status. It is the runner
    /// process's claim: held past its job's `ended` and by a runner the fold set aside, while the
    /// process lives, in our project as in another; and never by a status whose runner has died.
    #[test]
    fn a_compiling_unit_is_counted_by_its_live_runner_process_in_every_project() {
        let (world, found) = machine_world(&[("b", Status::Queued, false, 9)]);
        let me = std::process::id();
        let live = (me, os::start_time(me).unwrap());
        let gone = (live.0, live.1 + 1);
        let (theirs, _) = machine_world(&[("x", Status::Running, false, 1)]);
        let with = |edit: &dyn Fn(&mut World)| {
            let mut w = theirs.clone();
            edit(&mut w);
            w
        };
        let x = |w: &mut World, edit: &dyn Fn(&mut Job)| edit(w.jobs_mut().get_mut("x").unwrap());
        // Ended, its runner still at work (the re-review comes after `ended`): the unit is held, and
        // it comes back with no event, so the job is judged again in a moment.
        let ended = with(&|w| x(w, &|x| x.status = Status::Done));
        let (out, held) = plan_under(capped(1), &world, &[&ended], &found);
        assert_eq!(out.len(), 0, "{:?}", held.0);
        assert_eq!(held.1, Some(NOW_US + LINGER_MICROS));
        // Resumed by hand during its check: the fold has no runner for the job, and the check runs on.
        let set_aside = with(&|w| x(w, &|x| (x.status, x.runner) = (Status::Queued, None)));
        assert_eq!(plan_under(capped(1), &world, &[&set_aside], &found).0.len(), 0);
        // The job cleaned away under its live runner: the claim is the fold's, not the job's.
        let cleaned = with(&|w| drop(w.jobs_mut().remove("x")));
        assert_eq!(plan_under(capped(1), &world, &[&cleaned], &found).0.len(), 0);
        // Dispatched again beside the old process: two live processes of one job hold two units, and
        // a dead one none.
        let mut second = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let other = (second.id(), os::start_time(second.id()).unwrap());
        let claim = |runner| Claim { job: "x".into(), runner, no_compile: false };
        let both = with(&|w| w.claims.extend([claim(gone), claim(other)]));
        let units = room_under(capped(4), &world, &[&both], &found).taken(&Pool::Compiling);
        second.kill().unwrap();
        #[allow(clippy::disallowed_methods, reason = "a test reaps the child it killed")]
        second.wait().unwrap();
        assert_eq!(units, 2);
        // Once the process has exited, nothing is held, whatever the status says: in their project,
        let dead = with(&|w| w.claims[0].runner = gone);
        assert_eq!(plan_under(capped(1), &world, &[&dead], &found).0, ["b"]);
        // and in ours, where the count used to follow the status (a board reads before any reap).
        let (mut ours, found) = machine_world(&[("r", Status::Running, false, 1), ("b", Status::Queued, false, 9)]);
        assert_eq!(plan_under(capped(1), &ours, &[], &found).0.len(), 0, "our live runner holds the unit");
        ours.claims[0].runner = gone;
        assert_eq!(plan_under(capped(1), &ours, &[], &found).0, ["b"]);
        ours.jobs_mut().get_mut("r").unwrap().status = Status::Failed;
        ours.claims[0].runner = live;
        assert_eq!(plan_under(capped(1), &ours, &[], &found).0.len(), 0, "ended in our fold, alive on the machine");
    }

    /// Copilot review of PR #70, finding 3: a pass walked the queue by creation while the line across
    /// projects went by how long a job had waited, so a job could be first in one order and behind in
    /// the other. There is one order: since when it has waited.
    #[test]
    fn the_queue_is_walked_in_the_order_the_line_across_projects_is_kept() {
        let (mut world, found) =
            machine_world(&[("early", Status::Queued, false, 1), ("late", Status::Queued, false, 2)]);
        // `early` was created first, and went back into the queue (a retry, a resume) after `late` came.
        world.jobs_mut().get_mut("early").unwrap().queued_at = 9;
        let names: Vec<&str> = queue(&world).iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, ["late", "early"], "the one that has waited longest is judged first");
        let (out, held) = plan_under(capped(1), &world, &[], &found);
        assert_eq!((out, held.3), (vec!["late".to_owned()], vec!["early".to_owned()]), "and takes the one unit");
        // The same key orders the line across projects: theirs has waited since 5, between the two.
        let (mut theirs, _) = machine_world(&[("old", Status::Queued, false, 5)]);
        let me = std::process::id();
        let planner = (me, os::start_time(me).unwrap());
        theirs.jobs_mut().get_mut("old").unwrap().in_line =
            Some(crate::world::InLine { pool: Pool::Compiling.key(), planner });
        let (out, held) = plan_under(capped(2), &world, &[&theirs], &found);
        assert_eq!(out, ["late"], "ahead of theirs in both orders");
        assert!(
            held.0[0].starts_with("early: 1 of 2 compiling") && held.0[0].contains("waited longer"),
            "{:?}",
            held.0
        );
        // A job recorded before queueing times were waits since its creation.
        let old = Job { created_at: 4, ..Job::default() };
        assert_eq!((old.waiting_since(), world.jobs()["early"].waiting_since()), (4, 9));
    }

    /// Older jobs start first across projects: a unit is left for each job of another project that
    /// has waited longer in the line, while the dispatcher that put it there lives.
    #[test]
    fn a_free_compiling_unit_is_left_for_another_projects_job_that_waited_longer() {
        let me = std::process::id();
        let planner = (me, os::start_time(me).unwrap());
        let theirs = |queued_at: u64, planner: (u32, u64)| {
            let (mut w, _) = machine_world(&[("old", Status::Queued, false, queued_at)]);
            let line = crate::world::InLine { pool: Pool::Compiling.key(), planner };
            w.jobs_mut().get_mut("old").unwrap().in_line = Some(line);
            w
        };
        let (world, found) = machine_world(&[("b", Status::Queued, false, 9)]);
        let (out, held) = plan_under(capped(1), &world, &[&theirs(5, planner)], &found);
        assert_eq!(out.len(), 0, "theirs has waited since 5, ours since 9");
        let why = format!(
            "b: 0 of 1 compiling on this machine, over every project, \
             and 1 job of another project has waited longer for the rest ({CAP})"
        );
        assert_eq!(held.0, [why]);
        assert_eq!(held.3, ["b"], "ours is in line too, behind it");
        // Ours is the older one: theirs waits for it, as their dispatcher judges by the same rule.
        assert_eq!(plan_under(capped(1), &world, &[&theirs(12, planner)], &found).0.len(), 1);
        assert_eq!(
            plan_under(capped(1), &world, &[&theirs(9, planner)], &found).0.len(),
            1,
            "equal ages never deadlock"
        );
        // A line nobody tends holds nobody back: their dispatcher is gone, so nothing would start it.
        assert_eq!(plan_under(capped(1), &world, &[&theirs(5, (me, planner.1 + 1))], &found).0.len(), 1);
        // Their job is no longer queued (it started, or was stopped): the record is void.
        let mut started = theirs(5, planner);
        started.jobs_mut().get_mut("old").unwrap().status = Status::Stopped;
        assert_eq!(plan_under(capped(1), &world, &[&started], &found).0.len(), 1);
        // With two units and one of them taken, the one left is the older job's.
        let (busy, _) = machine_world(&[("r", Status::Running, false, 1), ("b", Status::Queued, false, 9)]);
        assert_eq!(plan_under(capped(2), &busy, &[&theirs(5, planner)], &found).0.len(), 0);
        assert_eq!(plan_under(capped(2), &world, &[&theirs(5, planner)], &found).0.len(), 1, "a unit for each");
    }

    /// The owner's ruling 2: no session starts while the machine's available memory is under the
    /// floor, read-only or not, a decision's session included. Unmeasured, or no floor: no gate.
    #[test]
    fn the_memory_gate_holds_every_new_session_under_the_machines_floor() {
        let (_d, cfg) = burst_config();
        let floor = Machine { min_free_mem_gb: Some(Quantity::of(4.0)), ..Machine::default() };
        let (world, found) = machine_world(&[("build", Status::Queued, false, 5), ("read", Status::Queued, true, 6)]);
        let room =
            |machine, mem_gb| room_under(machine, &world, &[], &found).with_gauges(Gauges { load: None, mem_gb });
        let hold = |machine, mem_gb, job: &str| room(machine, mem_gb).hold(&cfg, &world.jobs()[job], NOW_US);
        let low = "3.2 GiB of memory available, under the machine's 4 GiB floor \
                   (machine.min_free_mem_gb in the owner's policy)";
        for job in ["build", "read"] {
            let h = hold(floor, Some(3.21), job).unwrap();
            assert_eq!((h.why.as_str(), h.by.clone()), (low, Constraint::Memory), "{job}");
            assert_eq!(h.until, Some(NOW_US + LOOK_AGAIN_MICROS), "memory coming back is no event");
            assert_eq!(hold(floor, Some(4.0), job), None, "at the floor is room");
            assert_eq!(hold(floor, None, job), None, "memory that cannot be read holds nothing");
        }
        assert_eq!(hold(Machine::default(), Some(0.1), "build"), None, "no floor in the policy: no gate");
        assert_eq!(memory_hold(&floor, Some(3.21)).as_deref(), Some(low), "a check waits by the same judgment");
        assert_eq!(memory_hold(&floor, Some(5.0)), None);

        let (out, held) = plan_in(room(floor, Some(1.0)), &world);
        assert_eq!((out.len(), held.2.len(), held.3.len()), (0, 2, 0), "both held, and reach the held hook");
        // A decision's `agent:` session is a session too.
        let worker = Job { name: "w".into(), ..Job::default() };
        let model = Some("openai-codex/gpt-6.1");
        let seat = |mem_gb| place_session(&cfg, &mut room(floor, mem_gb), &worker, "omp", model, NOW_US, NOW_US);
        assert_eq!(seat(Some(1.0)).unwrap_err().by, Constraint::Memory);
        assert_eq!(seat(Some(8.0)).unwrap().account, "omp:default");
    }

    /// The owner's ruling 3: the disk floor is the machine's, and a queued build job waits for it.
    #[test]
    fn the_disk_gate_holds_a_build_under_the_machines_floor_and_no_read_only_job() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = "name = \"demo\"\n[limits]\nmin_free_disk_gb = 0\nper_account = 16\nmax_running = 20\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        let machine = Machine { min_free_disk_gb: Some(Quantity::of(30.0)), ..Machine::default() };
        let (world, found) = machine_world(&[("build", Status::Queued, false, 5), ("read", Status::Queued, true, 6)]);
        let hold = |ok, job: &str| {
            room_under(machine, &world, &[], &found).with_disk(ok).hold(&cfg, &world.jobs()[job], NOW_US)
        };
        let h = hold(false, "build").unwrap();
        assert_eq!(h.why, "free disk below 33 GB (the machine's 30 GB floor and its margin)");
        assert_eq!((h.by, h.until), (Constraint::Disk, Some(NOW_US + LOOK_AGAIN_MICROS)));
        assert_eq!(hold(false, "read"), None, "a read-only job makes no clone");
        assert_eq!(hold(true, "build"), None);
        let floor = Floor::of(&cfg.limits, &machine);
        assert!(floor.enough(Some(33.0)) && !floor.enough(Some(32.0)), "the machine's floor");
    }

    /// A hold names the constraint that binds: the first that holds the job, in the order `judge`
    /// asks them. A job held by something of its own project is not in line for the machine's pool.
    #[test]
    fn a_hold_names_the_constraint_that_binds_first() {
        let (_d, mut cfg) = burst_config();
        let mut machine =
            Machine { max_compiling: Some(1), min_free_mem_gb: Some(Quantity::of(4.0)), ..Machine::default() };
        cfg.limits.max_load = Some(8.0);
        cfg.profiles.insert("solo".into(), profile(Some(1), None));
        cfg.profiles.insert("daily".into(), profile(None, Some(1)));
        // Everything binds at once: one job of ours runs (a build, of profile `solo`), the load and
        // the memory are past their limits, the disk has no room, and no account has a seat.
        let (mut world, found) = machine_world(&[("r", Status::Running, false, 1), ("q", Status::Queued, false, 5)]);
        world.jobs_mut().get_mut("r").unwrap().profile = Some("solo".into());
        world.profile_starts.insert("daily".into(), vec![NOW_US - 1]);
        let gauges = Gauges { load: Some(9.5), mem_gb: Some(1.0) };
        let by = |cfg: &Config, machine: Machine, profile: Option<&str>, disk_ok| {
            let j = Job { profile: profile.map(Into::into), ..world.jobs()["q"].clone() };
            let room = Room::of(found.clone(), world.accounts.clone(), &world, &[], disk_ok)
                .with_machine(machine)
                .with_gauges(gauges);
            room.hold(cfg, &j, NOW_US).map(|h| (h.by, h.why))
        };
        let pool = |p: Pool| Some(Constraint::Pool(p));
        let first = |cfg: &Config, machine, profile, disk_ok| by(cfg, machine, profile, disk_ok).map(|(by, _)| by);
        cfg.limits.max_running = 1;
        cfg.limits.per_account = 1;
        assert_eq!(first(&cfg, machine, Some("solo"), false), pool(Pool::Profile("solo".into())));
        assert_eq!(first(&cfg, machine, Some("daily"), false), Some(Constraint::PerDay));
        let (running, why) = by(&cfg, machine, None, false).unwrap();
        assert_eq!(running, Constraint::Pool(Pool::Running));
        assert_eq!(why, "1 of 1 running in this project (limits.max_running)");
        cfg.limits.max_running = 20;
        let (load, why) = by(&cfg, machine, None, false).unwrap();
        assert_eq!((load, why.as_str()), (Constraint::Load, "load 9.5 is above 8 (limits.max_load)"));
        cfg.limits.max_load = None;
        assert_eq!(first(&cfg, machine, None, false), Some(Constraint::Memory));
        machine.min_free_mem_gb = None;
        assert_eq!(first(&cfg, machine, None, false), Some(Constraint::Disk));
        assert_eq!(first(&cfg, machine, None, true), Some(Constraint::Accounts));
        cfg.limits.per_account = 16;
        assert_eq!(first(&cfg, machine, None, true), pool(Pool::Compiling), "asked last: it would start but for it");
        assert_eq!(first(&cfg, capped(2), None, true), None);
        assert_eq!(Pool::Compiling.key(), "machine.max_compiling");
        assert_eq!(Pool::Profile("solo".into()).key(), "profiles.solo.max_running");

        // Held by the project's own limit, the job is not in line for the machine's pool.
        cfg.limits.max_running = 1;
        let mut held = Held::default();
        let mut room = room_under(capped(1), &world, &[], &found);
        assert_eq!(plan(&cfg, &mut room, &world, NOW_US, &mut held), []);
        assert_eq!((held.2.clone(), held.3.clone()), (vec!["q".to_owned()], Vec::new()));
    }

    #[test]
    fn admission_waits_for_the_floor_and_its_margin() {
        let l = floor(60.0, 3.0);
        assert!(!of(&l).enough(Some(61.0)), "over the floor is not enough to start");
        assert!(of(&l).enough(Some(63.0)));
        assert!(!of(&l).enough(None), "unmeasured is not enough");
        assert!(of(&floor(0.0, 3.0)).enough(Some(0.5)), "the floor off lets everything start");
        let (_d, cfg) = omp_config("");
        let cfg = Config { limits: Limits { min_free_disk_gb: 60.0.into(), ..cfg.limits }, ..cfg };
        let queued = Job { name: "q".into(), status: Status::Queued, ..Job::default() };
        let w = World::default();
        let hold = |ok| Room::of(Vec::new(), BTreeMap::new(), &w, &[], ok).hold(&cfg, &queued, NOW_US).map(|h| h.why);
        assert!(hold(false).unwrap().contains("free disk below 63 GB"), "{:?}", hold(false));
        assert!(hold(true).is_none_or(|w| !w.contains("free disk")));
    }
}
