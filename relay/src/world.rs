//! The state every system works from, derived by folding the bus. The only
//! thing stored is a checkpoint of that fold (`world.json`): a cache, so a
//! command reads only what was written since, never the source of truth.

use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{
    self, Action, DecideEvent, DispatchEvent, Event, Fork, LandEvent, Meters, Offsets, Outcome, Point, QueueEvent,
    QuotaEvent, SessionEvent, Stamped,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    #[default]
    Queued,
    Running,
    /// Stopped at its spend cap while its progress is checked.
    Checking,
    Done,
    Failed,
    Stopped,
}

impl Status {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Checking)
    }

    /// A runner process holds the job: it is running or being checked.
    pub fn has_runner(self) -> bool {
        matches!(self, Self::Running | Self::Checking)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Checking => "checking",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    pub task: String,
    pub status: Status,
    pub readonly: bool,
    /// Harness ids it may run on; empty means any enabled harness.
    pub harnesses: Vec<String>,
    /// Model per harness id.
    pub models: BTreeMap<String, String>,
    pub effort: Option<String>,
    pub budget_usd: f64,
    pub pinned: Option<String>,
    /// See `QueueEvent::Queued`.
    pub reuse_key: Option<String>,
    /// Jobs this one still waits for: those it was queued after, less any released
    /// and any that were done when cleaned away.
    #[serde(default)]
    pub after: Vec<String>,
    /// The profile it was queued under, which its limits (`max_running`, `max_per_day`) count.
    #[serde(default)]
    pub profile: Option<String>,
    /// The clean commit the last attempt of a read-only job read.
    #[serde(default)]
    pub ran_at: Option<String>,
    pub account: Option<String>,
    /// The session the last attempt ran in, once one started.
    pub session_id: Option<String>,
    /// The account (`harness:label`) that last wrote this session.
    pub transcript_account: Option<String>,
    pub message: Option<String>,
    pub follow_ups: u32,
    pub reruns: u32,
    pub attempts: u32,
    /// The session runner's pid and start time, together naming one process.
    pub runner: Option<(u32, u64)>,
    pub workdir: Option<PathBuf>,
    pub branch: Option<String>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub cost_usd: f64,
    pub turns: u32,
    pub reason: Option<String>,
    pub stop_requested: bool,
    /// Every checkpoint at a spend cap, oldest first.
    #[serde(default)]
    pub checkpoints: Vec<bus::Check>,
    /// The session it starts as a fork of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<Fork>,
    /// When it last went into the queue (bus time): queued, resumed, or back from a run.
    #[serde(default)]
    pub queued_at: u64,
    /// A decision let it past its profile's limits.
    #[serde(default)]
    pub forced: bool,
    /// The clone's size when it was last measured while the job ran, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
    /// What gc removed from the clone: `outputs` (the build outputs) or `clone` (all of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected: Option<String>,
    /// The job a decision queued this one to follow up.
    #[serde(default)]
    pub parent: Option<String>,
    /// How many follow-ups deep it is.
    #[serde(default)]
    pub depth: u32,
}

impl Job {
    /// Resumed by hand during its check: what its runner still says changes
    /// only what was spent, and the owner's instruction stands.
    fn let_go(&self) -> bool {
        self.status == Status::Queued && self.runner.is_none() && self.attempts > 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    pub enabled: bool,
    pub signed_out: bool,
    /// When the sign-in state last changed (signed out, or enabled again):
    /// across projects, the latest change wins.
    #[serde(default)]
    pub signed_at: Option<u64>,
    pub meters: Meters,
    /// Unix microseconds of the last reading.
    pub read_at: Option<u64>,
    /// Unix microseconds of the last attempt that read nothing.
    pub unreadable_at: Option<u64>,
    /// Unix seconds until which the account refuses sessions.
    pub limited_until: Option<i64>,
    /// The home's sign-in (`Identity::stamp`) when it was last read or found
    /// signed out. A different one on disk means someone signed in again.
    #[serde(default)]
    pub sign_in: Option<String>,
    /// The home holds no signed-in account. Learned from its files on every
    /// pass (`accounts::settle`), never from the bus.
    #[serde(skip)]
    pub unsigned: bool,
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            enabled: true,
            signed_out: false,
            signed_at: None,
            meters: Meters::new(),
            read_at: None,
            unreadable_at: None,
            limited_until: None,
            sign_in: None,
            unsigned: false,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub jobs: BTreeMap<String, Job>,
    pub accounts: BTreeMap<String, AccountState>,
    /// When (bus time, microseconds) each profile's sessions started, within a day
    /// of the latest start. Kept apart from the jobs, which can be cleaned away.
    #[serde(default)]
    pub profile_starts: BTreeMap<String, Vec<u64>>,
    /// Decisions by id, while their job is kept.
    #[serde(default)]
    pub decisions: BTreeMap<String, Decision>,
}

/// One decision point reached, and what its handlers answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub job: String,
    pub point: Point,
    pub created: u64,
    pub attempt: u32,
    pub requested_at: u64,
    /// Set while it waits for `relay decide answer`: until when (bus time).
    pub parked_until: Option<u64>,
    /// The orchestrator's answer, the first one recorded.
    pub answer: Option<Action>,
    /// Each handler's answer, in order.
    pub steps: Vec<Step>,
    /// The action carried out, once a handler gave one that passed.
    pub action: Option<Action>,
}

/// One handler's answer to a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub handler: String,
    pub action: Action,
    pub why: String,
    pub cost_usd: f64,
    pub refused: Option<String>,
}

impl Decision {
    /// Waiting for the orchestrator.
    pub fn pending(&self) -> bool {
        self.parked_until.is_some() && self.answer.is_none() && self.action.is_none()
    }
}

/// The window a profile's `max_per_day` counts over.
pub const DAY_MICROS: u64 = 24 * 3600 * 1_000_000;

/// Bump whenever the fold's meaning changes (`apply`, `current`, what `World`
/// holds, how a mark is kept), so checkpoints folded the old way are discarded.
const FOLD_VERSION: u32 = 9;

fn fold_version() -> String {
    format!("{}+fold{FOLD_VERSION}", env!("CARGO_PKG_VERSION"))
}

/// The fold up to `offsets`, stored as `<state>/world.json`.
#[derive(Debug, Serialize, Deserialize)]
struct Checkpoint {
    version: String,
    /// The latest event time folded in. A new event no later than this
    /// arrived out of order, and only a full fold places it right.
    last: u64,
    offsets: Offsets,
    world: World,
}

