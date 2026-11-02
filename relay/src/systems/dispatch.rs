//! dispatch: gives queued jobs a slot on the account with the most room, and
//! reaps sessions whose runner died. Emits: dispatched, reaped.
//!
//! It is the only system that acts on its own. `serve` takes the dispatcher
//! role with flock (a second one exits at once), wakes on the bus doorbell,
//! re-reads every 30 s as the lossless backstop (dead runners, resets,
//! staleness), and exits after a minute with nothing queued or running.
//! Every system that emits wakes it, so it never has to be started by hand.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{DispatchEvent, now_micros};
use crate::ctx::Ctx;
use crate::harness::Home;
use crate::os::{self, Doorbell, Role};
use crate::world::{Job, Status, World};

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
    let bell = Doorbell::watch(&ctx.bus)?;
    let version = binary_version();
    let mut idle_since: Option<Instant> = None;
    let mut last_held = Held::default();
    loop {
        if binary_version() != version {
            // A rebuilt binary takes over: drop the role, then start the new one.
            drop(role);
            return ctx.wake_dispatch();
        }
        let (busy, held) = tick(ctx)?;
        if held != last_held {
            for h in &held.0 {
                eprintln!("{} held: {h}", jiff::Timestamp::now());
            }
            last_held = held;
        }
        idle_since = if busy { None } else { Some(idle_since.unwrap_or_else(Instant::now)) };
        if idle_since.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            return Ok(());
        }
        if bell.wait(Duration::from_secs(30)) {
            // Let a burst of events land, then fold them in one pass.
            std::thread::sleep(Duration::from_millis(200));
            bell.wait(Duration::ZERO);
        }
    }
}

fn binary_version() -> Option<SystemTime> {
    std::env::current_exe().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

/// One pass over the world. Returns whether anything is still queued or running.
fn tick(ctx: &Ctx) -> Result<(bool, Held)> {
    let _lock = ctx.jobs_lock()?;
    let port = ctx.port::<DispatchEvent>();
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
    Ok((busy, held))
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
        let pick = found
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
            .max_by(|(_, x), (_, y)| x.total_cmp(y));
        let Some((a, _)) = pick else {
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
