//! quota: knows how much room each account has, and lets the owner switch
//! accounts off and on. Emits: account-enabled, reading.
//! Readings come from `claude -p /usage`, which costs no model call, and from
//! the `rate_limit_event`s sessions report.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::accounts::{self, Account};
use crate::bus::{QuotaEvent, now_micros};
use crate::ctx::Ctx;
use crate::os::Role;
use crate::sandbox;
use crate::usage::parse_usage;
use crate::world::World;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Each account's room, best first.
    Show,
    /// Read every account's quota now (or only stale readings).
    Refresh {
        #[arg(long)]
        stale: bool,
    },
    /// Let relay use an account again, for example after signing it back in.
    Enable { account: String },
    /// Stop relay from starting sessions on an account.
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
    if !accounts::discover(&ctx.cfg).iter().any(|a| a.label == account) {
        bail!("unknown account {account}");
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
    let todo: Vec<Account> = accounts::discover(&ctx.cfg)
        .into_iter()
        .filter(|a| !only_stale || accounts::is_stale(world.accounts.get(&a.label), now))
        .collect();
    let port = ctx.port::<QuotaEvent>();
    // In parallel: one reading can take a minute and a half.
    let readings: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = todo.iter().map(|a| s.spawn(move || (a.label.clone(), read_usage(ctx, a)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let now_s = i64::try_from(now / 1_000_000).unwrap_or(i64::MAX);
    for (label, text) in readings {
        let windows = text.map(|t| parse_usage(&t, now_s)).unwrap_or_default();
        // An unreadable account stays unread, which is not the same as having room.
        if windows.is_empty() {
            eprintln!("{label}: no reading");
        } else {
            port.emit(QuotaEvent::Reading { account: label, windows })?;
        }
    }
    Ok(())
}

fn read_usage(ctx: &Ctx, a: &Account) -> Option<String> {
    let cmd = ctx.cfg.session.command.first()?;
    let mut child = Command::new("timeout")
        .args(["90", cmd, "-p", "/usage", "--strict-mcp-config", "--setting-sources", "project,local"])
        .env_clear()
        .envs(sandbox::plain_env(ctx, &a.dir))
        .current_dir(&ctx.state)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let out = std::io::read_to_string(child.stdout.take()?).ok()?;
    let _ = child.wait();
    Some(out)
}

pub fn show(ctx: &Ctx, world: &World) -> Result<()> {
    let limits = &ctx.cfg.limits;
    let now = now_micros();
    let now_s = i64::try_from(now / 1_000_000).unwrap_or(i64::MAX);
    out!("accounts (floor {:.0}%, {} session(s) each)", limits.floor * 100.0, limits.per_account);
    let mut found = accounts::discover(&ctx.cfg);
    found.sort_by(|a, b| {
        let s = |x: &Account| world.accounts.get(&x.label).map_or(-1.0, |st| accounts::score(&st.windows, now_s));
        s(b).total_cmp(&s(a))
    });
    for a in found {
        let st = world.accounts.get(&a.label);
        let running = world
            .jobs
            .values()
            .filter(|j| j.status == crate::world::Status::Running && j.account.as_deref() == Some(&a.label))
            .count();
        let state = match accounts::refusal(st, limits.floor, now_s) {
            None => "ready".to_owned(),
            Some(accounts::Refusal::Limited) => {
                format!("limited until {}", fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
            }
            Some(r) => r.as_str().to_owned(),
        };
        let windows = st
            .map(|s| {
                s.windows
                    .iter()
                    .map(|(k, w)| {
                        format!(
                            "{k} {:.0}%{}",
                            w.utilization * 100.0,
                            w.resets_at.map_or(String::new(), |r| format!(" (resets {})", fmt_time(r)))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let age = st
            .and_then(|s| s.read_at)
            .map_or(String::new(), |t| format!("  read {} ago", crate::systems::view::ago(now, t)));
        out!("  {:<20} {:<34} running {running}  {windows}{age}", a.label, state);
    }
    Ok(())
}

pub fn fmt_time(unix_s: i64) -> String {
    jiff::Timestamp::from_second(unix_s).map_or_else(|_| "?".into(), |t| t.strftime("%b %-d %H:%MZ").to_string())
}