impl Checkpoint {
    /// The checkpoint at `file` brought up to date with `bus` (or a full fold
    /// when it is unusable), with the bytes `file` held.
    fn current(bus: &Path, file: &Path) -> Result<(Self, Vec<u8>)> {
        let old = std::fs::read(file).unwrap_or_default();
        let cached = serde_json::from_slice::<Self>(&old).ok().filter(|c| c.version == fold_version());
        let cp = match cached {
            Some(c) => c.advance(bus)?,
            None => None,
        };
        let cp = match cp {
            Some(c) => c,
            None => Self::full(bus)?,
        };
        Ok((cp, old))
    }

    fn full(bus: &Path) -> Result<Self> {
        let (events, offsets) = bus::read_since(bus, &Offsets::new())?.unwrap_or_default();
        let last = events.iter().map(|e| e.t).max().unwrap_or(0);
        Ok(Self { version: fold_version(), last, offsets, world: World::fold(&events) })
    }

    /// Folds what was written since, or `None` if only a full fold will do.
    fn advance(mut self, bus: &Path) -> Result<Option<Self>> {
        let Some((events, offsets)) = bus::read_since(bus, &self.offsets)? else { return Ok(None) };
        if events.first().is_some_and(|e| e.t <= self.last) {
            return Ok(None);
        }
        for e in &events {
            self.world.apply(e);
        }
        self.last = events.last().map_or(self.last, |e| e.t);
        self.offsets = offsets;
        Ok(Some(self))
    }
}

/// One account as two projects know it together: the later reading, the later
/// failed read, the later end of a limit, and signed out if either saw it.
/// Whether relay may use it at all (`enabled`) is each project's own switch.
pub fn merge_account(ours: &AccountState, theirs: &AccountState) -> AccountState {
    let newer = if theirs.read_at > ours.read_at { theirs } else { ours };
    let (meters, read_at, sign_in) = (newer.meters.clone(), newer.read_at, newer.sign_in.clone());
    // A sign-out stands until someone enables the account again, in any project.
    let latest = if theirs.signed_at > ours.signed_at { theirs } else { ours };
    AccountState {
        enabled: ours.enabled,
        signed_out: latest.signed_out,
        signed_at: latest.signed_at,
        meters,
        read_at,
        unreadable_at: ours.unreadable_at.max(theirs.unreadable_at),
        limited_until: ours.limited_until.max(theirs.limited_until),
        sign_in,
        unsigned: ours.unsigned,
    }
}

/// Writes `bytes` to `path` whole or not at all, readable by its owner only.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A job another job still waits for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Wait {
    pub job: String,
    /// What the dependency is doing: queued, running, or how it ended (failed,
    /// stopped, dequeued, gone).
    pub state: String,
    /// It has ended without being done, so the waiting job never starts on its own.
    pub blocked: bool,
}

impl Wait {
    /// One line for a board: `after a, b` while they work, `waits for a (failed)` once one cannot finish.
    pub fn summary(waits: &[Self]) -> Option<String> {
        let names = |w: &[&Self]| w.iter().map(|w| w.job.as_str()).collect::<Vec<_>>().join(", ");
        let blocked: Vec<_> = waits.iter().filter(|w| w.blocked).collect();
        if blocked.is_empty() {
            let all: Vec<_> = waits.iter().collect();
            return (!all.is_empty()).then(|| format!("after {}", names(&all)));
        }
        let reasons: Vec<_> = blocked.iter().map(|w| format!("{} ({})", w.job, w.state)).collect();
        Some(format!("waits for {}", reasons.join(", ")))
    }
}

impl World {
    /// The jobs `job` still waits for. A dependency that is done, or was done
    /// when it was cleaned away, is not among them; landing it is a later step.
    pub fn waits(&self, job: &Job) -> Vec<Wait> {
        let wait = |name: &String, state: &str, blocked| Wait { job: name.clone(), state: state.into(), blocked };
        job.after
            .iter()
            .filter_map(|a| match self.jobs.get(a) {
                Some(d) if d.status == Status::Done => None,
                Some(d) if d.status.is_live() => Some(wait(a, d.status.as_str(), false)),
                Some(d) if d.reason.as_deref() == Some("dequeued") => Some(wait(a, "dequeued", true)),
                Some(d) => Some(wait(a, d.status.as_str(), true)),
                None => Some(wait(a, "gone", true)),
            })
            .collect()
    }

    /// Whether `job` is queued with every dependency done: the dispatcher may give it a seat.
    pub fn startable(&self, job: &Job) -> bool {
        job.status == Status::Queued && self.waits(job).is_empty()
    }

    /// Whether `job` is queued and can never start on its own until released or its dependency is queued again.
    pub fn blocked(&self, job: &Job) -> bool {
        job.status == Status::Queued && self.waits(job).iter().any(|w| w.blocked)
    }

    /// The fold of the bus at `bus`, from the checkpoint at `file` when it is
    /// usable, which is then brought up to date. Any process may do this: the
    /// checkpoint is a cache of a deterministic fold, replaced atomically, and
    /// rewritten only when it changed. A failure to write it costs only time.
    pub fn load(bus: &Path, file: &Path) -> Result<Self> {
        let (cp, old) = Checkpoint::current(bus, file)?;
        if let Ok(bytes) = serde_json::to_vec(&cp)
            && bytes != old
        {
            let _ = write_atomic(file, &bytes);
        }
        Ok(cp.world)
    }

    /// As `load`, for a bus this process only listens to: the checkpoint at
    /// `file` is its owner's cache, so it is read but never written.
    pub fn peek(bus: &Path, file: &Path) -> Result<Self> {
        Checkpoint::current(bus, file).map(|(cp, _)| cp.world)
    }

    /// Takes in what other projects on this machine know about the accounts
    /// they share with this one (`merge_account`); their jobs stay theirs.
    pub fn hear_accounts(&mut self, others: &[World]) {
        for (key, theirs) in others.iter().flat_map(|w| &w.accounts) {
            let ours = self.accounts.entry(key.clone()).or_default();
            *ours = merge_account(ours, theirs);
        }
    }

    pub fn fold(events: &[Stamped]) -> Self {
        let mut w = Self::default();
        for e in events {
            w.apply(e);
        }
        w
    }

