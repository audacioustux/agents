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

/// How an account stands for `model`: its state cell, the meter that decides, and whether it
/// would take the job, by the dispatcher's own judgment ([`Room::admit`]): a full seat is not ready.
fn judge(room: &Room, a: &Home, model: Option<&str>, limits: &Limits, now: u64) -> (String, String, Floor, bool) {
    let h = harness_of(a);
    let st = room.account(&a.key());
    let admitted = room.admit(limits, a, model, now);
    let ready = admitted.is_ok();
    let state = match admitted {
        Ok(_) if !h.caps().quota => "ready (unmetered)".to_owned(),
        Ok(_) => "ready".to_owned(),
        Err(r) => r.busy.into_iter().chain(r.fit.as_ref().map(|f| refusal_cell(f, st))).collect::<Vec<_>>().join(", "),
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

/// A state cell for what holds an account back.
fn refusal_cell(r: &accounts::Refusal, st: Option<&AccountState>) -> String {
    match r {
        accounts::Refusal::SignedOut => {
            format!("signed out since {}", accounts::fmt_time(secs(st.and_then(|s| s.signed_at).unwrap_or(0))))
        }
        accounts::Refusal::Limited => {
            format!("limited until {}", accounts::fmt_time(st.and_then(|s| s.limited_until).unwrap_or(0)))
        }
        // The meter cell names the meter.
        accounts::Refusal::AtFloor(_) => "at floor".to_owned(),
        r => r.describe(),
    }
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
    /// Says that a reading of `accounts` is being taken, in the head and in `--json` (`refreshing`).
    pub fn refreshing(&mut self, accounts: Vec<String>) {
        if !accounts.is_empty() {
            self.head = format!("{}; refreshing {}", self.head, accounts.join(", "));
            self.json["refreshing"] = accounts.into();
        }
    }

    pub fn text(&self) -> String {
        let lines = crate::systems::view::table(&self.cells).into_iter().map(|l| format!("  {l}"));
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
    fn missing(&mut self, missing: &[(&str, String)]) {
        for (id, what) in missing {
            self.cells.push(vec![
                (*id).to_owned(),
                String::new(),
                format!("program not found: {what}"),
                String::new(),
                String::new(),
            ]);
            self.ready.push(false);
        }
        self.json["missing"] = missing.iter().map(|(id, _)| *id).collect::<Vec<_>>().into();
    }
}

/// Every account on the machine as all projects know it, judged by none of them.
fn machine_wide(room: &Room, now: u64) -> AccountsView {
    let found = room.found();
    let rows: Vec<Row<'_>> = found
        .iter()
        .map(|a| {
            let key = a.key();
            let st = room.account(&key);
            let state = accounts::shared_refusal(harness_of(a), st, now)
                .map_or_else(|| "not limited".to_owned(), |r| refusal_cell(&r, st));
            let tightest = st
                .and_then(|s| {
                    let all: Vec<&crate::harness::Meter> = s.meters.values().collect();
                    accounts::tightest(&all, &s.meters, secs(now))
                })
                .map_or(String::new(), |(k, m)| accounts::meter_line(k, m));
            Row {
                same_as: room.twin_of(&key).cloned(),
                identity: match accounts::sign_in(a) {
                    SignIn::As(id) => Some(id),
                    SignIn::Out | SignIn::Unknown => None,
                },
                running: room.running_on(&key),
                account: key,
                state,
                meters: st.map(|s| &s.meters),
                read_at: st.and_then(|s| s.read_at),
                tightest,
                floor: Floor { floor: None, floor_why: None },
                ready: false,
                by_model: Vec::new(),
            }
        })
        .collect();
    let mut names: Vec<&str> = found.iter().map(|a| a.harness).collect();
    names.dedup();
    let head = format!(
        "accounts on {}, machine-wide (each project judges its own floor, seats and exclusions)",
        if names.is_empty() { "no harness".to_owned() } else { names.join(", ") }
    );
    let (cells, ready) = rows.iter().flat_map(Row::cells).unzip();
    let json = serde_json::json!({ "machineWide": true, "harnesses": names, "accounts": rows });
    AccountsView { json, head, cells, ready }
}

/// The model a running job runs, as its dispatch recorded it, else (a record from before the
/// model was) the one it names on `harness`.
fn running_model(j: &Job, harness: &str) -> Option<String> {
    j.model.clone().or_else(|| j.models.get(harness).cloned())
}

/// The view of the accounts in `room`, with the models the jobs of `world` and of the other
/// projects (`others`) ask for, at `now` (bus time).
fn view(cfg: &Config, room: &Room, world: &World, others: &[&World], now: u64) -> AccountsView {
    let limits = &cfg.limits;
    let row = |a: &Home| {
        let key = a.key();
        let st = room.account(&key);
        // The dispatcher's own judgment, for the configured model and every other one its jobs ask for.
        let model = cfg.harness(a.harness).model;
        let (state, tightest, floor, ready) = judge(room, a, model.as_deref(), limits, now);
        let runs_here = |j: &&Job| j.status == Status::Running && j.account.as_ref() == Some(&key);
        // A running job asks for what its dispatch recorded, whichever project it is. A queued job
        // of another project that names no model asks for its own config's, which is not known
        // here; one that names a model asks for it on this machine's accounts like ours do.
        let running = world.jobs().values().chain(others.iter().flat_map(|w| w.jobs().values())).filter(runs_here);
        let queued = |j: &&Job| j.status == Status::Queued && may_use(j, a);
        let ours = world.jobs().values().filter(queued).filter_map(|j| model_for(cfg, j, a.harness));
        let theirs =
            others.iter().flat_map(|w| w.jobs().values()).filter(queued).filter_map(|j| j.models.get(a.harness));
        let asked: BTreeSet<String> = running
            .filter_map(|j| running_model(j, a.harness))
            .chain(ours)
            .chain(theirs.cloned())
            .filter(|m| Some(m) != model.as_ref())
            .collect();
        let by_model = asked
            .into_iter()
            .map(|m| {
                let (state, tightest, floor, ready) = judge(room, a, Some(&m), limits, now);
                Judged { model: m, state, tightest, floor, ready }
            })
            .collect();
        let identity = match accounts::sign_in(a) {
            SignIn::As(id) => Some(id),
            SignIn::Out | SignIn::Unknown => None,
        };
        Row {
            // Twins share their seats: a session on either is a session on both.
            running: room.running_on(&key),
            same_as: room.twin_of(&key).cloned(),
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
    let rows: Vec<Row<'_>> = room.found().iter().map(row).collect();
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
    fn the_configured_model_alone_renders_as_one_row() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", None), queued("b", Some("minimax-code/MiniMax-M3.1-Flash"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
        assert_eq!(crate::systems::view::table(&v.cells), ["omp:default  ready  4% minimax-code/General 7 Day"]);
        assert_eq!(v.ready, [true]);
        assert!(v.json["accounts"][0].get("byModel").is_none(), "{}", v.json);
    }

    #[test]
    fn a_model_another_projects_job_asks_for_is_judged_too() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let theirs = world(&[queued("a", Some("openai-codex/gpt-6.1-sol")), queued("b", None)]);
        let v = view_of(&cfg, &[omp()], &world(&[]), &[&theirs]);
        assert_eq!(v.cells.len(), 2, "{:?}", v.cells);
        assert_eq!((v.cells[1][0].as_str(), v.cells[1][2].as_str()), ("  for openai-codex/gpt-6.1-sol", "at floor"));
        assert_eq!(v.ready, [true, false]);
    }

    #[test]
    fn a_model_judged_as_its_account_is_no_sub_row_of_its_own() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("minimax-code/MiniMax-M3.5"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
        assert_eq!(v.cells.len(), 1, "the sub-row would repeat its account's row: {:?}", v.cells);
        assert_eq!(v.json["accounts"][0]["byModel"][0]["model"], "minimax-code/MiniMax-M3.5", "--json keeps it");
    }

    #[test]
    fn a_queued_jobs_model_at_floor_shows_the_hold() {
        let (_d, cfg) = cfg("minimax-code/MiniMax-M3.1-Flash");
        let jobs = [queued("a", Some("openai-codex/gpt-6.1-sol")), queued("b", Some("openai-codex/gpt-6.1-sol"))];
        let v = view_of(&cfg, &[omp()], &world(&jobs), &[]);
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
        let v = view_of(&cfg, &found, &w, &[]);
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
        assert_eq!(v.ready, [false], "the dispatcher would start nothing there on this model");
        // The same session on another project's bus fills the same seat.
        let theirs = world(&[Job {
            runner: Some((std::process::id(), crate::os::start_time(std::process::id()).unwrap())),
            ..running
        }]);
        let v = view_of(&cfg, &[omp()], &world(&[]), &[&theirs]);
        assert_eq!(v.ready, [false], "{}", v.json);
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
        let resets = accounts::fmt_time(secs(NOW) + 3_600);
        assert_eq!(v.cells[0][2], "at floor");
        assert_eq!(v.cells[0][4], format!("100% openai-codex/5 hours, resets {resets}"));
    }
}
