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
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, STALE_MICROS, harness_of};
use crate::bus::{DispatchEvent, now_micros};
use crate::ctx::Ctx;
use crate::harness::{Harness, Home};
use crate::os::{self, Doorbell, Role};
use crate::world::{AccountState, Job, Status, World};

/// The longest sleep. Only a runner that dies without a word (one started by
/// an earlier dispatcher, or killed outright) waits this long to be reaped.
const BACKSTOP: Duration = Duration::from_mins(5);
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

/// Why nothing more starts this pass, for the log; printed only when it changes.
#[derive(Debug, Default, PartialEq)]
struct Held(Vec<String>);

fn serve(ctx: &Ctx) -> Result<()> {
    let Some(role) = Role::try_take(&ctx.state.join("dispatch.lock"))? else { return Ok(()) };
    let bell = Doorbell::watch(&[&ctx.bus, &ctx.bus.join("session")])?;
    let version = binary_version();
    let mut idle_since: Option<Instant> = None;
    let mut last_held = Held::default();
    loop {
        if binary_version() != version {
            // A rebuilt binary takes over: drop the role, then start the new one.
            drop(role);
            return ctx.wake_dispatch();
        }
        let (busy, held, due_us) = tick(ctx)?;
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
        let mut sleep = BACKSTOP;
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

fn binary_version() -> Option<SystemTime> {
    crate::os::own_binary().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

/// One pass over the world. Returns whether anything is still queued or
/// running, why jobs are held, and when (bus time) the plan could next change.
fn tick(ctx: &Ctx) -> Result<(bool, Held, Option<u64>)> {
    let _lock = ctx.jobs_lock()?;
    let port = ctx.port(&Source(()));
    let mut world = ctx.world()?;
    for j in world.jobs.values_mut().filter(|j| j.status == Status::Running) {
        if !j.runner.is_some_and(|(pid, started)| os::same_process(pid, started)) {
            port.emit(DispatchEvent::Reaped { job: j.name.clone() })?;
            j.status = Status::Failed;
        }
    }
    let found = accounts::discover(&ctx.cfg);
    refresh_stale(ctx, &world, &found)?;
    let mut held = Held::default();
    let plan = plan(ctx, &world, &found, &mut held);
    for (job, account) in plan {
        let (pid, gate) = ctx.spawn_gated(&["session", "run", &job], &ctx.job_file(&job, "runner.log"))?;
        let started = os::start_time(pid).unwrap_or(0);
        // The runner reads its account from this event, so it waits at the gate until it is written.
        port.emit(DispatchEvent::Dispatched { job, account, runner_pid: pid, runner_started: started })?;
        drop(gate);
    }
    let busy = world.jobs.values().any(|j| j.status.is_live());
    let waiting = world.jobs.values().any(|j| j.status == Status::Queued);
    let states: Vec<_> = found.iter().map(|a| (harness_of(a), world.accounts.get(&a.key()))).collect();
    Ok((busy, held, next_due(&states, waiting, now_micros())))
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

/// Starts a quota reading for accounts whose reading is old, when some job waits.
fn refresh_stale(ctx: &Ctx, world: &World, found: &[Home]) -> Result<()> {
    let waiting = world.jobs.values().any(|j| j.status == Status::Queued);
    let now = now_micros();
    if waiting && found.iter().any(|a| accounts::due(harness_of(a), world.accounts.get(&a.key()), now)) {
        // `quota refresh` holds its own role, so overlapping passes start it once.
        ctx.spawn_self(&["quota", "refresh", "--stale"], &ctx.state.join("quota.log"))?;
    }
    Ok(())
}

/// Which queued jobs start now, and where. Pure apart from reading machine gauges.
fn plan(ctx: &Ctx, world: &World, found: &[Home], held: &mut Held) -> Vec<(String, String)> {
    let limits = &ctx.cfg.limits;
    let now_us = now_micros();
    let mut load: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0;
    for j in world.jobs.values().filter(|j| j.status == Status::Running) {
        *load.entry(j.account.clone().unwrap_or_default()).or_default() += 1;
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
        if !j.readonly && !disk_ok {
            held.0.push(format!("{}: free disk below {} GB", j.name, limits.min_free_disk_gb));
            continue;
        }
        // Every account of every harness the job allows, judged the same way:
        // on the meters its harness says govern the job's model there.
        let mut why = Vec::new();
        let fits: Vec<(&Home, f64)> = found
            .iter()
            .filter(|a| j.harnesses.is_empty() || j.harnesses.iter().any(|h| h == a.harness))
            .filter(|a| j.pinned.as_ref().is_none_or(|p| *p == a.key()))
            .filter(|a| load.get(a.key().as_str()).copied().unwrap_or(0) < limits.per_account)
            .filter_map(|a| {
                let model = j.models.get(a.harness).cloned().or_else(|| ctx.cfg.harness(a.harness).model);
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
            continue;
        };
        let key = a.key();
        total += 1;
        out.push((j.name.clone(), key.clone()));
        *load.entry(key).or_default() += 1;
    }
    out
}

/// Of the accounts that fit, the one a resumable session already ran on, so
/// the provider's prompt cache still holds its transcript (moving it means
/// re-reading all of it at full price); otherwise the best-scoring one.
fn choose<'a>(job: &Job, fits: &[(&'a Home, f64)]) -> Option<&'a Home> {
    let home = job
        .session_id
        .as_ref()
        .and(job.transcript_account.as_deref())
        .and_then(|last| fits.iter().find(|(a, _)| a.key() == last));
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