    fn account(&mut self, label: &str) -> &mut AccountState {
        self.accounts.entry(label.to_owned()).or_default()
    }

    /// Whether a session event comes from its job's current attempt, the only
    /// writer the fold listens to. Events from before files were per attempt have no source.
    fn current(&self, s: &Stamped, e: &SessionEvent) -> bool {
        let Some(o) = &s.from else { return true };
        e.job().is_none_or(|j| j == o.job)
            && self
                .jobs
                .get(&o.job)
                .is_some_and(|j| j.attempts == o.attempt && o.created.is_none_or(|c| c == j.created_at))
    }

    fn apply(&mut self, s: &Stamped) {
        match &s.event {
            Event::Queue(e) => self.apply_queue(s.t, e),
            Event::Dispatch(e) => self.apply_dispatch(s.t, e),
            Event::Session(e) if self.current(s, e) => self.apply_session(s.t, e),
            Event::Session(_) => {}
            Event::Quota(e) => self.apply_quota(s.t, e),
            Event::Decide(e) => self.apply_decide(s.t, e),
            Event::Land(LandEvent::Forgotten { job }) => {
                self.decisions.retain(|_, d| d.job != *job);
                // A job cleaned away after it finished has been waited for.
                if self.jobs.remove(job).is_some_and(|j| j.status == Status::Done) {
                    self.jobs.values_mut().for_each(|j| j.after.retain(|a| a != job));
                }
            }
            Event::Land(LandEvent::Collected { job, what, .. }) => {
                if let Some(j) = self.jobs.get_mut(job) {
                    // Without a clone, a resume makes a new one (from the rescued branch, when there is one).
                    if what == "clone" {
                        j.workdir = None;
                    }
                    j.collected = Some(what.clone());
                }
            }
        }
    }

