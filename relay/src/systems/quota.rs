//! quota: how much room each account has, for every harness alike, and the
//! owner's switches. Emits: account-enabled, reading. Readings come from each
//! harness's quota command (no model call) and from what sessions stream.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{QuotaEvent, now_micros, secs};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::harness::{Home, Meter, Meters};
use crate::os::Role;
use crate::sandbox;
use crate::systems::dispatch::{may_use, model_for, seats_taken};
use crate::world::{AccountState, Status, World};

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
    /// The account judged for each other model a queued or running job asks of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_model: Vec<Judged<'a>>,
}

/// An account judged for one model other than its harness's configured one.
#[derive(serde::Serialize)]
struct Judged<'a> {
    model: String,
    state: String,
    #[serde(skip)]
    tightest: Option<(&'a str, &'a Meter)>,
    #[serde(skip)]
    ready: bool,
}

impl Row<'_> {
    /// Its table row, then a sub-row per other model, each with whether it
    /// would take a job: what an operator scans first comes first, and only
    /// the meter that decides (`--json` has them all).
    fn cells(&self) -> impl Iterator<Item = (Vec<String>, bool)> {
        let line = |t: Option<(&str, &Meter)>| t.map_or(String::new(), |(k, m)| accounts::meter_line(k, m));
        let running = if self.running > 0 { format!("{} running", self.running) } else { String::new() };
        let own = vec![self.account.clone(), self.state.clone(), running, line(self.tightest)];
        let others = self.by_model.iter().map(move |j| {
            (vec![format!("  for {}", j.model), j.state.clone(), String::new(), line(j.tightest)], j.ready)
        });
        std::iter::once((own, self.ready)).chain(others)
    }
}

