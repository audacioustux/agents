//! quota: how much room each account has, for every harness alike, and the
//! owner's switches. Emits: account-enabled, reading. Readings come from each
//! harness's quota command (no model call) and from what sessions stream.

use std::collections::BTreeSet;
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
use crate::systems::dispatch::{Room, may_use, model_for};
use crate::world::{AccountState, Job, Status, World};

pub mod gauge;
use gauge::Gauge;

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
    /// Each account's room. Starts a reading of any that is stale, and says so.
    Show,
    /// Read every account's quota now (or only stale readings).
    Refresh {
        #[arg(long)]
        stale: bool,
        /// Only accounts some queued job may run on: a reading nobody waits for is not taken.
        #[arg(long)]
        wanted: bool,
        /// Read the accounts this project asked for (`start_refresh`) until none is left. What
        /// relay starts in the background; not for use by hand.
        #[arg(long, hide = true, conflicts_with_all = ["stale", "wanted"])]
        pending: bool,
    },
    /// Let relay use an account (`harness:label`) again, for example after signing it back in.
    Enable { account: String },
    /// Stop relay from starting sessions on an account (`harness:label`).
    Disable { account: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show => show(ctx, &ctx.world()?, true),
        Cmd::Refresh { pending: true, .. } => drain(ctx),
        Cmd::Refresh { stale, wanted, .. } => refresh(ctx, stale, wanted),
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
    show(ctx, &ctx.world()?, false)
}

/// The quota reader's role. Accounts belong to the machine, so there is one reader on it, whichever
/// project asks: two projects never read one account at once.
fn reader_lock(ctx: &Ctx) -> std::path::PathBuf {
    ctx.state_root.join("quota.lock")
}

/// The accounts in `room` a reading would be taken of: those that report quota and are signed in;
/// with `only_stale`, only those whose reading is due; with `only_wanted`, only those a queued job
/// of `world` may run on; with `only`, only those keys. One home of each sign-in, preferring one
/// not known signed out.
fn to_read(room: &Room, world: &World, select: &Select<'_>, now: u64) -> Vec<Home> {
    let Select { only_stale, only_wanted, only } = *select;
    let state = |a: &Home| room.account(&a.key());
    let mut todo: Vec<Home> = room
        .found()
        .iter()
        .filter(|a| harness_of(a).caps().quota && !state(a).is_some_and(|s| s.unsigned))
        .filter(|a| !only_stale || accounts::due(harness_of(a), state(a), now))
        .filter(|a| !only_wanted || wanted(world, a))
        .filter(|a| only.is_none_or(|keys| keys.contains(&a.key())))
        .cloned()
        .collect();
    // Homes signed in as one account share a reading: one of them is read.
    todo.sort_by_key(|a| state(a).is_some_and(|s| s.signed_out));
    let mut seen = BTreeSet::new();
    todo.retain(|a| seen.insert(room.twin_of(&a.key()).cloned().unwrap_or_else(|| a.key())));
    todo
}

/// The accounts this project asked to have read that no reader has taken yet: a set (one key per
/// line in `<state>/quota.pending`), changed only under its lock. Requests add to it
/// ([`start_refresh`]); this project's drainer ([`drain`]) empties it, reading each, before it exits.
struct Pending<'a>(&'a Ctx);

impl Pending<'_> {
    /// `f` on the set, under its lock; written back when `f` changed it.
    fn with<T>(&self, f: impl FnOnce(&mut BTreeSet<String>) -> T) -> Result<T> {
        let _lock = Role::take(&self.0.state.join("quota.pending.lock"))?;
        let file = self.0.state.join("quota.pending");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let mut set: BTreeSet<String> = text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
        let before = set.clone();
        let out = f(&mut set);
        if set != before {
            let text: String = set.iter().map(|k| k.clone() + "\n").collect();
            Ctx::write_private(&file, text.as_bytes())?;
        }
        Ok(out)
    }

    fn add(&self, keys: &[String]) -> Result<()> {
        self.with(|set| set.extend(keys.iter().cloned()))
    }

    fn take(&self) -> Result<BTreeSet<String>> {
        self.with(std::mem::take)
    }

    fn is_empty(&self) -> Result<bool> {
        self.with(|set| set.is_empty())
    }
}

/// The role of this project's drainer: one at a time reads what it asked for.
fn drainer_lock(ctx: &Ctx) -> std::path::PathBuf {
    ctx.state.join("quota.drainer.lock")
}

/// Asks for a reading of every account in `room` whose reading is due (with `wanted`, only of
/// accounts a queued job of `world` may run on), and returns them: each is in this project's
/// pending set, which a drainer it starts (unless one is at work, which will see them) reads to
/// the last before it exits, so every account returned is one a reader will read. The one way
/// relay starts a reading on its own: the dispatcher, a checker waiting for a seat, and every
/// command that shows the accounts.
pub fn start_refresh(ctx: &Ctx, room: &Room, world: &World, wanted: bool) -> Result<Vec<String>> {
    let select = Select { only_stale: true, only_wanted: wanted, only: None };
    let due: Vec<String> = to_read(room, world, &select, now_micros()).iter().map(Home::key).collect();
    if due.is_empty() {
        return Ok(due);
    }
    Pending(ctx).add(&due)?;
    // A drainer at work looks at the set again after it lets go of its role, so it sees these.
    if Role::try_take(&drainer_lock(ctx))?.is_some() {
        ctx.spawn_self(&["quota", "refresh", "--pending"], &ctx.state.join("quota.log"))?;
    }
    Ok(due)
}

/// This project's drainer: reads what is pending until nothing is, waiting for the machine's
/// reader in turn. It looks once more after letting go of its role, so a request made as it was
/// finishing (which found the role taken and started no drainer) is not left behind.
fn drain(ctx: &Ctx) -> Result<()> {
    let pending = Pending(ctx);
    loop {
        let Some(role) = Role::try_take(&drainer_lock(ctx))? else { return Ok(()) };
        loop {
            let asked = pending.take()?;
            if asked.is_empty() {
                break;
            }
            let r = read_accounts(ctx, &Select { only_stale: true, only_wanted: false, only: Some(&asked) })?;
            out!(
                "{} pending {}: {}",
                crate::os::stamp(),
                asked.iter().cloned().collect::<Vec<_>>().join(", "),
                r.json()
            );
        }
        drop(role);
        if pending.is_empty()? {
            return Ok(());
        }
    }
}

