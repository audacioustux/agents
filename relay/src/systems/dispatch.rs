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

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, STALE_MICROS, harness_of};
use crate::bus::{DispatchEvent, now_micros};
use crate::config::{Config, Profile};
use crate::ctx::Ctx;
use crate::harness::{Harness, Home};
use crate::os::{self, Role};
use crate::world::{AccountState, DAY_MICROS, Job, Status, Wait, World};

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
    let mut idle_since: Option<Instant> = None;
    let mut last_held = Held::default();
    loop {
        if os::binary_version() != version {
            // A rebuilt binary takes over: drop the role, then start the new one.
            drop(role);
            return ctx.wake_dispatch();
        }
        let (busy, held, due_us) = tick(ctx)?;
        let gc_in = gc_pass(ctx);
        if held != last_held {
            for h in &held.0 {
                eprintln!("{} held: {h}", jiff::Timestamp::now());
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
            eprintln!("{} gc did not start: {e:#}", jiff::Timestamp::now());
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
    for j in world.jobs.values_mut().filter(|j| j.status.has_runner()) {
        if !j.runner.is_some_and(|(pid, started)| os::same_process(pid, started)) {
            port.emit(DispatchEvent::Reaped { job: j.name.clone() })?;
            j.status = Status::Failed;
        }
    }
    // Accounts belong to the machine: what other projects learned of them, and
    // the seats their sessions hold, count here too. Their jobs stay theirs.
    let neighbours = ctx.neighbours();
    world.hear_accounts(&neighbours);
    let elsewhere = seats_taken(&neighbours);
    let found = accounts::discover(&ctx.cfg);
    accounts::settle(&mut world, &found);
    refresh_stale(ctx, &world, &found)?;
    let mut held = Held::default();
    let plan = plan(ctx, &world, &elsewhere, &found, now_micros(), &mut held);
    for (job, account) in plan {
        let (pid, gate) = ctx.spawn_gated(&["session", "run", &job], &ctx.job_file(&job, "runner.log"))?;
        let started = os::start_time(pid).unwrap_or(0);
        // The runner reads its account from this event, so it waits at the gate until it is written.
        port.emit(DispatchEvent::Dispatched { job, account, runner_pid: pid, runner_started: started })?;
        drop(gate);
    }
    // A job blocked on a dependency that ended badly waits for the operator, not for the dispatcher.
    let held_due = ask_held(ctx, &world, &held)?;
    let busy =
        world.jobs.values().any(|j| j.status == Status::Running || j.status == Status::Queued && !world.blocked(j));
    let waiting = world.jobs.values().any(|j| world.startable(j));
    let states: Vec<_> = found.iter().map(|a| (harness_of(a), world.accounts.get(&a.key()))).collect();
    let due = next_due(&states, waiting, now_micros());
    Ok((busy, Held(held.0, None, Vec::new()), [due, held.1, held_due].into_iter().flatten().min()))
}

/// Starts the `held` chain for each job in `held` that has waited past
/// `hooks.held_after_minutes` and was not asked about since it was queued;
/// returns when (bus time) the next one will have waited that long.
fn ask_held(ctx: &Ctx, world: &World, held: &Held) -> Result<Option<u64>> {
    if ctx.cfg.hooks.held.is_none() {
        return Ok(None);
    }
    let after = Duration::from_secs_f64(ctx.cfg.hooks.held_after_minutes * 60.0);
    let after = u64::try_from(after.as_micros()).unwrap_or(u64::MAX);
    let now = now_micros();
    let mut next = None;
    for j in held.2.iter().filter_map(|n| world.jobs.get(n)) {
        if crate::systems::decide::asked_while_held(world, j) {
            continue;
        }
        let at = j.queued_at.saturating_add(after);
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
/// `refresh_stale` starts a new one). `None` if nothing is pending.
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

/// Sessions running on each account in `worlds`, counting only jobs their
/// fold says run and whose runner is still alive.
pub fn seats_taken(worlds: &[World]) -> BTreeMap<String, usize> {
    let mut seats = BTreeMap::new();
    for j in worlds.iter().flat_map(|w| w.jobs.values()) {
        if j.status == Status::Running
            && let (Some(account), Some((pid, started))) = (&j.account, j.runner)
            && os::same_process(pid, started)
        {
            *seats.entry(account.clone()).or_default() += 1;
        }
    }
    seats
}

/// Starts a quota reading for accounts whose reading is old and that a
/// waiting job may run on: `claude /usage` is not free to call, and a job
/// pinned to one harness says nothing about another's accounts.
fn refresh_stale(ctx: &Ctx, world: &World, found: &[Home]) -> Result<()> {
    let now = now_micros();
    if found.iter().any(|a| {
        crate::systems::quota::wanted(world, a) && accounts::due(harness_of(a), world.accounts.get(&a.key()), now)
    }) {
        // `quota refresh` holds its own role, so overlapping passes start it once.
        ctx.spawn_self(&["quota", "refresh", "--stale", "--wanted"], &ctx.state.join("quota.log"))?;
    }
    Ok(())
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
        .map_or_else(String::new, |t| format!(", next slot {}", t.strftime("%H:%MZ")));
    Some((format!("profile {name}: {count} started in 24h (max {max}){at}"), Some(slot)))
}

/// Which queued jobs start now, and where. Pure apart from reading machine
/// gauges. `elsewhere` is the seats other projects hold: they count against
/// `limits.per_account`, but not against this project's `max_running`. `now_us`
/// is bus time, so a profile's daily window can be judged at any moment.
fn plan(
    ctx: &Ctx,
    world: &World,
    elsewhere: &BTreeMap<String, usize>,
    found: &[Home],
    now_us: u64,
    held: &mut Held,
) -> Vec<(String, String)> {
    let limits = &ctx.cfg.limits;
    let mut running: BTreeMap<&str, usize> = BTreeMap::new();
    let mut starts = world.profile_starts.clone();
    for p in world.jobs.values().filter(|j| j.status.has_runner()).filter_map(|j| j.profile.as_deref()) {
        *running.entry(p).or_default() += 1;
    }
    // Homes signed in as one account share a seat budget, held under the first one's key.
    let twins = accounts::twins(found);
    let seat = |key: &str| accounts::seat(&twins, key).to_owned();
    let mut load: BTreeMap<String, usize> = BTreeMap::new();
    for (key, n) in elsewhere {
        *load.entry(seat(key)).or_default() += n;
    }
    let mut total = 0;
    for j in world.jobs.values().filter(|j| j.status == Status::Running) {
        *load.entry(seat(j.account.as_deref().unwrap_or_default())).or_default() += 1;
        total += 1;
    }
    if let (Some(max), Some(now)) = (limits.max_load, os::load_average())
        && now > max
    {
        held.0.push(format!("load {now:.1} is above {max}"));
        return Vec::new();
    }
    // Unmeasured is not the same as enough.
    let disk_ok =
        limits.min_free_disk_gb <= 0.0 || os::free_disk_gb(&ctx.clones).is_some_and(|gb| gb >= limits.min_free_disk_gb);
    let mut queued: Vec<&Job> = world.jobs.values().filter(|j| j.status == Status::Queued).collect();
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
        let profile = j.profile.as_deref().and_then(|n| ctx.cfg.profiles.get(n).map(|p| (n, p)));
        if let Some((n, p)) = profile.filter(|_| !j.forced)
            && let Some((why, until)) = profile_hold(
                n,
                p,
                running.get(n).copied().unwrap_or(0),
                starts.get(n).map_or(&[], Vec::as_slice),
                now_us,
            )
        {
            held.0.push(format!("{}: {why}", j.name));
            held.1 = [held.1, until].into_iter().flatten().min();
            held.2.push(j.name.clone());
            continue;
        }
        if !j.readonly && !disk_ok {
            held.0.push(format!("{}: free disk below {} GB", j.name, limits.min_free_disk_gb));
            held.2.push(j.name.clone());
            continue;
        }
        // Every account of every harness the job allows, judged the same way:
        // on the meters its harness says govern the job's model there.
        let mut why = Vec::new();
        let fits: Vec<(&Home, f64)> = found
            .iter()
            .filter(|a| may_use(j, a))
            .filter(|a| load.get(&seat(&a.key())).copied().unwrap_or(0) < limits.per_account)
            .filter_map(|a| {
                let model = model_for(&ctx.cfg, j, a.harness);
                match accounts::fit(a, world.accounts.get(&a.key()), model.as_deref(), limits.floor, now_us) {
                    Ok(score) => Some((a, score)),
                    Err(r) => {
                        why.push(format!("{} {}", a.key(), r.describe()));
                        None
                    }
                }
            })
            .collect();
        let Some(a) = choose(j, &fits) else {
            held.0.push(format!("{}: no account has room ({})", j.name, why.join("; ")));
            held.2.push(j.name.clone());
            continue;
        };
        let key = a.key();
        total += 1;
        if let Some((n, _)) = profile {
            *running.entry(n).or_default() += 1;
            starts.entry(n.to_owned()).or_default().push(now_us);
        }
        out.push((j.name.clone(), key.clone()));
        *load.entry(seat(&key)).or_default() += 1;
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
    use std::path::PathBuf;

    use super::*;
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
        assert_eq!(why, "profile bug-hunt: 3 started in 24h (max 3), next slot 13:00Z");
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
    fn only_live_runners_hold_seats() {
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
            running("a", "c:x", Some((me, started))),
            running("b", "c:x", Some((me, started + 1))),
            running("c", "c:y", None),
            Job { status: Status::Done, ..running("d", "c:y", Some((me, started))) },
        ] {
            one.jobs.insert(j.name.clone(), j);
        }
        let mut two = World::default();
        two.jobs.insert("a".into(), running("a", "c:x", Some((me, started))));
        let seats = seats_taken(&[one, two]);
        assert_eq!(seats, BTreeMap::from([("c:x".to_owned(), 2)]), "a dead runner or a finished job holds none");
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
}