/// How an account stands for `model`: its state cell, the meter that decides, and whether it would take the job.
fn judge<'w>(
    a: &Home,
    st: Option<&'w AccountState>,
    model: Option<&str>,
    floor: f64,
    now: u64,
) -> (String, Option<(&'w str, &'w Meter)>, bool) {
    let h = harness_of(a);
    let fit = accounts::fit(a, st, model, floor, now);
    let ready = fit.is_ok();
    let state = match fit {
        Ok(_) if !h.caps().quota => "ready (unmetered)".to_owned(),
        Ok(_) => "ready".to_owned(),
        Err(accounts::Refusal::Limited) => {
            format!("limited until {}", accounts::fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
        }
        // The meter cell names the meter.
        Err(accounts::Refusal::AtFloor(_)) => "at floor".to_owned(),
        Err(r) => r.describe(),
    };
    let tightest = st.and_then(|s| {
        let governing = h.meters_for(model, &s.meters).ok()?;
        accounts::tightest(&governing, &s.meters, secs(now))
    });
    (state, tightest, ready)
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
    /// One row of table cells per account, each followed by a sub-row per
    /// other model its jobs ask for.
    pub cells: Vec<Vec<String>>,
    /// Whether each row's account would take a job (on that row's model) now.
    pub ready: Vec<bool>,
}

impl AccountsView {
    pub fn text(&self) -> String {
        let lines = crate::systems::view::table(&self.cells).into_iter().map(|l| format!("  {l}"));
        std::iter::once(self.head.clone()).chain(lines).collect::<Vec<_>>().join("\n")
    }
}

pub fn accounts_view(ctx: &Ctx, world: &World) -> AccountsView {
    let neighbours = ctx.neighbours();
    let mut world = world.clone();
    world.hear_accounts(&neighbours);
    view(&ctx.cfg, &accounts::discover(&ctx.cfg), &world, &seats_taken(&neighbours), now_micros())
}

/// The view of `found` in `world` (which has heard its neighbours), with the
/// seats other projects hold (`elsewhere`), at `now` (bus time).
fn view(cfg: &Config, found: &[Home], world: &World, elsewhere: &BTreeMap<String, usize>, now: u64) -> AccountsView {
    let floor = cfg.limits.floor;
    let row = |a: &Home| {
        let key = a.key();
        let st = world.accounts.get(&key);
        // The dispatcher's own judgment, for the configured model and every other one its jobs ask for.
        let model = cfg.harness(a.harness).model;
        let (state, tightest, ready) = judge(a, st, model.as_deref(), floor, now);
        let asked: BTreeSet<String> = world
            .jobs
            .values()
            .filter(|j| match j.status {
                Status::Queued => may_use(j, a),
                Status::Running => j.account.as_ref() == Some(&key),
                _ => false,
            })
            .filter_map(|j| model_for(cfg, j, a.harness))
            .filter(|m| Some(m) != model.as_ref())
            .collect();
        let by_model = asked
            .into_iter()
            .map(|m| {
                let (state, tightest, ready) = judge(a, st, Some(&m), floor, now);
                Judged { model: m, state, tightest, ready }
            })
            .collect();
        let running = world.jobs.values().filter(|j| j.status == Status::Running && j.account.as_ref() == Some(&key));
        Row {
            running: running.count() + elsewhere.get(&key).copied().unwrap_or(0),
            account: key,
            state,
            meters: st.map(|s| &s.meters),
            read_at: st.and_then(|s| s.read_at),
            tightest,
            ready,
            by_model,
        }
    };
    let rows: Vec<Row<'_>> = found.iter().map(row).collect();
    let names: Vec<_> = cfg.harnesses().iter().map(|h| h.id()).collect();
    let l = &cfg.limits;
    let head =
        format!("accounts on {} (floor {:.0}%, {} session(s) each)", names.join(", "), l.floor * 100.0, l.per_account);
    let (cells, ready) = rows.iter().flat_map(Row::cells).unzip();
    AccountsView { json: serde_json::json!({ "harnesses": names, "accounts": rows }), head, cells, ready }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::tempdir;
    use crate::world::Job;

    const NOW: u64 = 1_800_000_000 * 1_000_000;

    /// A config whose omp harness runs `model`.
    fn cfg(model: &str) -> (tempdir::Dir, Config) {
        let d = tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = format!("name = \"demo\"\n[harness.omp]\nmodel = \"{model}\"\n");
        let cfg = Config::parse(&text, d.path()).unwrap();
        (d, cfg)
    }

    fn omp() -> Home {
        Home { harness: "omp", label: "default".into(), dir: PathBuf::from("/o") }
    }

    /// Minimax has room; openai-codex is spent until 22:02Z.
    fn world(jobs: &[Job]) -> World {
        let mut st = AccountState { read_at: Some(NOW - 1), ..Default::default() };
        let now_s = secs(NOW);
        st.meters.insert("minimax-code/General 7 Day".into(), Meter { utilization: 0.04, resets_at: None });
        st.meters.insert("openai-codex/5 hours".into(), Meter { utilization: 1.0, resets_at: Some(now_s + 3_600) });
        let mut w = World::default();
        w.accounts.insert(omp().key(), st);
        for j in jobs {
            w.jobs.insert(j.name.clone(), j.clone());
        }
        w
    }

    fn queued(name: &str, model: Option<&str>) -> Job {
        let models = model.map(|m| BTreeMap::from([("omp".to_owned(), m.to_owned())])).unwrap_or_default();
        Job { name: name.into(), status: Status::Queued, models, ..Job::default() }
    }

    #[test]
    fn the_configured_model_alone_renders_as_one_row() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", None), queued("b", Some("minimax-code/MiniMax-M3.1-Flash"))];
        let v = view(&cfg, &[omp()], &world(&jobs), &BTreeMap::new(), NOW);
        assert_eq!(crate::systems::view::table(&v.cells), ["omp:default  ready  4% minimax-code/General 7 Day"]);
        assert_eq!(v.ready, [true]);
        assert!(v.json["accounts"][0].get("byModel").is_none(), "{}", v.json);
    }

    #[test]
    fn a_queued_jobs_model_at_floor_shows_the_hold() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("openai-codex/gpt-6.1-sol")), queued("b", Some("openai-codex/gpt-6.1-sol"))];
        let v = view(&cfg, &[omp()], &world(&jobs), &BTreeMap::new(), NOW);
        let resets = accounts::fmt_time(secs(NOW) + 3_600);
        assert_eq!(
            crate::systems::view::table(&v.cells),
            [
                "omp:default                     ready     4% minimax-code/General 7 Day".to_owned(),
                format!("  for openai-codex/gpt-6.1-sol  at floor  100% openai-codex/5 hours, resets {resets}"),
            ]
        );
        assert_eq!(v.ready, [true, false]);
        let by_model = &v.json["accounts"][0]["byModel"];
        assert_eq!(by_model[0]["model"], "openai-codex/gpt-6.1-sol");
        assert_eq!(by_model[0]["state"], "at floor");
        assert_eq!(v.json["accounts"][0]["state"], "ready");
    }

    #[test]
    fn the_state_cell_does_not_repeat_the_meter() {
        let (_d, cfg) = cfg("openai-codex/gpt-6.1-sol");
        let v = view(&cfg, &[omp()], &world(&[]), &BTreeMap::new(), NOW);
        let resets = accounts::fmt_time(secs(NOW) + 3_600);
        assert_eq!(v.cells[0][1], "at floor");
        assert_eq!(v.cells[0][3], format!("100% openai-codex/5 hours, resets {resets}"));
    }
}
