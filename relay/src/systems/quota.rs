//! quota: how much room each account has, for every harness alike, and the
//! owner's switches. Emits: account-enabled, reading. Readings come from each
//! harness's quota command (no model call) and from what sessions stream.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{QuotaEvent, now_micros, secs};
use crate::ctx::Ctx;
use crate::harness::Home;
use crate::os::Role;
use crate::sandbox;
use crate::world::World;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Each account's room.
    Show,
    /// Read every account's quota now (or only stale readings).
    Refresh {
        #[arg(long)]
        stale: bool,
    },
    /// Let relay use an account (`harness:label`) again, for example after signing it back in.
    Enable { account: String },
    /// Stop relay from starting sessions on an account (`harness:label`).
    Disable { account: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show => show(ctx, &ctx.world()?),
        Cmd::Refresh { stale } => refresh(ctx, stale),
        Cmd::Enable { account } => toggle(ctx, account, true),
        Cmd::Disable { account } => toggle(ctx, account, false),
    }
}

fn toggle(ctx: &Ctx, account: String, enabled: bool) -> Result<()> {
    if !accounts::discover(&ctx.cfg).iter().any(|a| a.key() == account) {
        bail!("unknown account {account}; accounts are harness:label, see: relay quota show");
    }
    ctx.port::<QuotaEvent>().emit(QuotaEvent::AccountEnabled { account, enabled })?;
    ctx.wake_dispatch()?;
    show(ctx, &ctx.world()?)
}

fn refresh(ctx: &Ctx, only_stale: bool) -> Result<()> {
    // One reader at a time: overlapping requests from dispatch passes collapse into this one.
    let Some(_role) = Role::try_take(&ctx.state.join("quota.lock"))? else { return Ok(()) };
    let world = ctx.world()?;
    let now = now_micros();
    let todo: Vec<Home> = accounts::discover(&ctx.cfg)
        .into_iter()
        .filter(|a| harness_of(a).caps().quota)
        .filter(|a| !only_stale || accounts::due(harness_of(a), world.accounts.get(&a.key()), now))
        .collect();
    let port = ctx.port::<QuotaEvent>();
    // In parallel: one reading can take a minute and a half.
    let readings: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = todo.iter().map(|a| s.spawn(move || (a, read(ctx, a)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let now_s = secs(now);
    for (home, text) in readings {
        let meters = text.map(|t| harness_of(home).parse_quota(&t, now_s)).unwrap_or_default();
        // An unreadable account stays unread, which is not the same as having room.
        let account = home.key();
        if meters.is_empty() {
            eprintln!("{account}: no reading");
            port.emit(QuotaEvent::Unreadable { account })?;
        } else {
            port.emit(QuotaEvent::Reading { account, meters })?;
        }
    }
    Ok(())
}

fn read(ctx: &Ctx, home: &Home) -> Option<String> {
    let inv = ctx.cfg.harness(home.harness).locate(harness_of(home).quota(home)?);
    let out = Command::new("timeout")
        .arg("90")
        .arg(&inv.program)
        .args(&inv.args)
        .env_clear()
        .envs(sandbox::session_env(ctx, &inv))
        .current_dir(&ctx.state)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn show(ctx: &Ctx, world: &World) -> Result<()> {
    let limits = &ctx.cfg.limits;
    let now = now_micros();
    let now_s = secs(now);
    let names: Vec<_> = ctx.cfg.harnesses().iter().map(|h| h.id()).collect();
    out!(
        "accounts on {} (floor {:.0}%, {} session(s) each)",
        names.join(", "),
        limits.floor * 100.0,
        limits.per_account
    );
    for a in accounts::discover(&ctx.cfg) {
        let key = a.key();
        let st = world.accounts.get(&key);
        let h = harness_of(&a);
        let running = world
            .jobs
            .values()
            .filter(|j| j.status == crate::world::Status::Running && j.account.as_deref() == Some(&key))
            .count();
        let state = match accounts::account_refusal(h, st, now_s) {
            None if !h.caps().quota => "ready (unmetered)".to_owned(),
            None => "ready".to_owned(),
            Some(accounts::Refusal::Limited) => {
                format!("limited until {}", fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
            }
            Some(r) => r.describe(),
        };
        let meters = st
            .map(|s| {
                s.meters
                    .iter()
                    .map(|(k, m)| {
                        let at = m.resets_at.map_or(String::new(), |r| format!(" (resets {})", fmt_time(r)));
                        format!("{k} {:.0}%{at}", m.utilization * 100.0)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let age = st
            .and_then(|s| s.read_at)
            .map_or(String::new(), |t| format!("  read {} ago", crate::systems::view::ago(now, t)));
        out!("  {key:<28} {state:<34} running {running}  {meters}{age}");
    }
    Ok(())
}

pub fn fmt_time(unix_s: i64) -> String {
    jiff::Timestamp::from_second(unix_s).map_or_else(|_| "?".into(), |t| t.strftime("%b %-d %H:%MZ").to_string())
}
