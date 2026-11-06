//! quota: how much room each account has, for every harness alike, and the
//! owner's switches. Emits: account-enabled, reading. Readings come from each
//! harness's quota command (no model call) and from what sessions stream.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

use anyhow::Result;
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{QuotaEvent, now_micros, secs};
use crate::config::{Config, Limits};
use crate::ctx::Ctx;
use crate::harness::{Home, Identity, Meters, SignIn};
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
        /// Only accounts some queued job may run on: a reading nobody waits for is not taken.
        #[arg(long)]
        wanted: bool,
    },
    /// Let relay use an account (`harness:label`) again, for example after signing it back in.
    Enable { account: String },
    /// Stop relay from starting sessions on an account (`harness:label`).
    Disable { account: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show => show(ctx, &ctx.world()?),
        Cmd::Refresh { stale, wanted } => refresh(ctx, stale, wanted),
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

fn refresh(ctx: &Ctx, only_stale: bool, only_wanted: bool) -> Result<()> {
    // One reader at a time: overlapping requests from dispatch passes collapse into this one.
    let Some(_role) = Role::try_take(&ctx.state.join("quota.lock"))? else {
        return ctx.reply(&serde_json::json!({ "skipped": "a reading is already being taken" }), "");
    };
    // A reading another project took recently is as good as one of ours.
    let mut world = ctx.world()?;
    world.hear_accounts(&ctx.neighbours());
    let found = accounts::discover(&ctx.cfg);
    accounts::settle(&mut world, &found);
    let now = now_micros();
    let state = |a: &Home| world.accounts.get(&a.key());
    let mut todo: Vec<Home> = found
        .iter()
        .filter(|a| harness_of(a).caps().quota && !state(a).is_some_and(|s| s.unsigned))
        .filter(|a| !only_stale || accounts::due(harness_of(a), state(a), now))
        .filter(|a| !only_wanted || wanted(&world, a))
        .cloned()
        .collect();
    // Homes signed in as one account share a reading: one of them is read, preferring one not known signed out.
    todo.sort_by_key(|a| state(a).is_some_and(|s| s.signed_out));
    let twins = accounts::twins(&found);
    let mut seen = BTreeSet::new();
    todo.retain(|a| seen.insert(accounts::seat(&twins, &a.key()).to_owned()));
    let port = ctx.port(&Source(()));
    // In parallel: one reading can take a minute and a half.
    let readings: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = todo.iter().map(|a| s.spawn(move || (a, read(ctx, a)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let now_s = secs(now);
    let (mut read, mut unreadable, mut signed_out) = (Vec::new(), Vec::new(), Vec::new());
    for (home, text) in readings {
        let h = harness_of(home);
        let meters = text.as_deref().map(|t| h.parse_quota(t, now_s)).unwrap_or_default();
        let account = home.key();
        let sign_in = match accounts::sign_in(home) {
            SignIn::As(id) => Some(id.stamp),
            _ => None,
        };
        if !meters.is_empty() {
            read.push(account.clone());
            port.emit(QuotaEvent::Reading { account, meters, sign_in })?;
        } else if text.as_deref().is_some_and(|t| h.lapsed(t)) {
            signed_out.push(account.clone());
            port.emit(QuotaEvent::SignedOut { account, sign_in })?;
        } else {
            // An unreadable account stays unread, which is not the same as having room.
            unreadable.push(account.clone());
            port.emit(QuotaEvent::Unreadable { account })?;
        }
    }
    let mut text = Vec::new();
    if !unreadable.is_empty() {
        text.push(format!("no reading: {}", unreadable.join(", ")));
    }
    if !signed_out.is_empty() {
        text.push(format!("signed out: {}", signed_out.join(", ")));
    }
    ctx.reply(&serde_json::json!({ "read": read, "unreadable": unreadable, "signedOut": signed_out }), &text.join("\n"))
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
    /// Who the home is signed in as: a plan and a short hash, never an email address or uuid.
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<Identity>,
    /// The first home signed in as the same account, when this one is not the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    same_as: Option<String>,
    state: String,
    running: usize,
    meters: Option<&'a Meters>,
    read_at: Option<u64>,
    /// The meter that decides among those that govern the model, as a table cell.
    #[serde(skip)]
    tightest: String,
    /// The floor of that meter, and why when it is not `limits.floor`.
    #[serde(flatten)]
    floor: Floor,
    #[serde(skip)]
    ready: bool,
    /// The account judged for each other model a queued or running job asks of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_model: Vec<Judged>,
}

/// An account judged for one model other than its harness's configured one.
#[derive(serde::Serialize)]
struct Judged {
    model: String,
    state: String,
    #[serde(skip)]
    tightest: String,
    #[serde(flatten)]
    floor: Floor,
    #[serde(skip)]
    ready: bool,
}

/// The floor that applies to the meter that decides.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Floor {
    #[serde(skip_serializing_if = "Option::is_none")]
    floor: Option<f64>,
    /// Present when the floor is the near-reset one: `floor 98%, resets in 10h`.
    #[serde(skip_serializing_if = "Option::is_none")]
    floor_why: Option<String>,
}

impl Row<'_> {
    /// Its table row, then a sub-row per other model, each with whether it
    /// would take a job: what an operator scans first comes first, and only
    /// the meter that decides (`--json` has them all).
    fn cells(&self) -> impl Iterator<Item = (Vec<String>, bool)> {
        let running = if self.running > 0 { format!("{} running", self.running) } else { String::new() };
        let who = self.identity.as_ref().map(Identity::label).unwrap_or_default();
        let state = match &self.same_as {
            Some(first) => format!("{}, same sign-in as {first}", self.state),
            None => self.state.clone(),
        };
        let own = vec![self.account.clone(), who, state, running, self.tightest.clone()];
        // A sub-row that says what its account's row says is left out.
        let others = self.by_model.iter().filter(|j| (&j.state, &j.tightest) != (&self.state, &self.tightest));
        let others = others.map(move |j| {
            (
                vec![format!("  for {}", j.model), String::new(), j.state.clone(), String::new(), j.tightest.clone()],
                j.ready,
            )
        });
        std::iter::once((own, self.ready)).chain(others)
    }
}

/// How an account stands for `model`: its state cell, the meter that decides, and whether it would take the job.
fn judge(
    a: &Home,
    st: Option<&AccountState>,
    model: Option<&str>,
    limits: &Limits,
    now: u64,
) -> (String, String, Floor, bool) {
    let h = harness_of(a);
    let fit = accounts::fit(a, st, model, limits, now);
    let ready = fit.is_ok();
    let state = match fit {
        Ok(_) if !h.caps().quota => "ready (unmetered)".to_owned(),
        Ok(_) => "ready".to_owned(),
        Err(accounts::Refusal::SignedOut) => {
            format!("signed out since {}", accounts::fmt_time(secs(st.and_then(|s| s.signed_at).unwrap_or(0))))
        }
        Err(accounts::Refusal::Limited) => {
            format!("limited until {}", accounts::fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
        }
        // The meter cell names the meter.
        Err(accounts::Refusal::AtFloor(_)) => "at floor".to_owned(),
        Err(r) => r.describe(),
    };
    let deciding = st.and_then(|s| {
        let governing = h.meters_for(model, &s.meters).ok()?;
        // The meter that holds the account back, else the closest to its limit.
        accounts::at_floor(&governing, &s.meters, limits, secs(now))
            .or_else(|| accounts::tightest(&governing, &s.meters, secs(now)))
    });
    let cell = deciding.map_or(String::new(), |(k, m)| accounts::meter_line(k, m));
    let floor = deciding.map_or(Floor { floor: None, floor_why: None }, |(_, m)| Floor {
        floor: Some(limits.floor_for(m.resets_at, secs(now)).0),
        floor_why: accounts::floor_note(limits, m, secs(now)),
    });
    // A floor loosened for a reset is part of what the cell says.
    let cell = floor.floor_why.as_ref().map_or(cell.clone(), |why| format!("{cell} ({why})"));
    (state, cell, floor, ready)
}

/// Whether a queued job may run on `account`, so its reading decides something.
pub fn wanted(world: &World, account: &Home) -> bool {
    world.jobs.values().any(|j| world.startable(j) && may_use(j, account))
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
    accounts_among(&ctx.cfg, world, &ctx.neighbours())
}

/// As [`accounts_view`], for `world` among the other projects' folds, already read
/// (outside any project, `world` is empty and `others` are all of them).
pub fn accounts_among<'a>(
    cfg: &Config,
    world: &World,
    others: impl IntoIterator<Item = &'a World> + Clone,
) -> AccountsView {
    let mut world = world.clone();
    world.hear_accounts(others.clone());
    let found = accounts::discover(cfg);
    accounts::settle(&mut world, &found);
    view(cfg, &found, &world, &seats_taken(others), now_micros())
}

/// The view of `found` in `world` (which has heard its neighbours), with the
/// seats other projects hold (`elsewhere`), at `now` (bus time).
fn view(cfg: &Config, found: &[Home], world: &World, elsewhere: &BTreeMap<String, usize>, now: u64) -> AccountsView {
    let limits = &cfg.limits;
    let twins = accounts::twins(found);
    let row = |a: &Home| {
        let key = a.key();
        let st = world.accounts.get(&key);
        // The dispatcher's own judgment, for the configured model and every other one its jobs ask for.
        let model = cfg.harness(a.harness).model;
        let (state, tightest, floor, ready) = judge(a, st, model.as_deref(), limits, now);
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
                let (state, tightest, floor, ready) = judge(a, st, Some(&m), limits, now);
                Judged { model: m, state, tightest, floor, ready }
            })
            .collect();
        // Twins share their seats: a session on either is a session on both.
        let seat = accounts::seat(&twins, &key);
        let here = |k: &str| accounts::seat(&twins, k) == seat;
        let running =
            world.jobs.values().filter(|j| j.status == Status::Running && j.account.as_deref().is_some_and(here));
        let identity = match accounts::sign_in(a) {
            SignIn::As(id) => Some(id),
            SignIn::Out | SignIn::Unknown => None,
        };
        Row {
            running: running.count() + elsewhere.iter().filter(|(k, _)| here(k)).map(|(_, n)| n).sum::<usize>(),
            same_as: twins.get(&key).cloned(),
            identity,
            account: key,
            state,
            meters: st.map(|s| &s.meters),
            read_at: st.and_then(|s| s.read_at),
            tightest,
            floor,
            ready,
            by_model,
        }
    };
    let rows: Vec<Row<'_>> = found.iter().map(row).collect();
    let names: Vec<_> = cfg.harnesses().iter().map(|h| h.id()).collect();
    let l = &cfg.limits;
    let near = l
        .floor_near_reset
        .map_or(String::new(), |n| format!(", {:.0}% within {}h of a reset", n.floor * 100.0, n.within_hours));
    let head = format!(
        "accounts on {} (floor {:.0}%{near}, {} session(s) each)",
        names.join(", "),
        l.floor * 100.0,
        l.per_account
    );
    let (cells, ready) = rows.iter().flat_map(Row::cells).unzip();
    AccountsView { json: serde_json::json!({ "harnesses": names, "accounts": rows }), head, cells, ready }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::tempdir;
    use crate::harness::Meter;
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
    fn only_an_account_a_queued_job_may_use_is_worth_reading() {
        let claude = Home { harness: "claude", label: "a".into(), dir: PathBuf::from("/c") };
        let mut on_omp = queued("a", None);
        on_omp.harnesses = vec!["omp".into()];
        let w = world(&[on_omp.clone()]);
        assert!(wanted(&w, &omp()) && !wanted(&w, &claude), "a job pinned to omp says nothing about claude");
        let running = Job { status: Status::Running, ..on_omp };
        assert!(!wanted(&world(&[running]), &omp()), "nothing waits, so nothing is read");
        assert!(wanted(&world(&[queued("b", None)]), &claude), "an unpinned job may use any harness");
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
    fn a_model_judged_as_its_account_is_no_sub_row_of_its_own() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("minimax-code/MiniMax-M3.5"))];
        let v = view(&cfg, &[omp()], &world(&jobs), &BTreeMap::new(), NOW);
        assert_eq!(v.cells.len(), 1, "the sub-row would repeat its account's row: {:?}", v.cells);
        assert_eq!(v.json["accounts"][0]["byModel"][0]["model"], "minimax-code/MiniMax-M3.5", "--json keeps it");
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
    fn the_board_shows_who_each_home_is_and_which_are_one_account() {
        let (dir, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let home = |label: &str, uuid: &str| {
            let dir = dir.path().join(label);
            std::fs::create_dir_all(&dir).unwrap();
            let json = format!(
                r#"{{"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"me@secret.invalid","planDisplayName":"Ultra"}}}}"#
            );
            std::fs::write(dir.join(".claude.json"), json).unwrap();
            Home { harness: "claude", label: label.into(), dir }
        };
        let found = [home("a", "u-1"), home("b", "u-1"), home("c", "u-2")];
        let mut w = World::default();
        for h in &found {
            w.accounts.insert(h.key(), AccountState { read_at: Some(NOW - 1), ..Default::default() });
        }
        w.accounts.get_mut("claude:c").unwrap().signed_out = true;
        w.accounts.get_mut("claude:c").unwrap().signed_at = Some(NOW);
        let v = view(&cfg, &found, &w, &BTreeMap::new(), NOW);
        let table = v.text();
        let (first, third) = (v.json["accounts"][0]["identity"]["id"].as_str().unwrap(), &v.json["accounts"][2]);
        assert!(table.contains(&format!("claude:a  Ultra #{first}  ready")), "{table}");
        assert!(table.contains(&format!("claude:b  Ultra #{first}  ready, same sign-in as claude:a")), "{table}");
        assert!(table.contains("signed out since "), "{table}");
        assert_eq!(v.json["accounts"][1]["sameAs"], "claude:a");
        assert!(third.get("sameAs").is_none());
        for secret in ["me@secret.invalid", "u-1", "u-2"] {
            assert!(!table.contains(secret) && !v.json.to_string().contains(secret), "{secret}");
        }
    }

    #[test]
    fn the_state_cell_does_not_repeat_the_meter() {
        let (_d, cfg) = cfg("openai-codex/gpt-6.1-sol");
        let v = view(&cfg, &[omp()], &world(&[]), &BTreeMap::new(), NOW);
        let resets = accounts::fmt_time(secs(NOW) + 3_600);
        assert_eq!(v.cells[0][2], "at floor");
        assert_eq!(v.cells[0][4], format!("100% openai-codex/5 hours, resets {resets}"));
    }
}
