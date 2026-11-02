//! quota: how much room each account has, for every harness alike, and the
//! owner's switches. Emits: account-enabled, reading. Readings come from each
//! harness's quota command (no model call) and from what sessions stream.

use std::process::{Command, Stdio};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{QuotaEvent, now_micros, secs};
use crate::ctx::Ctx;
use crate::harness::{Harness, Home, Meter, Meters};
use crate::os::Role;
use crate::sandbox;
use crate::systems::dispatch::seats_taken;
use crate::world::{Status, World};

/// The capability to write quota's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> std::path::PathBuf {
        crate::bus::system_file("quota")
    }
}

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
        fail!(NotFound, "unknown account {account}; accounts are harness:label, see: relay quota show");
    }
    ctx.port(&Source(())).emit(QuotaEvent::AccountEnabled { account, enabled })?;
    ctx.wake_dispatch()?;
    show(ctx, &ctx.world()?)
}

fn refresh(ctx: &Ctx, only_stale: bool) -> Result<()> {
    // One reader at a time: overlapping requests from dispatch passes collapse into this one.
    let Some(_role) = Role::try_take(&ctx.state.join("quota.lock"))? else {
        return ctx.reply(&serde_json::json!({ "skipped": "a reading is already being taken" }), "");
    };
    // A reading another project took recently is as good as one of ours.
    let mut world = ctx.world()?;
    world.hear_accounts(&ctx.neighbours());
    let now = now_micros();
    let todo: Vec<Home> = accounts::discover(&ctx.cfg)
        .into_iter()
        .filter(|a| harness_of(a).caps().quota)
        .filter(|a| !only_stale || accounts::due(harness_of(a), world.accounts.get(&a.key()), now))
        .collect();
    let port = ctx.port(&Source(()));
    // In parallel: one reading can take a minute and a half.
    let readings: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = todo.iter().map(|a| s.spawn(move || (a, read(ctx, a)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let now_s = secs(now);
    let (mut read, mut unreadable) = (Vec::new(), Vec::new());
    for (home, text) in readings {
        let meters = text.map(|t| harness_of(home).parse_quota(&t, now_s)).unwrap_or_default();
        // An unreadable account stays unread, which is not the same as having room.
        let account = home.key();
        if meters.is_empty() {
            unreadable.push(account.clone());
            port.emit(QuotaEvent::Unreadable { account })?;
        } else {
            read.push(account.clone());
            port.emit(QuotaEvent::Reading { account, meters })?;
        }
    }
    let text = if unreadable.is_empty() { String::new() } else { format!("no reading: {}", unreadable.join(", ")) };
    ctx.reply(&serde_json::json!({ "read": read, "unreadable": unreadable }), &text)
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

/// One account, as `show` reports it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Row<'a> {
    account: String,
    state: String,
    running: usize,
    meters: Option<&'a Meters>,
    read_at: Option<u64>,
    /// The meter closest to its limit among those that govern the model.
    #[serde(skip)]
    tightest: Option<(&'a str, &'a Meter)>,
    #[serde(skip)]
    ready: bool,
}

impl Row<'_> {
    /// Its cells for a table: what an operator scans first comes first, and
    /// only the meter that decides (`--json` has them all).
    fn cells(&self) -> Vec<String> {
        let tightest = self.tightest.map_or(String::new(), |(k, m)| {
            let at = m.resets_at.map_or(String::new(), |r| format!(", resets {}", fmt_time(r)));
            format!("{:.0}% {k}{at}", m.utilization * 100.0)
        });
        let running = if self.running > 0 { format!("{} running", self.running) } else { String::new() };
        vec![self.account.clone(), self.state.clone(), running, tightest]
    }
}

/// Of the meters that govern `model`, the one closest to its limit.
fn tightest<'m>(h: &dyn Harness, model: Option<&str>, meters: &'m Meters, now_s: i64) -> Option<(&'m str, &'m Meter)> {
    let governing = h.meters_for(model, meters).ok()?;
    meters
        .iter()
        .filter(|&(_, m)| governing.iter().any(|g| std::ptr::eq(*g, m)) && m.resets_at.is_none_or(|r| r > now_s))
        .max_by(|a, b| a.1.utilization.total_cmp(&b.1.utilization))
        .map(|(k, m)| (k.as_str(), m))
}

pub fn show(ctx: &Ctx, world: &World) -> Result<()> {
    let v = accounts_view(ctx, world);
    ctx.reply(&v.json, &v.text())
}

/// Every account's room: as every project on this machine knows it, with
/// the sessions all of them run there.
pub struct AccountsView {
    pub json: serde_json::Value,
    pub head: String,
    /// One row of table cells per account.
    pub cells: Vec<Vec<String>>,
    /// Whether each account would take a job now.
    pub ready: Vec<bool>,
}

impl AccountsView {
    pub fn text(&self) -> String {
        let lines = crate::systems::view::table(&self.cells).into_iter().map(|l| format!("  {l}"));
        std::iter::once(self.head.clone()).chain(lines).collect::<Vec<_>>().join("\n")
    }
}

pub fn accounts_view(ctx: &Ctx, world: &World) -> AccountsView {
    let now = now_micros();
    let neighbours = ctx.neighbours();
    let mut world = world.clone();
    world.hear_accounts(&neighbours);
    let elsewhere = seats_taken(&neighbours);
    let row = |a: &Home| {
        let (key, h) = (a.key(), harness_of(a));
        let st = world.accounts.get(&key);
        // The dispatcher's own judgment, for the harness's configured model.
        let model = ctx.cfg.harness(a.harness).model;
        let fit = accounts::fit(a, st, model.as_deref(), ctx.cfg.limits.floor, now);
        let ready = fit.is_ok();
        let state = match fit {
            Ok(_) if !h.caps().quota => "ready (unmetered)".to_owned(),
            Ok(_) => "ready".to_owned(),
            Err(accounts::Refusal::Limited) => {
                format!("limited until {}", fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
            }
            Err(r) => r.describe(),
        };
        let running = world.jobs.values().filter(|j| j.status == Status::Running && j.account.as_ref() == Some(&key));
        Row {
            running: running.count() + elsewhere.get(&key).copied().unwrap_or(0),
            account: key,
            state,
            meters: st.map(|s| &s.meters),
            read_at: st.and_then(|s| s.read_at),
            tightest: st.and_then(|s| tightest(h, model.as_deref(), &s.meters, secs(now))),
            ready,
        }
    };
    let rows: Vec<Row<'_>> = accounts::discover(&ctx.cfg).iter().map(row).collect();
    let names: Vec<_> = ctx.cfg.harnesses().iter().map(|h| h.id()).collect();
    let l = &ctx.cfg.limits;
    let head =
        format!("accounts on {} (floor {:.0}%, {} session(s) each)", names.join(", "), l.floor * 100.0, l.per_account);
    let cells = rows.iter().map(Row::cells).collect();
    let ready = rows.iter().map(|r| r.ready).collect();
    AccountsView { json: serde_json::json!({ "harnesses": names, "accounts": rows }), head, cells, ready }
}

pub fn fmt_time(unix_s: i64) -> String {
    jiff::Timestamp::from_second(unix_s).map_or_else(|_| "?".into(), |t| t.strftime("%b %-d %H:%MZ").to_string())
}