/// Which accounts a reading takes: see [`to_read`].
#[derive(Clone, Copy)]
struct Select<'a> {
    only_stale: bool,
    only_wanted: bool,
    only: Option<&'a BTreeSet<String>>,
}

/// What a reading took: the accounts read (also those another reader read while this one
/// waited), those that gave no reading, and those found signed out.
struct Taken {
    read: Vec<String>,
    unreadable: Vec<String>,
    signed_out: Vec<String>,
}

impl Taken {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "read": self.read, "unreadable": self.unreadable, "signedOut": self.signed_out })
    }
}

fn refresh(ctx: &Ctx, only_stale: bool, only_wanted: bool) -> Result<()> {
    let t = read_accounts(ctx, &Select { only_stale, only_wanted, only: None })?;
    let mut text = Vec::new();
    if !t.unreadable.is_empty() {
        text.push(format!("no reading: {}", t.unreadable.join(", ")));
    }
    if !t.signed_out.is_empty() {
        text.push(format!("signed out: {}", t.signed_out.join(", ")));
    }
    ctx.reply(&t.json(), &text.join("\n"))
}

/// Takes the readings `select` names. One reader on the machine at a time: one that finds another
/// at work waits for it, then takes only what that one did not, so it never ends without the
/// readings it was asked for.
fn read_accounts(ctx: &Ctx, select: &Select<'_>) -> Result<Taken> {
    let asked = now_micros();
    let _role = Role::take(&reader_lock(ctx))?;
    // A reading another project took recently is as good as one of ours.
    let world = ctx.world()?;
    let neighbours = ctx.neighbours();
    let room = Room::new(&ctx.cfg, &world, &neighbours.iter().collect::<Vec<_>>(), true);
    let now = now_micros();
    let meanwhile = |a: &Home| room.account(&a.key()).and_then(|s| s.read_at).is_some_and(|t| t >= asked);
    let (taken, todo): (Vec<Home>, Vec<Home>) = to_read(&room, &world, select, now).into_iter().partition(meanwhile);
    let port = ctx.port(&Source(()));
    // In parallel: one reading can take a minute and a half.
    let readings: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = todo.iter().map(|a| s.spawn(move || (a, read(ctx, a)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let now_s = secs(now);
    let mut read: Vec<String> = taken.iter().map(Home::key).collect();
    let (mut unreadable, mut signed_out) = (Vec::new(), Vec::new());
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
    Ok(Taken { read, unreadable, signed_out })
}

fn read(ctx: &Ctx, home: &Home) -> Option<String> {
    let inv = ctx.cfg.locate(home.harness, harness_of(home).quota(home)?).ok()?;
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

/// One account, as `show --json` reports it.
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
    /// Room left per day on the meters that govern, as dispatch ranks it, when its quota fits.
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    /// The floor of the meter that decides, and why when it is not `limits.floor`.
    #[serde(flatten)]
    floor: Floor,
    /// The account judged for each other model a queued or running job asks of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_model: Vec<Judged>,
}

/// An account judged for one model other than its harness's configured one.
#[derive(serde::Serialize)]
struct Judged {
    model: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(flatten)]
    floor: Floor,
}

/// The floor that applies to the meter that decides.
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct Floor {
    #[serde(skip_serializing_if = "Option::is_none")]
    floor: Option<f64>,
    /// Present when the floor is the near-reset one: `floor 98%, resets in 10h`.
    #[serde(skip_serializing_if = "Option::is_none")]
    floor_why: Option<String>,
}

/// How an account stands for one model, by the dispatcher's own judgment.
struct Judgment {
    state: String,
    /// The meter that decides: the one that holds the account back, else the closest to its limit.
    deciding: Option<String>,
    floor: Floor,
    /// How dispatch ranks it ([`accounts::fit`]) when its own state and quota let it take the job.
    score: Option<f64>,
    /// Whether it would take the job now, its seats included ([`Room::admit`]).
    ready: bool,
}

/// How an account stands for `model`, by the dispatcher's own judgment ([`Room::admit`]): a full
/// seat is not ready.
fn judge(room: &Room, a: &Home, model: Option<&str>, limits: &Limits, now: u64) -> Judgment {
    let h = harness_of(a);
    let st = room.account(&a.key());
    let admitted = room.admit(limits, a, model, now);
    let ready = admitted.is_ok();
    let state = match admitted {
        Ok(_) if !h.caps().quota => "ready (unmetered)".to_owned(),
        Ok(_) => "ready".to_owned(),
        Err(r) => {
            let fit = r.fit.as_ref().map(|f| refusal_cell(f, st, now));
            r.busy.into_iter().chain(fit).collect::<Vec<_>>().join(", ")
        }
    };
    let deciding = st.and_then(|s| {
        let governing = h.meters_for(model, &s.meters).ok()?;
        accounts::at_floor(&governing, &s.meters, limits, secs(now))
            .or_else(|| accounts::tightest(&governing, &s.meters, secs(now)))
    });
    let floor = deciding.map_or_else(Floor::default, |(_, m)| Floor {
        floor: Some(limits.floor_for(m.resets_at, secs(now)).0),
        floor_why: accounts::floor_note(limits, m, secs(now)),
    });
    let score = accounts::fit(a, st, model, limits, now).ok();
    Judgment { state, deciding: deciding.map(|(k, _)| k.to_owned()), floor, score, ready }
}

/// A state cell for what holds an account back.
fn refusal_cell(r: &accounts::Refusal, st: Option<&AccountState>, now: u64) -> String {
    match r {
        accounts::Refusal::SignedOut => {
            format!("signed out since {}", accounts::fmt_time(secs(st.and_then(|s| s.signed_at).unwrap_or(0))))
        }
        accounts::Refusal::Limited => {
            format!("limited until {}", accounts::fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
        }
        accounts::Refusal::Stale => st
            .and_then(|s| s.read_at)
            .map_or_else(|| r.describe(), |t| format!("stale (read {} ago)", crate::systems::view::ago(now, t))),
        // The meter's gauge shows it.
        accounts::Refusal::AtFloor(_) => "at floor".to_owned(),
        r => r.describe(),
    }
}

/// One row of the accounts board: an account with the meter that decides, another meter of the
/// account above, another model one of its jobs asks for, or a harness whose program is not found.
#[derive(Debug, Clone, Default)]
pub struct Shown {
    /// `▸ claude:a` on the account the next start would take, else `  claude:a`; a model's
    /// sub-row's is `    for <model>`, and a meter's is blank.
    pub account: String,
    pub state: String,
    /// The row's meter in words (`7 days`, `openai-codex/5 hours`), and its gauge.
    pub meter: String,
    pub gauge: Option<Gauge>,
    /// Room left per day, as dispatch ranks it: `12%/d`.
    pub room: String,
    pub running: String,
    /// Whether it would take a job (on that row's model) now.
    pub ready: bool,
    /// A sub-row, which leaves blank what its account's row says.
    pub sub: bool,
}

impl Shown {
    /// Its cells under [`AccountsView::HEADINGS`], its gauge written by `gauge`.
    fn cells(&self, gauge: &impl Fn(&Gauge) -> String) -> Vec<String> {
        let shown = self.gauge.as_ref().map_or_else(String::new, gauge);
        let cells = [&self.account, &self.state, &self.meter, &shown, &self.room, &self.running];
        cells.into_iter().cloned().collect()
    }

    /// `main` showing the first of `meters`, then a sub-row for each of the others.
    fn with_meters(main: Self, meters: Vec<(String, Gauge)>) -> Vec<Self> {
        let mut meters = meters.into_iter().map(|(name, g)| (gauge::heading(&name), Some(g)));
        let first = meters.next().map_or(main.clone(), |(meter, gauge)| Self { meter, gauge, ..main });
        let others = meters.map(|(meter, gauge)| Self { meter, gauge, sub: true, ..Self::default() });
        std::iter::once(first).chain(others).collect()
    }
}

/// The meters of an account whose state is `st` as gauges at `now`, each with its name, their
/// floors by `limits` when a project's rules apply: the `deciding` one marked and first, then
/// the others, the closest to its floor first.
fn meters(
    st: Option<&AccountState>,
    limits: Option<&Limits>,
    deciding: Option<&str>,
    now: u64,
) -> Vec<(String, Gauge)> {
    let gauge = |(name, m): (&String, &crate::harness::Meter)| {
        (name.clone(), Gauge::new(m, limits, deciding == Some(name.as_str()), secs(now)))
    };
    let mut all: Vec<(String, Gauge)> = st.into_iter().flat_map(|s| s.meters.iter().map(gauge)).collect();
    all.sort_by(|(_, a), (_, b)| b.governing.cmp(&a.governing).then(b.past_floor().total_cmp(&a.past_floor())));
    all
}

/// A `room/day` cell: `12%/d`, or `—` when the account's quota would not take the job.
fn room_cell(score: Option<f64>) -> String {
    score.map_or_else(|| "—".to_owned(), |s| format!("{:.0}%/d", s * 100.0))
}

/// An account cell, `▸` on the account the next start would take.
fn account_cell(key: &str, picked: bool) -> String {
    format!("{} {key}", if picked { "▸" } else { " " })
}

/// Whether a queued job may run on `account`, so its reading decides something.
pub fn wanted(world: &World, account: &Home) -> bool {
    world.jobs().values().any(|j| world.startable(j) && may_use(j, account))
}

/// Every account's room; with `refresh`, a reading of each stale one is started first, and the view says so.
pub fn show(ctx: &Ctx, world: &World, refresh: bool) -> Result<()> {
    let neighbours = ctx.neighbours();
    let others: Vec<&World> = neighbours.iter().collect();
    let room = room_for(Rules::Project(ctx), world, &others);
    let mut v = accounts_in(Rules::Project(ctx), &room, world, &others);
    if refresh {
        v.refreshing(start_refresh(ctx, &room, world, false)?);
    }
    ctx.reply(&v.json, &v.text())
}

/// Every account's room: as every project on this machine knows it, with
/// the sessions all of them run there.
#[derive(Debug, Clone, Default)]
pub struct AccountsView {
    pub json: serde_json::Value,
    pub head: String,
    /// A row per account, each followed by a sub-row per other meter, then per other model its
    /// jobs ask for.
    pub rows: Vec<Shown>,
}

impl AccountsView {
    /// Says that a reading of `accounts` is being taken, in the head and in `--json` (`refreshing`).
    pub fn refreshing(&mut self, accounts: Vec<String>) {
        if !accounts.is_empty() {
            self.head = format!("{}; refreshing {}", self.head, accounts.join(", "));
            self.json["refreshing"] = accounts.into();
        }
    }

    /// The columns' headings: the account, its state, a meter and its gauge, room/day, run.
    pub const HEADINGS: [&str; 6] = ["", "state", "meter", "gauge", "room/day", "run"];

    /// Each row's cells under [`AccountsView::HEADINGS`], each gauge written by `gauge`.
    pub fn cells(&self, gauge: impl Fn(&Gauge) -> String) -> Vec<Vec<String>> {
        self.rows.iter().map(|r| r.cells(&gauge)).collect()
    }

    /// The rows as aligned text under their headings, bars `bar` cells wide. A column empty in
    /// every row is left out, its heading too.
    pub fn table(&self, bar: usize) -> Vec<String> {
        let body = self.cells(|g| g.text(bar));
        let used = |c: usize| body.iter().any(|r| r.get(c).is_some_and(|s| !s.is_empty()));
        let heads =
            Self::HEADINGS.iter().enumerate().map(|(c, h)| if used(c) { (*h).to_owned() } else { String::new() });
        crate::systems::view::table(&std::iter::once(heads.collect()).chain(body).collect::<Vec<_>>())
    }

    pub fn text(&self) -> String {
        self.text_in(None)
    }

    /// The head and the table, indented, its bars as wide as `width` columns allow, down to
    /// [`gauge::MIN_BAR`] cells: a narrow window shrinks the bars before it cuts the lines.
    pub fn text_in(&self, width: Option<usize>) -> String {
        let fits = |lines: &[String]| width.is_none_or(|w| lines.iter().all(|l| l.chars().count() + 2 <= w));
        let table = if self.rows.is_empty() {
            Vec::new()
        } else {
            let mut tables = (gauge::MIN_BAR..=gauge::BAR).rev().map(|bar| self.table(bar));
            let narrowest = self.table(gauge::MIN_BAR);
            tables.find(|t| fits(t)).unwrap_or(narrowest)
        };
        let lines = table.into_iter().map(|l| format!("  {l}"));
        std::iter::once(self.head.clone()).chain(lines).collect::<Vec<_>>().join("\n")
    }
}

/// Whose rules judge whether an account would take a job.
#[derive(Debug, Clone, Copy)]
pub enum Rules<'a> {
    /// A project's: its floor, seats, `accounts.exclude`, harnesses and `quota disable`. Only a
    /// project relay runs in has a [`Ctx`], so the owner's policy alone, with every default, can
    /// never judge as if it were one.
    Project(&'a Ctx),
    /// None, outside every project: every account on the machine with what all projects share
    /// (sign-ins, limits, readings, sessions running), and no account called ready, since what
    /// would decide that is each project's own.
    MachineWide,
}

impl<'a> Rules<'a> {
    /// The project's rules where relay runs in one (`home`), else none.
    pub fn of(home: Option<&'a Ctx>) -> Self {
        home.map_or(Self::MachineWide, Self::Project)
    }
}

/// The room `rules` judge accounts and jobs in, for `world` among the other projects' folds
/// (outside any project, `world` is empty and `others` are all of them).
pub fn room_for(rules: Rules<'_>, world: &World, others: &[&World]) -> Room {
    match rules {
        Rules::Project(ctx) => {
            Room::new(&ctx.cfg, world, others, crate::systems::dispatch::disk_ok(&ctx.cfg.limits, &ctx.clones))
        }
        Rules::MachineWide => Room::machine_wide(world, others),
    }
}

/// The accounts' view for `world` among the other projects' folds, already read, judged by
/// `rules` in `room` (from [`room_for`] with the same rules and folds).
pub fn accounts_in(rules: Rules<'_>, room: &Room, world: &World, others: &[&World]) -> AccountsView {
    match rules {
        Rules::Project(ctx) => {
            let mut v = view(&ctx.cfg, room, world, others, now_micros());
            v.missing(&ctx.cfg.missing_harnesses());
            v
        }
        Rules::MachineWide => machine_wide(room, now_micros()),
    }
}

impl AccountsView {
    /// A row for each harness the project expects whose program is not found, never called ready.
    fn missing(&mut self, missing: &[(&str, crate::config::Missing)]) {
        for (id, lacks) in missing {
            self.rows.push(Shown { account: account_cell(id, false), state: lacks.cell(), ..Shown::default() });
        }
        self.json["missing"] = missing.iter().map(|(id, _)| *id).collect::<Vec<_>>().into();
    }
}

/// Who a home is signed in as, as its files say.
fn identity(a: &Home) -> Option<Identity> {
    match accounts::sign_in(a) {
        SignIn::As(id) => Some(id),
        SignIn::Out | SignIn::Unknown => None,
    }
}

/// The state cell of an account, which names its first twin.
fn state_cell(state: &str, same_as: Option<&String>) -> String {
    match same_as {
        Some(first) => format!("{state}, same sign-in as {first}"),
        None => state.to_owned(),
    }
}

/// Every account on the machine as all projects know it, judged by none of them: no floor, no
/// room per day and no next pick, since those are each project's own.
fn machine_wide(room: &Room, now: u64) -> AccountsView {
    let found = room.found();
    let mut rows = Vec::new();
    let mut shown = Vec::new();
    for a in found {
        let key = a.key();
        let st = room.account(&key);
        let state = accounts::shared_refusal(harness_of(a), st, now)
            .map_or_else(|| "not limited".to_owned(), |r| refusal_cell(&r, st, now));
        let tightest = st.and_then(|s| {
            let all: Vec<&crate::harness::Meter> = s.meters.values().collect();
            accounts::tightest(&all, &s.meters, secs(now)).map(|(k, _)| k)
        });
        let row = Row {
            same_as: room.twin_of(&key).cloned(),
            identity: identity(a),
            running: room.running_on(&key),
            account: key,
            state,
            meters: st.map(|s| &s.meters),
            read_at: st.and_then(|s| s.read_at),
            score: None,
            floor: Floor::default(),
            by_model: Vec::new(),
        };
        let main = Shown {
            account: account_cell(&row.account, false),
            state: state_cell(&row.state, row.same_as.as_ref()),
            running: row.running.to_string(),
            ..Shown::default()
        };
        shown.extend(Shown::with_meters(main, meters(st, None, tightest, now)));
        rows.push(row);
    }
    let mut names: Vec<&str> = found.iter().map(|a| a.harness).collect();
    names.dedup();
    let head = format!(
        "accounts on {}, machine-wide (each project judges its own floor, seats and exclusions)",
        if names.is_empty() { "no harness".to_owned() } else { names.join(", ") }
    );
    let json = serde_json::json!({ "machineWide": true, "harnesses": names, "accounts": rows });
    AccountsView { json, head, rows: shown }
}

/// The model a running job runs, as its dispatch recorded it, else (a record from before the
/// model was) the one it names on `harness`.
fn running_model(j: &Job, harness: &str) -> Option<String> {
    j.model.clone().or_else(|| j.models.get(harness).cloned())
}

/// The models other than `model` (its harness's configured one) that jobs ask of the account `a`:
/// the running ones' of any project, our queued ones' and the queued ones of other projects that
/// name one.
fn asked(cfg: &Config, a: &Home, model: Option<&String>, world: &World, others: &[&World]) -> BTreeSet<String> {
    let key = a.key();
    let runs_here = |j: &&Job| j.status == Status::Running && j.account.as_ref() == Some(&key);
    // A running job asks for what its dispatch recorded, whichever project it is. A queued job
    // of another project that names no model asks for its own config's, which is not known
    // here; one that names a model asks for it on this machine's accounts like ours do.
    let running = world.jobs().values().chain(others.iter().flat_map(|w| w.jobs().values())).filter(runs_here);
    let queued = |j: &&Job| j.status == Status::Queued && may_use(j, a);
    let ours = world.jobs().values().filter(queued).filter_map(|j| model_for(cfg, j, a.harness));
    let theirs = others.iter().flat_map(|w| w.jobs().values()).filter(queued).filter_map(|j| j.models.get(a.harness));
    running
        .filter_map(|j| running_model(j, a.harness))
        .chain(ours)
        .chain(theirs.cloned())
        .filter(|m| Some(m) != model)
        .collect()
}

/// What the accounts' view is judged in: the project's config and room, the account the next
/// start would take, and the time.
struct Judge<'a> {
    cfg: &'a Config,
    room: &'a Room,
    next: Option<String>,
    now: u64,
}

impl<'a> Judge<'a> {
    /// The account `a`'s row for `--json`, and its board rows: its own with the meter that
    /// decides, a sub-row for each other meter, then one for each other model its jobs ask for
    /// that it would judge otherwise.
    fn account(&self, a: &Home, world: &World, others: &[&World]) -> (Row<'a>, Vec<Shown>) {
        let (cfg, room, now): (_, &'a Room, _) = (self.cfg, self.room, self.now);
        let key = a.key();
        let st = room.account(&key);
        // The dispatcher's own judgment, for the configured model and every other one its jobs ask for.
        let model = cfg.harness(a.harness).model;
        let own = judge(room, a, model.as_deref(), &cfg.limits, now);
        let asked = asked(cfg, a, model.as_ref(), world, others);
        let by_model: Vec<(String, Judgment)> =
            asked.into_iter().map(|m| (m.clone(), judge(room, a, Some(&m), &cfg.limits, now))).collect();
        let identity = identity(a);
        let same_as = room.twin_of(&key).cloned();
        let main = Shown {
            account: account_cell(&key, self.next.as_ref() == Some(&key)),
            state: state_cell(&own.state, same_as.as_ref()),
            room: room_cell(own.score),
            // Twins share their seats: a session on either is a session on both.
            running: room.running_on(&key).to_string(),
            ready: own.ready,
            ..Shown::default()
        };
        let mut shown = Shown::with_meters(main, meters(st, Some(&cfg.limits), own.deciding.as_deref(), now));
        // A sub-row that says what its account's row says is left out.
        let differs = |j: &Judgment| (&j.state, &j.deciding) != (&own.state, &own.deciding);
        shown.extend(by_model.iter().filter(|(_, j)| differs(j)).map(|(m, j)| self.sub_row(m, j, st)));
        let row = Row {
            running: room.running_on(&key),
            same_as,
            identity,
            account: key,
            state: own.state,
            meters: st.map(|s| &s.meters),
            read_at: st.and_then(|s| s.read_at),
            score: own.score,
            floor: own.floor,
            by_model: by_model
                .into_iter()
                .map(|(model, j)| Judged { model, state: j.state, score: j.score, floor: j.floor })
                .collect(),
        };
        (row, shown)
    }

    /// The sub-row for `model` on an account whose state is `st`: its state, the meter that
    /// decides for it, and its room per day.
    fn sub_row(&self, model: &str, j: &Judgment, st: Option<&AccountState>) -> Shown {
        let mut deciding = meters(st, Some(&self.cfg.limits), j.deciding.as_deref(), self.now).into_iter();
        let (meter, gauge) = deciding
            .next()
            .filter(|(_, g)| g.governing)
            .map_or_else(Default::default, |(name, g)| (gauge::heading(&name), Some(g)));
        Shown {
            account: format!("    for {model}"),
            state: j.state.clone(),
            meter,
            gauge,
            room: room_cell(j.score),
            ready: j.ready,
            sub: true,
            ..Shown::default()
        }
    }
}

/// The head of a project's accounts: its harnesses, floors and seats, and the next pick.
fn head(cfg: &Config, next: Option<&(Option<String>, String)>) -> String {
    let names: Vec<_> = cfg.harnesses().iter().map(|h| h.id()).collect();
    let l = &cfg.limits;
    let near = l.floor_near_reset.map_or(String::new(), |n| {
        format!(" ({:.0}% in the last {}h before a reset)", n.floor * 100.0, n.within_hours)
    });
    let pick = match next {
        Some((Some(job), a)) => format!("next pick ▸ {a} for {job}"),
        Some((None, a)) => format!("next pick ▸ {a}"),
        None => "next pick: none".to_owned(),
    };
    format!(
        "accounts on {} · floor {:.0}%{near} · {} session(s) each · {pick}",
        names.join(", "),
        l.floor * 100.0,
        l.per_account
    )
}

/// The view of the accounts in `room`, with the models the jobs of `world` and of the other
/// projects (`others`) ask for, at `now` (bus time).
fn view(cfg: &Config, room: &Room, world: &World, others: &[&World], now: u64) -> AccountsView {
    let next = crate::systems::dispatch::next_pick(cfg, room, world, now);
    let judge = Judge { cfg, room, next: next.as_ref().map(|(_, a)| a.clone()), now };
    let mut rows = Vec::new();
    let mut shown = Vec::new();
    for a in room.found() {
        let (row, lines) = judge.account(a, world, others);
        rows.push(row);
        shown.extend(lines);
    }
    let names: Vec<_> = cfg.harnesses().iter().map(|h| h.id()).collect();
    let json = serde_json::json!({
        "harnesses": names,
        "accounts": rows,
        "next": next.as_ref().map(|(_, a)| a),
    });
    AccountsView { json, head: head(cfg, next.as_ref()), rows: shown }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use std::collections::BTreeMap;

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

    /// The view of `found` in `world` among `others`, with `world`'s account states as they are.
    fn view_of(cfg: &Config, found: &[Home], world: &World, others: &[&World]) -> AccountsView {
        let room = Room::of(found.to_vec(), world.accounts.clone(), world, others, true);
        view(cfg, &room, world, others, NOW)
    }

    fn cells(v: &AccountsView) -> Vec<Vec<String>> {
        v.cells(|g| g.text(gauge::BAR))
    }

    fn ready(v: &AccountsView) -> Vec<bool> {
        v.rows.iter().map(|r| r.ready).collect()
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
            w.jobs_mut().insert(j.name.clone(), j.clone());
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
    fn the_configured_model_alone_is_one_account_row_and_its_other_meter() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", None), queued("b", Some("minimax-code/MiniMax-M3.1-Flash"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
        let table = v.table(gauge::BAR);
        assert_eq!(table.len(), 3, "a heading, the account and its other meter: {table:#?}");
        assert!(table[0].trim_start().starts_with("state  meter  "), "{}", table[0]);
        let deciding = "▸ omp:default  ready  minimax-code/General 7 Day  ▌░░░░░░░░░│░   4%*";
        assert!(table[1].starts_with(deciding) && table[1].ends_with("14%/d     0"), "{}", table[1]);
        let other = &table[2];
        assert!(other.starts_with(' ') && other.contains("openai-codex/5 hours"), "{other}");
        assert!(other.ends_with("██████████│█ 100%  1h"), "{other}");
        assert_eq!(ready(&v), [true, false]);
        assert!(v.json["accounts"][0].get("byModel").is_none(), "{}", v.json);
    }

    #[test]
    fn a_model_another_projects_job_asks_for_is_judged_too() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let theirs = world(&[queued("a", Some("openai-codex/gpt-6.1-sol")), queued("b", None)]);
        let v = view_of(&cfg, &[omp()], &world(&[]), &[&theirs]);
        let cells = cells(&v);
        assert_eq!(cells.len(), 3, "the account, its other meter, the other model: {cells:?}");
        assert_eq!((cells[2][0].as_str(), cells[2][1].as_str()), ("    for openai-codex/gpt-6.1-sol", "at floor"));
        assert_eq!(ready(&v), [true, false, false]);
    }

    #[test]
    fn a_model_judged_as_its_account_is_no_sub_row_of_its_own() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("minimax-code/MiniMax-M3.5"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
        let models = v.rows.iter().filter(|r| r.account.starts_with("    for")).count();
        assert_eq!(models, 0, "the sub-row would repeat its account's row: {:?}", v.rows);
        assert_eq!(v.json["accounts"][0]["byModel"][0]["model"], "minimax-code/MiniMax-M3.5", "--json keeps it");
    }

    #[test]
    fn a_queued_jobs_model_at_floor_shows_the_hold() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("openai-codex/gpt-6.1-sol")), queued("b", Some("openai-codex/gpt-6.1-sol"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
        let cells = cells(&v);
        let (minimax, codex) = ("▌░░░░░░░░░│░   4%*      ", "██████████│█ 100%* 1h   ");
        let account = ["  omp:default", "ready", "minimax-code/General 7 Day", minimax, "14%/d", "0"];
        assert_eq!(cells[0], account, "the codex jobs are held, so nothing is picked");
        assert_eq!(cells[1], ["", "", "openai-codex/5 hours", "██████████│█ 100%  1h   ", "", ""]);
        assert_eq!(cells[2], ["    for openai-codex/gpt-6.1-sol", "at floor", "openai-codex/5 hours", codex, "—", ""]);
        assert_eq!(ready(&v), [true, false, false]);
        assert!(v.head.ends_with("next pick: none"), "{}", v.head);
        let by_model = &v.json["accounts"][0]["byModel"];
        assert_eq!(by_model[0]["model"], "openai-codex/gpt-6.1-sol");
        assert_eq!(by_model[0]["state"], "at floor");
        assert_eq!(v.json["accounts"][0]["state"], "ready");
    }

    #[test]
    fn the_board_says_which_homes_are_one_account() {
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
        let v = view_of(&cfg, &found, &w, &[]);
        let table = v.text();
        let (first, third) = (&v.json["accounts"][0]["identity"], &v.json["accounts"][2]);
        assert!(table.contains("claude:a  ready") && first["plan"] == "Ultra", "who it is, in --json: {}", v.json);
        assert!(table.contains("claude:b  ready, same sign-in as claude:a"), "{table}");
        assert!(table.contains("signed out since "), "{table}");
        assert_eq!(v.json["accounts"][1]["sameAs"], "claude:a");
        assert!(third.get("sameAs").is_none());
        for secret in ["me@secret.invalid", "u-1", "u-2"] {
            assert!(!table.contains(secret) && !v.json.to_string().contains(secret), "{secret}");
        }
    }

    /// Audit finding 14: the pane judges an account as the dispatcher does, its seats too, so an
    /// account whose seats are full is not `ready`, while one of its other providers still is.
    #[test]
    fn an_account_with_no_free_seat_is_not_ready() {
        let d = tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text =
            "name = \"demo\"\n[limits]\nper_account = 1\n[harness.omp]\nmodel = \"minimax-code/MiniMax-M3.1-Flash\"\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        let running = Job {
            name: "r".into(),
            status: Status::Running,
            account: Some(omp().key()),
            model: Some("minimax-code/MiniMax-M3.1-Flash".into()),
            provider: "minimax-code".into(),
            ..Job::default()
        };
        let v = view_of(&cfg, &[omp()], &world(std::slice::from_ref(&running)), &[]);
        assert_eq!(v.json["accounts"][0]["state"], "busy (1 of 1 seat on minimax-code)", "{}", v.json);
        assert_eq!(v.json["accounts"][0]["running"], 1);
        assert_eq!(ready(&v), [false, false], "the dispatcher would start nothing there on this model");
        assert_eq!(cells(&v)[0][4..], ["14%/d", "1"], "its quota has room for when the seat frees");
        // The same session on another project's bus fills the same seat.
        let theirs = world(&[Job {
            runner: Some((std::process::id(), crate::os::start_time(std::process::id()).unwrap())),
            ..running
        }]);
        let v = view_of(&cfg, &[omp()], &world(&[]), &[&theirs]);
        assert_eq!(ready(&v), [false, false], "{}", v.json);
    }

    fn claude(label: &str) -> Home {
        Home { harness: "claude", label: label.into(), dir: PathBuf::from("/c") }
    }

    /// A claude-only config, its `[limits]` table `limits` (or none).
    fn claude_cfg(limits: &str) -> (tempdir::Dir, Config) {
        let d = tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = format!("name = \"demo\"\n{limits}\n[harness.omp]\nenabled = false\n");
        let cfg = Config::parse(&text, d.path()).unwrap();
        (d, cfg)
    }

    /// A claude account's label and its meters, each `(window, used, hours to its reset)`.
    type Readings<'a> = (&'a str, &'a [(&'a str, f64, i64)]);

    /// The claude accounts `(label, meters)`, read at `read`, each meter `(window, used, hours
    /// to its reset)`; with `jobs`.
    fn claude_world(accounts: &[Readings<'_>], read: u64, jobs: &[Job]) -> (World, Vec<Home>) {
        let mut w = World::default();
        for (label, meters) in accounts {
            let mut st = AccountState { read_at: Some(read), ..Default::default() };
            for &(window, utilization, hours) in *meters {
                st.meters.insert(window.into(), Meter { utilization, resets_at: Some(secs(NOW) + hours * 3600) });
            }
            w.accounts.insert(claude(label).key(), st);
        }
        for j in jobs {
            w.jobs_mut().insert(j.name.clone(), j.clone());
        }
        (w, accounts.iter().map(|(label, _)| claude(label)).collect())
    }

    #[test]
    fn the_meter_that_decides_is_marked_and_on_its_accounts_row() {
        let (_d, cfg) = claude_cfg("");
        let a: &[_] = &[("five_hour", 0.09, 1), ("seven_day", 0.05, 48)];
        let (w, found) = claude_world(&[("a", a), ("b", &[("seven_day", 0.31, 124)])], NOW - 1, &[]);
        let v = view_of(&cfg, &found, &w, &[]);
        let cells = cells(&v);
        assert_eq!(cells[0][..4], ["▸ claude:a", "ready", "5 hours", "█▏░░░░░░░░│░   9%* 1h   "]);
        assert_eq!(cells[1], ["", "", "7 days", "▋░░░░░░░░░│░   5%  2d   ", "", ""], "a sub-row");
        assert_eq!(cells[2][..4], ["  claude:b", "ready", "7 days", "███▊░░░░░░│░  31%* 5d4h "]);
        assert_eq!(cells.len(), 3, "a window an account lacks is no row: {cells:?}");
        assert!(v.text().lines().nth(1).unwrap().trim_start().starts_with("state  meter  "), "{}", v.text());
    }

    #[test]
    fn a_meter_near_its_reset_moves_its_floor_mark() {
        let (_d, cfg) = claude_cfg("[limits]\nfloor_near_reset = { within_hours = 12, floor = 0.98 }\n");
        let (w, found) = claude_world(&[("a", &[("five_hour", 0.5, 2), ("seven_day", 0.5, 48)])], NOW - 1, &[]);
        let v = view_of(&cfg, &found, &w, &[]);
        let gauge = |meter: &str| v.rows.iter().find(|r| r.meter == meter).and_then(|r| r.gauge.clone()).unwrap();
        let mark = |meter: &str| gauge(meter).bar(12).chars().position(|c| c == '│');
        assert_eq!((mark("5 hours"), mark("7 days")), (Some(11), Some(10)), "98% two hours from its reset, else 90%");
        assert!(v.head.contains("floor 90% (98% in the last 12h before a reset)"), "{}", v.head);
    }

    #[test]
    fn the_next_pick_is_marked_where_dispatch_would_start() {
        let (_d, cfg) = claude_cfg("[limits]\nmin_free_disk_gb = 0\n");
        let accounts: &[Readings<'_>] =
            &[("a", &[("seven_day", 0.2, 144)]), ("b", &[("seven_day", 0.5, 24)]), ("c", &[("seven_day", 0.95, 24)])];
        let (w, found) = claude_world(accounts, NOW - 1, &[]);
        let v = view_of(&cfg, &found, &w, &[]);
        let room = Room::of(found.clone(), w.accounts.clone(), &w, &[], true);
        let (_, picked) = crate::systems::dispatch::next_pick(&cfg, &room, &w, NOW).unwrap();
        assert_eq!(picked, "claude:b", "room that a reset would take first");
        let marked: Vec<&str> = v.rows.iter().filter(|r| r.account.starts_with('▸')).map(|r| &r.account[4..]).collect();
        assert_eq!(marked, [picked.as_str()]);
        assert_eq!(cells(&v).iter().map(|r| r[4].as_str()).collect::<Vec<_>>(), ["13%/d", "50%/d", "—"]);
        assert!(v.head.ends_with("next pick ▸ claude:b"), "{}", v.head);
        assert_eq!(v.json["next"], "claude:b");
        assert!((v.json["accounts"][1]["score"].as_f64().unwrap() - 0.5).abs() < 1e-9, "{}", v.json);
        // A job that resumes a session goes back to its account, and the mark follows it there.
        let resume = Job {
            name: "q".into(),
            status: Status::Queued,
            session_id: Some("s".into()),
            transcript_account: Some("claude:a".into()),
            ..Job::default()
        };
        let (w, found) = claude_world(accounts, NOW - 1, &[resume]);
        let v = view_of(&cfg, &found, &w, &[]);
        assert!(v.rows[0].account.starts_with("▸ claude:a") && v.head.ends_with("▸ claude:a for q"), "{}", v.text());
    }

    #[test]
    fn a_stale_row_says_how_old_its_reading_is_and_keeps_its_sessions() {
        let (_d, cfg) = claude_cfg("");
        let running =
            Job { name: "r".into(), status: Status::Running, account: Some("claude:a".into()), ..Job::default() };
        let (w, found) = claude_world(&[("a", &[("seven_day", 0.81, 41)])], NOW - 3 * 3600 * 1_000_000, &[running]);
        let v = view_of(&cfg, &found, &w, &[]);
        let row = &cells(&v)[0];
        assert!(row[1].ends_with("stale (read 3h ago)"), "{row:?}");
        assert_eq!(row[2..4], ["7 days", "█████████▊│░  81%* 1d17h"], "its last reading still shows");
        assert_eq!(row[4..], ["—", "1"]);
    }

    /// The meters this machine's harnesses really report: omp's per provider, claude's with an
    /// overage window, puku's monthly one.
    fn real_meters() -> (World, Vec<Home>) {
        let reading = |meters: &[(&str, f64, i64)]| {
            let mut st = AccountState { read_at: Some(NOW - 1), ..Default::default() };
            for &(window, utilization, minutes) in meters {
                let resets_at = Some(secs(NOW) + minutes * 60);
                st.meters.insert(window.into(), Meter { utilization, resets_at });
            }
            st
        };
        let (day, hour) = (24 * 60, 60);
        let omp_st = reading(&[
            ("google-antigravity/Claude & GPT (shared) (Weekly)", 0.42, 3 * day),
            ("google-antigravity/Gemini (Weekly)", 0.10, 3 * day),
            ("minimax-code/General 5 Hour", 0.05, 2 * hour),
            ("minimax-code/General 7 Day", 0.04, 5 * day),
            ("openai-codex/5 hours", 0.0, 5 * hour),
            ("openai-codex/7 days", 0.19, 5 * day + 4 * hour),
        ]);
        let tj = reading(&[
            ("five_hour", 0.44, 10),
            ("seven_day", 0.38, 2 * day + 16 * hour),
            ("seven_day_overage_included", 0.13, 2 * day + 16 * hour),
        ]);
        let puku = reading(&[("five_hour", 0.07, 3 * hour), ("monthly", 0.31, 29 * day + 23 * hour)]);
        let tj_home = claude("tj.pro");
        let puku_home = Home { harness: "puku", label: "p".into(), dir: PathBuf::from("/p") };
        let mut w = World::default();
        w.accounts.insert(tj_home.key(), tj);
        w.accounts.insert(omp().key(), omp_st);
        w.accounts.insert(puku_home.key(), puku);
        (w, vec![tj_home, omp(), puku_home])
    }

    /// One row per account with the meter that decides, then a sub-row per other meter, the
    /// closest to its floor first: as wide as the longest meter name, not one column per name.
    #[test]
    fn each_meter_is_a_row_of_its_account_the_deciding_one_first() {
        let (_d, cfg) = cfg("openai-codex/gpt-6-luna");
        let (w, found) = real_meters();
        let v = view_of(&cfg, &found, &w, &[]);
        let table = v.table(gauge::BAR);
        assert_eq!(table.len(), 1 + 3 + 6 + 2, "a heading, then every meter once: {table:#?}");
        assert!(table[0].trim_start().starts_with("state  meter") && table[0].contains("gauge"), "{}", table[0]);
        let meter_of = |line: &str, name: &str| line.contains(&format!("  {name}  "));
        assert!(table[1].contains("claude:tj.pro") && meter_of(&table[1], "5 hours") && table[1].contains("44%*"));
        assert!(meter_of(&table[2], "7 days") && meter_of(&table[3], "seven day overage included"), "{table:#?}");
        assert!(table[4].contains("omp:default") && meter_of(&table[4], "openai-codex/7 days"), "{}", table[4]);
        assert!(meter_of(&table[5], "google-antigravity/Claude & GPT (shared) (Weekly)"), "the fullest other next");
        assert!(meter_of(&table[9], "openai-codex/5 hours"), "the emptiest last: {}", table[9]);
        for sub in [2, 3, 5, 6, 7, 8, 9, 11] {
            assert!(table[sub].starts_with("   ") && !table[sub].contains('*'), "{}", table[sub]);
        }
        let widest = table.iter().map(|l| l.chars().count()).max().unwrap();
        assert!(widest < 120, "{widest}: {table:#?}");
    }

    #[test]
    fn a_narrow_window_shrinks_the_bars_before_it_cuts_lines() {
        let (_d, cfg) = claude_cfg("");
        let (w, found) = claude_world(&[("a", &[("five_hour", 0.3, 3), ("seven_day", 0.5, 48)])], NOW - 1, &[]);
        let v = view_of(&cfg, &found, &w, &[]);
        let widest = |t: &str| t.lines().skip(1).map(|l| l.chars().count()).max().unwrap();
        let wide = widest(&v.text());
        assert_eq!(widest(&v.text_in(Some(wide - 6))), wide - 6, "the bars six cells shorter");
        assert_eq!(widest(&v.text_in(Some(10))), wide - 7, "no bar under five cells; the rest is cut");
    }

    /// A request for an account already pending adds nothing; the drainer takes them all at once.
    #[test]
    fn pending_requests_are_a_set_the_drainer_empties() {
        let d = tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"demo\"\n", d.path()).unwrap();
        let ctx = Ctx::unclaimed(cfg, d.path().join("relay.toml"), d.path().join("state"), d.path().join("data"));
        std::fs::create_dir_all(&ctx.state).unwrap();
        let pending = Pending(&ctx);
        assert!(pending.is_empty().unwrap());
        pending.add(&["claude:a".into()]).unwrap();
        pending.add(&["claude:b".into(), "claude:a".into()]).unwrap();
        let taken: Vec<String> = pending.take().unwrap().into_iter().collect();
        assert_eq!(taken, ["claude:a", "claude:b"]);
        assert!(pending.is_empty().unwrap() && pending.take().unwrap().is_empty());
    }

    #[test]
    fn the_state_cell_does_not_repeat_the_meter() {
        let (_d, cfg) = cfg("openai-codex/gpt-6.1-sol");
        let v = view_of(&cfg, &[omp()], &world(&[]), &[]);
        let cells = cells(&v);
        assert_eq!(cells[0][1], "at floor");
        assert_eq!(cells[0][2..4], ["openai-codex/5 hours", "██████████│█ 100%* 1h   "], "the meter that decides");
    }
}