    fn apply_queue(&mut self, t: u64, e: &QueueEvent) {
        match e {
            QueueEvent::Queued {
                job,
                task,
                readonly,
                harnesses,
                models,
                effort,
                budget_usd,
                pinned,
                reuse_key,
                after,
                profile,
                fork,
                parent,
                depth,
            } => {
                let j = Job {
                    name: job.clone(),
                    task: task.clone(),
                    readonly: *readonly,
                    harnesses: harnesses.clone(),
                    models: models.clone(),
                    effort: effort.clone(),
                    budget_usd: *budget_usd,
                    pinned: pinned.clone(),
                    reuse_key: reuse_key.clone(),
                    after: after.clone(),
                    profile: profile.clone(),
                    fork: fork.as_deref().cloned(),
                    parent: parent.clone(),
                    depth: *depth,
                    created_at: t,
                    queued_at: t,
                    ..Job::default()
                };
                self.jobs.insert(job.clone(), j);
            }
            QueueEvent::Released { job, from } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.after.retain(|a| !from.is_empty() && !from.contains(a));
                }
            }
            QueueEvent::Resumed { job, message, budget_usd, models } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    // Resumed by hand during its check: the checking runner no longer speaks for it.
                    if j.status == Status::Checking {
                        j.runner = None;
                    }
                    j.status = Status::Queued;
                    j.queued_at = t;
                    j.message = Some(message.clone());
                    j.reason = None;
                    j.stop_requested = false;
                    j.follow_ups = 0;
                    j.reruns = 0;
                    j.budget_usd = budget_usd.unwrap_or(j.budget_usd);
                    j.models.extend(models.clone());
                }
            }
            QueueEvent::Rerouted { job, harnesses, models, profile } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.harnesses.clone_from(harnesses);
                    j.models.clone_from(models);
                    j.profile.clone_from(profile);
                    j.queued_at = t;
                }
            }
            QueueEvent::Forced { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.forced = true;
                }
            }
            QueueEvent::Dequeued { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    (j.status, j.reason, j.ended_at) = (Status::Stopped, Some("dequeued".into()), Some(t));
                }
            }
            QueueEvent::StopRequested { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.stop_requested = true;
                }
            }
        }
    }

    fn apply_dispatch(&mut self, t: u64, e: &DispatchEvent) {
        match e {
            DispatchEvent::Dispatched { job, account, runner_pid, runner_started } => {
                if let Some(profile) = self.jobs.get(job).and_then(|j| j.profile.clone()) {
                    let starts = self.profile_starts.entry(profile).or_default();
                    starts.push(t);
                    starts.retain(|&s| s + DAY_MICROS > t);
                }
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Running;
                    j.account = Some(account.clone());
                    j.runner = Some((*runner_pid, *runner_started));
                    j.started_at = Some(t);
                    j.attempts += 1;
                    j.reason = None;
                    j.forced = false;
                }
            }
            DispatchEvent::Reaped { job } => {
                // A runner can report and exit between dispatch's snapshot and its liveness check.
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.status.has_runner()) {
                    // A stop kills the runner, so its silent death is the stop.
                    (j.status, j.reason) = if j.stop_requested {
                        (Status::Stopped, Some("stopped by request".into()))
                    } else {
                        (Status::Failed, Some("session runner died without reporting".into()))
                    };
                    j.ended_at = Some(t);
                }
            }
        }
    }

    fn apply_quota(&mut self, t: u64, e: &QuotaEvent) {
        match e {
            QuotaEvent::AccountEnabled { account, enabled } => {
                let a = self.account(account);
                (a.enabled, a.signed_out, a.signed_at) = (*enabled, false, Some(t));
                // Enabling is the owner saying it is signed in: read it now, not after a back-off.
                a.unreadable_at = None;
            }
            QuotaEvent::Reading { account, meters, sign_in } => {
                let a = self.account(account);
                a.meters.clone_from(meters);
                a.read_at = Some(t);
                a.sign_in.clone_from(sign_in);
                // A reading that works is a session that works.
                if a.signed_out {
                    (a.signed_out, a.signed_at) = (false, Some(t));
                }
            }
            QuotaEvent::SignedOut { account, sign_in } => {
                let a = self.account(account);
                (a.signed_out, a.signed_at) = (true, Some(t));
                // Asked again after a reading's worth of time, not on every pass.
                a.unreadable_at = Some(t);
                a.sign_in.clone_from(sign_in);
            }
            QuotaEvent::Unreadable { account } => self.account(account).unreadable_at = Some(t),
        }
    }

    fn apply_session(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::Cloned { job, workdir, branch } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.workdir = Some(workdir.clone());
                    j.branch = Some(branch.clone());
                    j.collected = None;
                }
            }
            SessionEvent::Started { job, account, session_id, commit } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.ran_at.clone_from(commit);
                    j.transcript_account = Some(account.clone());
                    j.session_id = Some(session_id.clone());
                    j.message = None;
                }
            }
            SessionEvent::Disk { job, bytes } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.disk_bytes = Some(*bytes);
                }
            }
            SessionEvent::Identified { job, session_id } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.session_id = Some(session_id.clone());
                }
            }
            SessionEvent::Rate { .. } | SessionEvent::Limited { .. } | SessionEvent::SignedOut { .. } => {
                self.apply_account_fact(t, e);
            }
            SessionEvent::FollowUp { job, message, budget_usd } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.message = Some(message.clone());
                    j.follow_ups += 1;
                    j.budget_usd = budget_usd.unwrap_or(j.budget_usd);
                }
            }
            SessionEvent::Requeued { job, profile, harnesses, models, effort, budget_usd } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.profile = Some(profile.clone());
                    j.harnesses.clone_from(harnesses);
                    j.models.clone_from(models);
                    j.effort.clone_from(effort);
                    j.budget_usd = *budget_usd;
                    (j.session_id, j.transcript_account, j.message, j.fork) = (None, None, None, None);
                    (j.follow_ups, j.reruns) = (0, 0);
                }
            }
            SessionEvent::Checking { job, .. } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Checking;
                }
            }
            SessionEvent::Checked { job, check } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    // A decision's cost is counted from its `decided` events.
                    if check.decision.is_none() {
                        j.cost_usd += check.cost_usd;
                    }
                    if j.let_go() {
                        return;
                    }
                    if let Some(cap) = check.next_cap_usd {
                        j.budget_usd = cap;
                        j.message = Some(check.message.clone());
                        (j.follow_ups, j.reruns) = (0, 0);
                    }
                    j.checkpoints.push(check.clone());
                }
            }
            SessionEvent::Ended { job, outcome, cost_usd, turns, reason } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.cost_usd += cost_usd;
                    j.turns += turns;
                    if j.let_go() {
                        return;
                    }
                    j.reason.clone_from(reason);
                    j.ended_at = Some(t);
                    j.status = match outcome {
                        Outcome::Done => Status::Done,
                        Outcome::Failed => Status::Failed,
                        Outcome::Stopped => Status::Stopped,
                        Outcome::Retry | Outcome::Rerun => Status::Queued,
                    };
                    if j.status == Status::Queued {
                        j.queued_at = t;
                    }
                    if *outcome == Outcome::Rerun {
                        j.reruns += 1;
                    }
                }
            }
        }
    }

    /// What a session learned of its account.
    fn apply_account_fact(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::Rate { account, meters } => {
                let a = self.account(account);
                a.meters.clone_from(meters);
                a.read_at = Some(t);
            }
            SessionEvent::Limited { account, until } => {
                let a = self.account(account);
                a.limited_until = Some(a.limited_until.map_or(*until, |u| u.max(*until)));
            }
            SessionEvent::SignedOut { account } => {
                let a = self.account(account);
                (a.signed_out, a.signed_at) = (true, Some(t));
            }
            _ => {}
        }
    }

    fn apply_decide(&mut self, t: u64, e: &DecideEvent) {
        match e {
            DecideEvent::DecisionRequested { id, job, point, created, attempt } => {
                // A runner waiting on its decision holds no seat on its account.
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.created_at == *created && j.attempts == *attempt)
                    && j.status == Status::Running
                    && !matches!(point, Point::Held | Point::Disk)
                {
                    j.status = Status::Checking;
                }
                self.decisions.insert(
                    id.clone(),
                    Decision {
                        id: id.clone(),
                        job: job.clone(),
                        point: *point,
                        created: *created,
                        attempt: *attempt,
                        requested_at: t,
                        parked_until: None,
                        answer: None,
                        steps: Vec::new(),
                        action: None,
                    },
                );
            }
            DecideEvent::Parked { id, until } => {
                if let Some(d) = self.decisions.get_mut(id).filter(|d| d.action.is_none()) {
                    d.parked_until = Some(*until);
                }
            }
            DecideEvent::Decided { id, handler, action, why, cost_usd, refused } => {
                let Some(d) = self.decisions.get_mut(id) else { return };
                if let Some(j) = self.jobs.get_mut(&d.job).filter(|j| j.created_at == d.created) {
                    j.cost_usd += cost_usd;
                }
                if handler == "orchestrator" {
                    d.parked_until = None;
                    if d.answer.is_none() {
                        d.answer = Some(action.clone());
                    }
                }
                if d.action.is_none() && refused.is_none() && *action != Action::Defer {
                    d.action = Some(action.clone());
                }
                d.steps.push(Step {
                    handler: handler.clone(),
                    action: action.clone(),
                    why: why.clone(),
                    cost_usd: *cost_usd,
                    refused: refused.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(t: u64, event: Event) -> Stamped {
        Stamped { t, event, from: None }
    }

    fn queued(job: &str) -> Event {
        Event::Queue(QueueEvent::Queued {
            job: job.into(),
            task: "t".into(),
            readonly: false,
            harnesses: Vec::new(),
            models: BTreeMap::new(),
            effort: None,
            budget_usd: 1.0,
            pinned: None,
            reuse_key: None,
            after: Vec::new(),
            profile: None,
            fork: None,
            parent: None,
            depth: 0,
        })
    }

    fn queued_after(job: &str, after: &[&str]) -> Event {
        let Event::Queue(QueueEvent::Queued {
            job,
            task,
            readonly,
            harnesses,
            models,
            effort,
            budget_usd,
            pinned,
            reuse_key,
            profile,
            ..
        }) = queued(job)
        else {
            unreachable!()
        };
        Event::Queue(QueueEvent::Queued {
            job,
            task,
            readonly,
            harnesses,
            models,
            effort,
            budget_usd,
            pinned,
            reuse_key,
            after: after.iter().map(|a| (*a).to_owned()).collect(),
            profile,
            fork: None,
            parent: None,
            depth: 0,
        })
    }

    fn ended(job: &str, outcome: Outcome) -> Event {
        Event::Session(SessionEvent::Ended { job: job.into(), outcome, cost_usd: 0.0, turns: 1, reason: None })
    }

    #[test]
    fn a_collected_clone_leaves_the_branch_and_the_size_is_kept() {
        let dir = std::path::PathBuf::from("/c/a");
        let events = [
            st(1, queued("a")),
            st(
                2,
                Event::Session(SessionEvent::Cloned {
                    job: "a".into(),
                    workdir: dir.clone(),
                    branch: "relay/a".into(),
                }),
            ),
            st(3, Event::Session(SessionEvent::Disk { job: "a".into(), bytes: 7 })),
            st(4, ended("a", Outcome::Done)),
            st(5, Event::Land(LandEvent::Collected { job: "a".into(), what: "outputs".into(), bytes: 9 })),
        ];
        let w = World::fold(&events);
        assert_eq!((w.jobs["a"].disk_bytes, w.jobs["a"].collected.as_deref()), (Some(7), Some("outputs")));
        assert_eq!(w.jobs["a"].workdir.as_ref(), Some(&dir), "emptied, not removed");
        let gone = Event::Land(LandEvent::Collected { job: "a".into(), what: "clone".into(), bytes: 9 });
        let w = World::fold(&[&events[..], &[st(6, gone)]].concat());
        assert_eq!((w.jobs["a"].workdir.as_ref(), w.jobs["a"].branch.as_deref()), (None, Some("relay/a")));
    }

    #[test]
    fn a_resume_may_move_the_job_to_another_model() {
        let fail = ended("a", Outcome::Failed);
        let to = |m: &str| BTreeMap::from([("claude".to_owned(), m.to_owned())]);
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "go on".into(),
            budget_usd: None,
            models: to("claude-opus-5-5"),
        });
        let w = World::fold(&[st(1, queued("a")), st(2, fail), st(3, resumed)]);
        assert_eq!(w.jobs["a"].models, to("claude-opus-5-5"));
        assert_eq!(w.jobs["a"].status, Status::Queued);
    }

    #[test]
    fn a_profiles_starts_are_kept_for_a_day_and_outlive_the_job() {
        let mut q = queued("a");
        if let Event::Queue(QueueEvent::Queued { profile, .. }) = &mut q {
            *profile = Some("hunt".into());
        }
        let dispatched = |t, job: &str| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: job.into(),
                    account: "claude:a".into(),
                    runner_pid: 1,
                    runner_started: 1,
                }),
            )
        };
        let day = DAY_MICROS;
        let events = [
            st(1, q),
            st(2, queued("plain")),
            dispatched(10, "a"),
            dispatched(11, "plain"),
            dispatched(10 + day, "a"),
            st(20 + day, Event::Land(LandEvent::Forgotten { job: "a".into() })),
        ];
        let w = World::fold(&events);
        assert_eq!(
            w.profile_starts["hunt"],
            [10 + day],
            "a start older than a day is dropped; a job without a profile counts for none"
        );
        assert_eq!(w.profile_starts.len(), 1);
        assert!(!w.jobs.contains_key("a"), "the starts remain after the job is cleaned away");
    }

    #[test]
    fn dependencies_fold_the_same_every_time() {
        let release = |from: &[&str]| {
            Event::Queue(QueueEvent::Released { job: "c".into(), from: from.iter().map(|f| (*f).to_owned()).collect() })
        };
        let events = [
            st(1, queued("a")),
            st(2, queued("b")),
            st(3, queued_after("c", &["a", "b", "a"])),
            st(4, release(&["b"])),
            st(5, release(&["zzz"])),
        ];
        let w = World::fold(&events);
        assert_eq!(w, World::fold(&events), "a replay is the same state");
        assert_eq!(w.jobs["c"].after, ["a", "a"], "only what was released is dropped");
        let all = World::fold(&[&events[..3], &[st(4, release(&[]))]].concat());
        assert!(all.jobs["c"].after.is_empty(), "no names releases all");
        let stored: World = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(stored, w, "the checkpoint keeps it");
    }

    #[test]
    fn a_job_waits_for_done_and_is_blocked_by_anything_else() {
        let dispatched = |job: &str| {
            Event::Dispatch(DispatchEvent::Dispatched {
                job: job.into(),
                account: "c:w".into(),
                runner_pid: 7,
                runner_started: 1,
            })
        };
        let mut events = vec![st(1, queued("a")), st(2, queued_after("b", &["a"]))];
        let state = |events: &[Stamped]| {
            let w = World::fold(events);
            (w.startable(&w.jobs["b"]), w.blocked(&w.jobs["b"]), Wait::summary(&w.waits(&w.jobs["b"])))
        };
        assert_eq!(state(&events), (false, false, Some("after a".into())), "queued");
        events.push(st(3, dispatched("a")));
        assert_eq!(state(&events), (false, false, Some("after a".into())), "running");
        events.push(st(4, ended("a", Outcome::Failed)));
        assert_eq!(state(&events), (false, true, Some("waits for a (failed)".into())), "failed");
        events.push(st(
            5,
            Event::Queue(QueueEvent::Resumed {
                job: "a".into(),
                message: "again".into(),
                budget_usd: None,
                models: BTreeMap::new(),
            }),
        ));
        assert_eq!(state(&events), (false, false, Some("after a".into())), "queued again, the dependent waits again");
        events.extend([st(6, dispatched("a")), st(7, ended("a", Outcome::Done))]);
        assert_eq!(state(&events), (true, false, None), "done");
        events.push(st(8, Event::Land(LandEvent::Forgotten { job: "a".into() })));
        assert_eq!(state(&events), (true, false, None), "a done job cleaned away was waited for");
    }

    #[test]
    fn a_dependency_that_ended_otherwise_or_vanished_blocks() {
        let events = [
            st(1, queued("a")),
            st(2, Event::Queue(QueueEvent::Dequeued { job: "a".into() })),
            st(3, queued_after("b", &["a"])),
            st(4, queued_after("c", &["a", "gone"])),
            st(5, Event::Land(LandEvent::Forgotten { job: "a".into() })),
        ];
        let w = World::fold(&events[..4]);
        assert_eq!(Wait::summary(&w.waits(&w.jobs["b"])).as_deref(), Some("waits for a (dequeued)"));
        let w = World::fold(&events);
        assert_eq!(
            Wait::summary(&w.waits(&w.jobs["b"])).as_deref(),
            Some("waits for a (gone)"),
            "not done when cleaned"
        );
    }

    #[test]
    fn lifecycle_and_retry() {
        let w = World::fold(&[
            st(1, queued("a")),
            st(
                2,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "x".into(),
                    runner_pid: 9,
                    runner_started: 1,
                }),
            ),
            st(
                3,
                Event::Session(SessionEvent::Started {
                    job: "a".into(),
                    account: "x".into(),
                    session_id: "s".into(),
                    commit: None,
                }),
            ),
            st(4, Event::Session(SessionEvent::Limited { account: "x".into(), until: 100 })),
            st(
                5,
                Event::Session(SessionEvent::Ended {
                    job: "a".into(),
                    outcome: Outcome::Retry,
                    cost_usd: 0.5,
                    turns: 2,
                    reason: None,
                }),
            ),
        ]);
        let j = &w.jobs["a"];
        assert_eq!(j.status, Status::Queued);
        assert_eq!(j.transcript_account.as_deref(), Some("x"));
        assert_eq!(j.attempts, 1);
        assert!((j.cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(w.accounts["x"].limited_until, Some(100));
    }

    #[test]
    fn a_lapsed_reading_signs_an_account_out_and_a_good_one_signs_it_back_in() {
        let ev = |t, e| st(t, Event::Quota(e));
        let out = QuotaEvent::SignedOut { account: "x".into(), sign_in: Some("s1".into()) };
        let good = QuotaEvent::Reading { account: "x".into(), meters: Meters::new(), sign_in: Some("s2".into()) };
        let w = World::fold(&[ev(3, out.clone())]);
        let a = &w.accounts["x"];
        assert_eq!((a.signed_out, a.signed_at, a.unreadable_at), (true, Some(3), Some(3)));
        let w = World::fold(&[ev(3, out), ev(5, good)]);
        let a = &w.accounts["x"];
        assert_eq!((a.signed_out, a.signed_at, a.sign_in.as_deref()), (false, Some(5), Some("s2")));
    }

    #[test]
    fn reaped_after_stop_is_stopped() {
        let mut evs = vec![
            st(1, queued("a")),
            st(
                2,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "x".into(),
                    runner_pid: 9,
                    runner_started: 1,
                }),
            ),
        ];
        let failed = World::fold(
            &[evs.clone(), vec![st(3, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() }))]].concat(),
        );
        assert_eq!(failed.jobs["a"].status, Status::Failed);
        evs.push(st(3, Event::Queue(QueueEvent::StopRequested { job: "a".into() })));
        evs.push(st(4, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() })));
        assert_eq!(World::fold(&evs).jobs["a"].status, Status::Stopped);
    }

    #[test]
    fn only_the_current_attempt_is_heard() {
        let dispatched = |t| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "x".into(),
                    runner_pid: 9,
                    runner_started: t,
                }),
            )
        };
        let ended = |t, attempt| Stamped {
            t,
            event: Event::Session(SessionEvent::Ended {
                job: "a".into(),
                outcome: Outcome::Done,
                cost_usd: 0.0,
                turns: 1,
                reason: None,
            }),
            from: Some(crate::bus::Origin { job: "a".into(), created: Some(1), attempt }),
        };
        // Attempt 1's runner outlived its attempt and reports after attempt 2 started: not heard.
        let w = World::fold(&[st(1, queued("a")), dispatched(2), dispatched(3), ended(4, 1)]);
        assert_eq!(w.jobs["a"].status, Status::Running);
        let w = World::fold(&[st(1, queued("a")), dispatched(2), dispatched(3), ended(4, 2)]);
        assert_eq!(w.jobs["a"].status, Status::Done);
        // A file speaks only for its own job.
        let mut other = ended(4, 2);
        other.from = Some(crate::bus::Origin { job: "b".into(), created: Some(1), attempt: 2 });
        assert_eq!(
            World::fold(&[st(1, queued("a")), dispatched(2), dispatched(3), other]).jobs["a"].status,
            Status::Running
        );
    }

    #[test]
    fn a_reap_after_the_runner_reported_changes_nothing() {
        let done = Event::Session(SessionEvent::Ended {
            job: "a".into(),
            outcome: Outcome::Done,
            cost_usd: 0.0,
            turns: 1,
            reason: None,
        });
        let dispatched = Event::Dispatch(DispatchEvent::Dispatched {
            job: "a".into(),
            account: "x".into(),
            runner_pid: 9,
            runner_started: 1,
        });
        let reaped = Event::Dispatch(DispatchEvent::Reaped { job: "a".into() });
        let w = World::fold(&[st(1, queued("a")), st(2, dispatched), st(3, done), st(4, reaped)]);
        assert_eq!(w.jobs["a"].status, Status::Done);
    }

    #[test]
    fn a_checkpoint_raises_the_cap_and_a_hand_resume_during_it_stands() {
        let dispatched = || {
            Event::Dispatch(DispatchEvent::Dispatched {
                job: "a".into(),
                account: "x".into(),
                runner_pid: 9,
                runner_started: 1,
            })
        };
        let session = |e| Event::Session(e);
        let checking = session(SessionEvent::Checking { job: "a".into(), checkpoint: 1 });
        let check = bus::Check {
            checkpoint: 1,
            verdict: bus::CheckVerdict::Continue,
            why: "on track".into(),
            message: "Checkpoint 1".into(),
            next_cap_usd: Some(2.0),
            head: None,
            tree: None,
            checker: Some("y".into()),
            cost_usd: 0.5,
            decision: None,
        };
        let checked = session(SessionEvent::Checked { job: "a".into(), check });
        let retry = session(SessionEvent::Ended {
            job: "a".into(),
            outcome: Outcome::Retry,
            cost_usd: 1.0,
            turns: 1,
            reason: None,
        });
        let at_check = [st(1, queued("a")), st(2, dispatched()), st(3, checking.clone())];
        assert_eq!(World::fold(&at_check).jobs["a"].status, Status::Checking);
        let w = World::fold(&[at_check.to_vec(), vec![st(4, checked), st(5, retry.clone())]].concat());
        let j = &w.jobs["a"];
        assert_eq!((j.status, j.budget_usd, j.cost_usd), (Status::Queued, 2.0, 1.5));
        assert_eq!((j.message.as_deref(), j.checkpoints.len()), (Some("Checkpoint 1"), 1));

        let by_hand = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "mine".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        });
        let failed = session(SessionEvent::Ended {
            job: "a".into(),
            outcome: Outcome::Failed,
            cost_usd: 0.0,
            turns: 0,
            reason: Some("waits".into()),
        });
        let w = World::fold(&[at_check.to_vec(), vec![st(4, by_hand), st(5, failed)]].concat());
        assert_eq!((w.jobs["a"].status, w.jobs["a"].message.as_deref()), (Status::Queued, Some("mine")));
    }

    #[test]
    fn limits_only_move_later_and_enable_clears_sign_out() {
        let w = World::fold(&[
            st(1, Event::Session(SessionEvent::Limited { account: "x".into(), until: 200 })),
            st(2, Event::Session(SessionEvent::Limited { account: "x".into(), until: 100 })),
            st(3, Event::Session(SessionEvent::SignedOut { account: "x".into() })),
            st(4, Event::Quota(QuotaEvent::AccountEnabled { account: "x".into(), enabled: true })),
        ]);
        assert_eq!(w.accounts["x"].limited_until, Some(200));
        assert!(!w.accounts["x"].signed_out);
    }

    #[test]
    fn forgotten_jobs_disappear() {
        let w = World::fold(&[st(1, queued("a")), st(2, Event::Land(LandEvent::Forgotten { job: "a".into() }))]);
        assert!(w.jobs.is_empty());
    }

    mod merge {
        use super::super::*;
        use crate::harness::Meter;

        fn read(at: u64, utilization: f64) -> AccountState {
            let mut a = AccountState { read_at: Some(at), ..AccountState::default() };
            a.meters.insert("five_hour".into(), Meter { utilization, resets_at: None });
            a
        }

        #[test]
        fn the_later_reading_wins_whole() {
            let (old, new) = (read(10, 0.9), read(20, 0.1));
            for (ours, theirs) in [(&old, &new), (&new, &old)] {
                let m = merge_account(ours, theirs);
                assert_eq!((m.read_at, &m.meters), (Some(20), &new.meters));
            }
            let unread = AccountState::default();
            assert_eq!(merge_account(&unread, &old).read_at, Some(10), "a reading beats none");
            assert_eq!(merge_account(&old, &unread).meters, old.meters);
        }

        #[test]
        fn the_later_failed_read_wins() {
            let at = |t| AccountState { unreadable_at: t, ..AccountState::default() };
            assert_eq!(merge_account(&at(Some(5)), &at(Some(9))).unreadable_at, Some(9));
            assert_eq!(merge_account(&at(Some(9)), &at(Some(5))).unreadable_at, Some(9));
            assert_eq!(merge_account(&at(None), &at(Some(5))).unreadable_at, Some(5));
        }

        #[test]
        fn the_later_limit_wins() {
            let until = |u| AccountState { limited_until: u, ..AccountState::default() };
            assert_eq!(merge_account(&until(Some(100)), &until(Some(200))).limited_until, Some(200));
            assert_eq!(merge_account(&until(Some(200)), &until(Some(100))).limited_until, Some(200));
            assert_eq!(merge_account(&until(Some(100)), &until(None)).limited_until, Some(100));
        }

        #[test]
        fn a_sign_out_anywhere_counts() {
            let out = AccountState { signed_out: true, signed_at: Some(5), ..AccountState::default() };
            let fine = AccountState::default();
            assert!(merge_account(&fine, &out).signed_out);
            assert!(merge_account(&out, &fine).signed_out);
            assert!(!merge_account(&fine, &fine).signed_out);
            // Enabled again later, in this project or another: the sign-out is over.
            let back = AccountState { signed_at: Some(9), ..AccountState::default() };
            assert!(!merge_account(&back, &out).signed_out && !merge_account(&out, &back).signed_out);
        }

        #[test]
        fn enabled_stays_ours() {
            let off = AccountState { enabled: false, ..AccountState::default() };
            let on = AccountState::default();
            assert!(!merge_account(&off, &on).enabled);
            assert!(merge_account(&on, &off).enabled);
        }

        #[test]
        fn hearing_adds_accounts_and_leaves_jobs() {
            let mut ours = World::default();
            ours.accounts.insert("c:a".into(), read(10, 0.5));
            let mut theirs = World::default();
            theirs.accounts.insert("c:a".into(), read(20, 0.2));
            theirs.accounts.insert(
                "c:b".into(),
                AccountState { enabled: false, signed_out: true, signed_at: Some(5), ..read(5, 0.0) },
            );
            theirs.jobs.insert("j".into(), Job::default());
            let mut other = World::default();
            other.accounts.insert("c:a".into(), AccountState { limited_until: Some(99), ..read(15, 0.9) });
            ours.hear_accounts(&[theirs, other]);
            assert!(ours.jobs.is_empty());
            let a = &ours.accounts["c:a"];
            assert_eq!((a.read_at, a.limited_until), (Some(20), Some(99)));
            let b = &ours.accounts["c:b"];
            assert!(b.enabled && b.signed_out, "an account only they know is enabled here");
            assert_eq!(b.read_at, Some(5));
        }
    }

    mod checkpoint {
        use super::super::*;
        use crate::config::tempdir::Dir;

        struct Fixture {
            _dir: Dir,
            bus: PathBuf,
            file: PathBuf,
        }

        impl Fixture {
            fn new() -> Self {
                let dir = Dir::new();
                let bus = dir.path().join("bus");
                std::fs::create_dir_all(bus.join("session")).unwrap();
                let file = dir.path().join("world.json");
                Self { _dir: dir, bus, file }
            }

            fn append(&self, rel: &str, text: &str) {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.bus.join(rel)).unwrap();
                f.write_all(text.as_bytes()).unwrap();
            }

            fn load(&self) -> World {
                World::load(&self.bus, &self.file).unwrap()
            }

            fn full(&self) -> World {
                World::fold(&bus::read_all(&self.bus).unwrap())
            }

            fn checkpoint(&self) -> serde_json::Value {
                serde_json::from_slice(&std::fs::read(&self.file).unwrap()).unwrap()
            }

            /// Plants an account no event names, so a load that keeps it read incrementally.
            fn plant(&self) {
                let mut c = self.checkpoint();
                c["world"]["accounts"]["planted"] = serde_json::to_value(AccountState::default()).unwrap();
                std::fs::write(&self.file, serde_json::to_vec(&c).unwrap()).unwrap();
            }
        }

        fn queued(t: u64, job: &str) -> String {
            format!(
                r#"{{"t":{t},"type":"queued","job":"{job}","task":"x","readonly":false,"effort":null,"budget_usd":1.0,"pinned":null}}
"#
            )
        }

        fn dispatched(t: u64, job: &str) -> String {
            format!(
                r#"{{"t":{t},"type":"dispatched","job":"{job}","account":"c:w","runner_pid":7,"runner_started":1}}
"#
            )
        }

        fn ended(t: u64, job: &str) -> String {
            format!(
                r#"{{"t":{t},"type":"ended","job":"{job}","outcome":"done","cost_usd":0.25,"turns":3,"reason":null}}
"#
            )
        }

        fn limited(t: u64, until: i64) -> String {
            format!(
                r#"{{"t":{t},"type":"limited","account":"c:w","until":{until}}}
"#
            )
        }

        #[test]
        fn incremental_equals_full_fold() {
            let b = Fixture::new();
            b.append("queue.ndjson", &(queued(1, "a") + &queued(2, "b")));
            assert_eq!(b.load(), b.full());
            b.plant();
            b.append("dispatch.ndjson", &(dispatched(3, "a") + &dispatched(4, "b")));
            b.append("session/a.1.ndjson", &(limited(5, 99) + &ended(6, "a")));
            b.append("session/b.1.ndjson", &limited(7, 50));
            b.append("quota.ndjson", "{\"t\":8,\"type\":\"unreadable\",\"account\":\"c:w\"}\n");
            let w = b.load();
            assert!(w.accounts.contains_key("planted"), "read only what is new");
            let mut full = b.full();
            full.accounts.insert("planted".into(), AccountState::default());
            assert_eq!(w, full);
            assert_eq!(w.jobs["a"].status, Status::Done);
            assert_eq!(w.jobs["b"].status, Status::Running);
            assert_eq!(b.checkpoint()["offsets"]["session/b.1.ndjson"]["len"], limited(7, 50).len());
            // Nothing new: the checkpoint is not rewritten.
            let before = std::fs::metadata(&b.file).unwrap().modified().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert_eq!(b.load(), w);
            assert_eq!(std::fs::metadata(&b.file).unwrap().modified().unwrap(), before);
            // A forgotten job's session files go; what they said stays folded.
            std::fs::remove_file(b.bus.join("session/b.1.ndjson")).unwrap();
            b.append("land.ndjson", "{\"t\":9,\"type\":\"forgotten\",\"job\":\"b\"}\n");
            let w = b.load();
            assert!(w.accounts.contains_key("planted") && !w.jobs.contains_key("b"));
            assert!(b.checkpoint()["offsets"].get("session/b.1.ndjson").is_none());
        }

        #[test]
        fn a_peek_never_writes() {
            let b = Fixture::new();
            b.append("queue.ndjson", &queued(1, "a"));
            assert_eq!(World::peek(&b.bus, &b.file).unwrap(), b.full());
            assert!(!b.file.exists(), "no checkpoint is made");
            b.load();
            b.plant();
            let planted = std::fs::read(&b.file).unwrap();
            b.append("dispatch.ndjson", &dispatched(2, "a"));
            let w = World::peek(&b.bus, &b.file).unwrap();
            assert!(w.accounts.contains_key("planted"), "the checkpoint is used");
            assert_eq!(w.jobs["a"].status, Status::Running);
            assert_eq!(std::fs::read(&b.file).unwrap(), planted, "nor brought up to date");
        }

        #[test]
        fn a_torn_line_is_read_once_complete() {
            let b = Fixture::new();
            b.append("queue.ndjson", &queued(1, "a"));
            b.load();
            b.plant();
            let line = dispatched(2, "a");
            let (head, tail) = line.split_at(20);
            b.append("dispatch.ndjson", head);
            assert_eq!(b.load().jobs["a"].status, Status::Queued);
            assert_eq!(b.checkpoint()["offsets"]["dispatch.ndjson"]["len"], 0);
            b.append("dispatch.ndjson", tail);
            let w = b.load();
            assert!(w.accounts.contains_key("planted"));
            assert_eq!(w.jobs["a"].status, Status::Running);
        }

        #[test]
        fn a_mismatched_version_folds_in_full() {
            let b = Fixture::new();
            b.append("queue.ndjson", &queued(1, "a"));
            b.load();
            b.plant();
            let mut c = b.checkpoint();
            c["version"] = "0.0.0+fold0".into();
            std::fs::write(&b.file, serde_json::to_vec(&c).unwrap()).unwrap();
            assert_eq!(b.load(), b.full());
            assert_eq!(b.checkpoint()["version"], fold_version());
            // Unreadable is the same as missing.
            std::fs::write(&b.file, "{").unwrap();
            assert_eq!(b.load(), b.full());
        }

        #[test]
        fn a_shrunk_file_folds_in_full() {
            let b = Fixture::new();
            b.append("queue.ndjson", &(queued(1, "a") + &queued(2, "b")));
            b.load();
            b.plant();
            std::fs::write(b.bus.join("queue.ndjson"), queued(3, "c")).unwrap();
            let w = b.load();
            assert_eq!(w, b.full());
            assert_eq!(w.jobs.keys().collect::<Vec<_>>(), ["c"]);
        }

        #[test]
        fn a_file_replaced_under_its_name_folds_in_full() {
            // A file replaced under its name, longer than the one read before.
            let b = Fixture::new();
            b.append("queue.ndjson", &queued(1, "a"));
            b.append("dispatch.ndjson", &dispatched(2, "a"));
            b.append("session/a.1.ndjson", &limited(3, 50));
            b.load();
            b.plant();
            let replacement = b.bus.join("session/a.1.ndjson.new");
            std::fs::write(&replacement, limited(4, 60) + &limited(5, 70)).unwrap();
            std::fs::rename(&replacement, b.bus.join("session/a.1.ndjson")).unwrap();
            let w = b.load();
            assert_eq!(w, b.full());
            assert!(!w.accounts.contains_key("planted"), "folded in full, not from the stale mark");
        }

        #[test]
        fn a_late_event_folds_in_full() {
            let b = Fixture::new();
            b.append("queue.ndjson", &queued(1, "a"));
            b.append("dispatch.ndjson", &dispatched(2, "a"));
            b.append("session/a.1.ndjson", &limited(5, 99));
            b.load();
            b.plant();
            // Stamped before the last event folded, appended after it.
            b.append("session/a.1.ndjson", &ended(4, "a"));
            assert_eq!(b.load(), b.full());
        }
    }
}
