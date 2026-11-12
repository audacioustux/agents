//! The state every system works from, derived by folding the bus. The only
//! thing stored is a checkpoint of that fold (`world.json`): a cache, so a
//! command reads only what was written since, never the source of truth.

use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{
    self, Action, DecideEvent, DevShellEvent, DispatchEvent, Event, Fork, LandEvent, Meters, Offsets, Outcome, Point,
    QueueEvent, QuotaEvent, ReadFailure, SessionEvent, Stamped,
};
use crate::harness::report::reset_ahead_limit;

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
    pub kind: bus::Kind,
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
    /// The account its dispatch gave it. A job back in the queue has none: the next dispatch
    /// chooses again (the account that last wrote its session is `transcript_account`).
    pub account: Option<String>,
    /// The model its latest dispatch resolved on that account (`DispatchEvent::Dispatched`): what
    /// its session runs, the seat it holds and what the boards show, whichever project reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The provider whose seat that dispatch took (omp's model prefix); empty for one seat budget.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
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
    /// The build job's clone: the session's to write, so git runs in it only as [`crate::ctx::SessionRepo`] allows.
    pub workdir: Option<crate::ctx::SessionRepo>,
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
    /// What the latest run spent past its cap after it had finished, as `(spent, cap)` in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overran: Option<(f64, f64)>,
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
    /// How its result is accepted.
    #[serde(default)]
    pub acceptance: bus::Acceptance,
    /// The commit its branch started from.
    #[serde(default)]
    pub base: Option<String>,
    /// Every acceptance check run, oldest first; only the latest keeps its output tail.
    #[serde(default)]
    pub bars: Vec<Bar>,
    /// The model family a review ran on.
    #[serde(default)]
    pub family: Option<String>,
    /// The reviews of this job's branch, oldest first.
    #[serde(default)]
    pub verdicts: Vec<Reviewed>,
    /// Runs of this job that ended failed, since it was last requeued under a profile.
    #[serde(default)]
    pub failed_ends: u32,
    /// For a review: it was taken out of its build job's chain, and by whom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unchained: Option<Unchained>,
    /// For a review: the brief it is told, in place of its own, when its session next starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebrief: Option<String>,
}

/// A review that no longer belongs to its build job's chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unchained {
    /// The review that replaced it; none when the chain was detached.
    pub by: Option<String>,
}

impl Unchained {
    /// For the board: `superseded by x` or `detached`.
    pub fn summary(&self) -> String {
        self.by.as_ref().map_or_else(|| "detached".to_owned(), |b| format!("superseded by {b}"))
    }
}

/// One acceptance check run, as the job's record keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub commit: Option<String>,
    pub result: bus::BarResult,
    pub exit: i32,
    pub base_exit: Option<i32>,
    /// For `fail`: the failing tests its output names. For `base-red`: the failing tests, all failing
    /// at the base commit too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing: Vec<String>,
    /// A `fail` at the same commit as the check before it, failing the same way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_progress: bool,
    /// The output tail; kept for the latest run only.
    pub tail: String,
}

impl Bar {
    /// Its result, with the failing tests it names (`base-red: a::one, a::two`) and whether it repeats
    /// the check before it (`fail: a::one (no progress: same commit, same check failure)`).
    pub fn summary(&self) -> String {
        let mut s = self.result.as_str().to_owned();
        if !self.failing.is_empty() {
            s = format!("{s}: {}", self.failing.join(", "));
        }
        if self.no_progress {
            s = format!("{s} ({})", bus::NO_PROGRESS);
        }
        s
    }
}

/// One review of a job's branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reviewed {
    pub reviewer: String,
    pub commit: String,
    pub verdict: bus::Verdict,
    pub reason: Option<bus::Reason>,
    pub family: Option<String>,
    /// The builder's head when the verdict came in, when the branch had moved past `commit`.
    #[serde(default)]
    pub stale_head: Option<String>,
}

impl Reviewed {
    /// A reject, or a verdict on a commit the branch has moved past: the reviewer is to judge the branch again
    /// once its builder is done.
    pub fn wants_again(&self) -> bool {
        self.stale_head.is_some() || self.verdict == bus::Verdict::Reject
    }

    /// `accept`, `reject: test-failure`, or `stale-head`.
    pub fn summary(&self) -> String {
        if self.stale_head.is_some() {
            return "stale-head".to_owned();
        }
        match self.reason {
            Some(r) => format!("{}: {}", self.verdict.as_str(), r.as_str()),
            None => self.verdict.as_str().to_owned(),
        }
    }
}

impl Job {
    pub fn readonly(&self) -> bool {
        self.kind.readonly()
    }

    /// The acceptance check, if the job declares one.
    pub fn check(&self) -> Option<&str> {
        self.acceptance.command()
    }

    /// The build job this job reviews.
    pub fn review_of(&self) -> Option<&str> {
        self.acceptance.review_of()
    }

    /// Whether the process that last ran it is still alive, whatever the fold says: a runner
    /// goes on after its job's `ended` (a re-review, waking the dispatcher), and that work is
    /// the job's until it exits.
    pub fn runner_alive(&self) -> bool {
        self.runner.is_some_and(|(pid, started)| crate::os::same_process(pid, started))
    }

    /// The seat a running job holds, as its dispatch recorded it: its account, and what it draws
    /// on there. A run recorded before the model was falls back to the model the job itself names,
    /// never to a config's default; one with no provider either way draws on an unknown one
    /// (`Draws::Unknown`), which counts against every provider of the account until it ends.
    pub fn seat(&self) -> Option<(&str, crate::accounts::Draws)> {
        if self.status != Status::Running {
            return None;
        }
        let account = self.account.as_deref()?;
        let harness = account.split_once(':').map_or(account, |(h, _)| h);
        let model = self.model.as_deref().or_else(|| self.models.get(harness).map(String::as_str));
        Some((account, crate::accounts::draws(model, &self.provider, harness)))
    }

    /// Back in the queue at `t`: no account until a dispatch gives it one.
    fn requeue(&mut self, t: u64) {
        (self.status, self.queued_at, self.account) = (Status::Queued, t, None);
    }

    /// Resumed by hand during its check: what its runner still says changes
    /// only what was spent, and the owner's instruction stands.
    fn let_go(&self) -> bool {
        self.status == Status::Queued && self.runner.is_none() && self.attempts > 0
    }
}

/// An account's reads that gave nothing, since its last good one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failing {
    /// Why the latest one did.
    pub why: ReadFailure,
    /// How many in a row, the latest included.
    pub tries: u32,
    /// Unix microseconds of the first.
    pub since: u64,
    /// Unix microseconds of the latest: the streak ends at a good reading after it.
    #[serde(default)]
    pub last: u64,
}

impl AccountState {
    /// The failed reads since the last good one, however it came (a quota command, a session's rate
    /// events, a twin home's reading, another project's): the streak as it stands after `read_at`.
    /// What every reader of `failing` asks, so a good reading needs no clearing on the path it came by.
    pub fn failure(&self) -> Option<Failing> {
        self.failing.as_ref().and_then(|f| f.after(self.read_at))
    }
}

impl Failing {
    /// The streak as it stands after a good reading at `read_at`: none when the last failure came
    /// before it; when the reading came in the middle, the failures before it no longer count, and
    /// only the latest is known to follow it, so it is one try, started then.
    pub fn after(&self, read_at: Option<u64>) -> Option<Self> {
        match read_at {
            Some(r) if self.last <= r => None,
            Some(r) if self.since <= r => Some(Self { tries: 1, since: self.last, ..self.clone() }),
            _ => Some(self.clone()),
        }
    }

    /// `timed out after 90 s (3 tries since Nov 12 18:08 +06)`.
    pub fn describe(&self) -> String {
        let tries = if self.tries == 1 { "1 try".to_owned() } else { format!("{} tries", self.tries) };
        format!("{} ({tries} since {})", self.why.describe(), crate::accounts::fmt_time(crate::bus::secs(self.since)))
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
    #[serde(deserialize_with = "crate::harness::deserialize_meters")]
    pub meters: Meters,
    /// Unix microseconds of the last reading.
    pub read_at: Option<u64>,
    /// Unix microseconds of the last attempt that read nothing.
    pub unreadable_at: Option<u64>,
    /// Why the reads since the last good one gave nothing, and how many: gone when a reading works.
    #[serde(default)]
    pub failing: Option<Failing>,
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
            failing: None,
            limited_until: None,
            sign_in: None,
            unsigned: false,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    /// Read through `jobs()`; changed only through `jobs_mut()` and the fold, which drop `deps`.
    jobs: BTreeMap<String, Job>,
    pub accounts: BTreeMap<String, AccountState>,
    /// When (bus time, microseconds) each profile's sessions started, within a day
    /// of the latest start. Kept apart from the jobs, which can be cleaned away.
    #[serde(default)]
    pub profile_starts: BTreeMap<String, Vec<u64>>,
    /// Decisions by id, while their job is kept.
    #[serde(default)]
    pub decisions: BTreeMap<String, Decision>,
    /// The project's development environment (`session.env_shell`): its latest warm-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devshell: Option<DevShell>,
    /// What every job waits for, worked out once for this state of `jobs` and shared by every query.
    #[serde(skip)]
    deps: Deps,
}

/// What the bus says of the project's development environment, for the key it was last warmed for.
/// Whether it is ready is what is recorded on disk (`systems::devshell::Gate`); this says how the
/// warm-ups went, for the held reason and the retry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevShell {
    pub key: String,
    /// When the warm-up under way began: its first attempt since the last one that ended for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warming_since: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<u64>,
    /// The attempts for this key that did not start it, since it last did.
    #[serde(default)]
    pub failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<WarmFailure>,
}

/// The latest attempt that did not start the development environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmFailure {
    pub at: u64,
    pub why: String,
    /// The end of its stderr.
    pub tail: String,
    pub timed_out: bool,
}

/// The dependency analysis of one state of a world's jobs, built on the first query after a change
/// (`Deps::build`): what each job waits for, and whether it is blocked. Every query
/// (`waits`, `blocked`, `startable`, `moves_on_its_own`) reads it, so a whole pass costs one linear
/// build. Nothing can leave it stale: `World::jobs` is private, and `jobs_mut` and the fold drop it.
#[derive(Default)]
struct Deps(std::sync::OnceLock<Analysis>);

/// The built analysis: each job's waits.
#[derive(Debug)]
struct Analysis {
    waits: std::collections::HashMap<String, Vec<Wait>>,
}

#[cfg(test)]
thread_local! {
    /// Every step any build on this thread took, for the bound on a whole pass's work a test asserts.
    static BUILD_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Clone for Deps {
    /// A clone builds its own on its first query: the analysis is a function of the jobs.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Deps {
    /// A cache: two worlds with the same jobs are equal whether or not either has built it.
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Deps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.get().is_some() { "Deps(built)" } else { "Deps" })
    }
}

/// One decision point reached, and what its handlers answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub job: String,
    pub point: Point,
    pub created: u64,
    pub attempt: u32,
    /// The process that waits for the answer (pid, start time); `None` in events from before it was recorded.
    #[serde(default)]
    pub waiter: Option<(u32, u64)>,
    pub requested_at: u64,
    /// Set while it waits for `relay decide answer`: until when (bus time).
    pub parked_until: Option<u64>,
    /// The orchestrator's answer, the first one recorded in time; the runner
    /// carries it out only if it passes policy.
    pub answer: Option<Action>,
    /// The orchestrator's line for the board.
    #[serde(default)]
    pub answer_why: String,
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
    /// Parked at `now` (bus time) with nothing answered or carried out yet. Not enough to answer it:
    /// the job may have moved on since (`World::awaiting`).
    fn open(&self, now: u64) -> bool {
        self.parked_until.is_some_and(|u| u > now) && self.answer.is_none() && self.action.is_none()
    }
}

/// The window a profile's `max_per_day` counts over.
pub const DAY_MICROS: u64 = 24 * 3600 * 1_000_000;

/// Bump whenever the fold's meaning changes (`apply`, `current`, what `World`
/// holds, how a mark is kept), so checkpoints folded the old way are discarded.
/// `tests::the_fold_version_moves_with_the_fold` pins it to what the fold makes of a
/// fixed bus, so a fold that changes without a bump fails the tests.
/// 13: `dispatched` records the model and provider; a job back in the queue has no account.
/// 14: the development environment's warm-ups (`devshell`).
/// 18: a meter says its window, provider and scope (17 is the decision seats, #63).
const FOLD_VERSION: u32 = 18;

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
        // The later failed attempt's account of itself, as the merged reading leaves it.
        failing: match (&ours.failing, &theirs.failing) {
            (Some(o), Some(t)) if t.last > o.last => Some(t),
            (o, t) => o.as_ref().or(t.as_ref()),
        }
        .and_then(|f| f.after(read_at)),
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

/// The one walk that builds `Deps`, in two passes and no recursion, so no depth of chain can exhaust
/// the stack. First every queued job's blocking (`stuck`), by a depth-first walk on an explicit stack,
/// each job once however many paths reach it: linear in the jobs and their `after` lists. The jobs on
/// the stack are `path`, kept for constant-time membership: a job met on it is in a cycle of queued
/// jobs, which never starts, so that answer is a fact and is kept like any other. Then each job's
/// waits, read off those answers.
struct Walk<'w> {
    jobs: &'w BTreeMap<String, Job>,
    stuck: std::collections::HashMap<&'w str, bool>,
    path: std::collections::HashSet<&'w str>,
    steps: usize,
}

/// What one dependency, `a`, of a job means: blocked for good, waiting on a queued job (whose blocking
/// is still to be known), or neither.
enum Edge<'w> {
    Done,
    Blocked(&'static str),
    Live(&'static str),
    Queued(&'w Job),
}

impl Deps {
    fn build(jobs: &BTreeMap<String, Job>) -> Analysis {
        let mut walk =
            Walk { jobs, stuck: std::collections::HashMap::new(), path: std::collections::HashSet::new(), steps: 0 };
        for j in jobs.values().filter(|j| j.status == Status::Queued) {
            walk.settle(j);
        }
        let waits = jobs.values().map(|j| (j.name.clone(), walk.waits(j))).collect();
        #[cfg(test)]
        BUILD_STEPS.with(|c| c.set(c.get() + walk.steps));
        Analysis { waits }
    }
}

impl<'w> Walk<'w> {
    fn edge(&self, a: &str) -> Edge<'w> {
        match self.jobs.get(a) {
            Some(d) if d.status == Status::Done => Edge::Done,
            Some(d) if d.status == Status::Queued => Edge::Queued(d),
            Some(d) if d.status.is_live() => Edge::Live(d.status.as_str()),
            Some(d) if d.reason.as_deref() == Some("dequeued") => Edge::Blocked("dequeued"),
            Some(d) => Edge::Blocked(d.status.as_str()),
            None => Edge::Blocked("gone"),
        }
    }

    /// Works out whether `root` and every queued job under it is stuck, on an explicit stack of
    /// (job, its next dependency, blocked so far).
    fn settle(&mut self, root: &'w Job) {
        if self.stuck.contains_key(root.name.as_str()) {
            return;
        }
        let mut stack: Vec<(&'w Job, usize, bool)> = vec![(root, 0, false)];
        self.path.insert(&root.name);
        self.steps += 1;
        while let Some(top) = stack.len().checked_sub(1) {
            let (job, next) = (stack[top].0, stack[top].1);
            if let Some(a) = job.after.get(next) {
                stack[top].1 += 1;
                self.steps += 1;
                let blocked = match self.edge(a) {
                    Edge::Done | Edge::Live(_) => false,
                    Edge::Blocked(_) => true,
                    Edge::Queued(d) if self.path.contains(d.name.as_str()) => true,
                    Edge::Queued(d) => {
                        if let Some(&known) = self.stuck.get(d.name.as_str()) {
                            known
                        } else {
                            self.path.insert(&d.name);
                            self.steps += 1;
                            stack.push((d, 0, false));
                            continue;
                        }
                    }
                };
                stack[top].2 |= blocked;
            } else {
                let (job, _, blocked) = stack[top];
                stack.pop();
                self.path.remove(job.name.as_str());
                self.stuck.insert(&job.name, blocked);
                if let Some(parent) = stack.last_mut() {
                    parent.2 |= blocked;
                }
            }
        }
    }

    /// What `job` waits for, read off the settled blocking of every queued job.
    fn waits(&mut self, job: &'w Job) -> Vec<Wait> {
        let wait = |name: &String, state: &str, blocked| Wait { job: name.clone(), state: state.into(), blocked };
        let mut out = Vec::new();
        for a in &job.after {
            self.steps += 1;
            out.extend(match self.edge(a) {
                Edge::Done => None,
                Edge::Queued(d) if d.name == job.name => Some(wait(a, "a cycle", true)),
                Edge::Queued(d) if self.stuck.get(d.name.as_str()).copied().unwrap_or(false) => {
                    Some(wait(a, "blocked", true))
                }
                Edge::Queued(d) => Some(wait(a, d.status.as_str(), false)),
                Edge::Live(state) => Some(wait(a, state, false)),
                Edge::Blocked(state) => Some(wait(a, state, true)),
            });
        }
        out
    }
}

/// `meters` as read at bus time `t`, each with no reset time that lies further ahead than its
/// window: a reading from before the parsers judged the range ([`reset_in_range`]) holds such a
/// time (a reset rolled a year out), which is no reset time. The bus keeps the reading as it was.
fn in_reach(meters: &Meters, t: u64) -> Meters {
    let read_s = i64::try_from(t / 1_000_000).unwrap_or(i64::MAX);
    let mut out = meters.clone();
    for m in out.values_mut() {
        let limit = reset_ahead_limit(read_s, m.window);
        m.resets_at = m.resets_at.filter(|r| limit.is_none_or(|l| *r <= l));
    }
    out
}

impl World {
    /// The jobs `job` still waits for. A dependency that is done, or was done
    /// when it was cleaned away, is not among them; landing it is a later step.
    pub fn waits(&self, job: &Job) -> Vec<Wait> {
        match self.analysis().waits.get(&job.name) {
            Some(w) => w.clone(),
            // Not one of this world's jobs: walked on its own.
            None => Deps::build(&std::iter::once((job.name.clone(), job.clone())).chain(self.jobs.clone()).collect())
                .waits
                .remove(&job.name)
                .unwrap_or_default(),
        }
    }

    /// The jobs, by name. They change only through `jobs_mut` and the fold.
    pub fn jobs(&self) -> &BTreeMap<String, Job> {
        &self.jobs
    }

    /// The jobs to change, which drops the dependency analysis built for them as they were.
    pub fn jobs_mut(&mut self) -> &mut BTreeMap<String, Job> {
        self.deps = Deps::default();
        &mut self.jobs
    }

    /// This state's dependency analysis, built on the first query since the jobs last changed.
    fn analysis(&self) -> &Analysis {
        self.deps.0.get_or_init(|| Deps::build(&self.jobs))
    }

    /// Whether `job` is queued with every dependency done: the dispatcher may give it a seat.
    pub fn startable(&self, job: &Job) -> bool {
        job.status == Status::Queued && self.waits(job).is_empty()
    }

    /// Whether `job` is queued and can never start on its own until released or its dependency is queued again.
    pub fn blocked(&self, job: &Job) -> bool {
        job.status == Status::Queued && self.waits(job).iter().any(|w| w.blocked)
    }

    /// Whether decision `d` is still about its job as the job is now: the same incarnation and
    /// attempt, in the state its point holds the job in (queued for `held`, held by its runner
    /// otherwise). A resume, a reap, an end or a new attempt make it stale.
    pub fn asks_now(&self, d: &Decision) -> bool {
        self.jobs.get(&d.job).is_some_and(|j| {
            (j.created_at, j.attempts) == (d.created, d.attempt)
                && if d.point == Point::Held { j.status == Status::Queued } else { j.status.has_runner() }
        })
    }

    /// Whether decision `id` is still on record and asks about its job as it is ([`World::asks_now`]).
    /// A decision of a job that was forgotten (dequeued and cleaned) is no longer on record: it asks
    /// about nothing, and a handler still at work on it is withdrawn.
    pub fn still_asks(&self, id: &str) -> bool {
        self.decisions.get(id).is_some_and(|d| self.asks_now(d))
    }

    /// Whether decision `d` waits for the orchestrator at `now` (bus time): parked, unanswered, and
    /// still about its job as it is (`asks_now`). The fold hears an answer only then; whether the
    /// process that waits for it is alive is not the fold's to know (`decide::unanswerable`).
    pub fn awaiting(&self, d: &Decision, now: u64) -> bool {
        d.open(now) && self.asks_now(d)
    }

    /// Whether `job` will change state with nobody acting on it: a runner holds it (running, or
    /// checking while a decision is parked), whose end or death the dispatcher must hear, or it is
    /// queued and not blocked. The one test of "still moving": the dispatcher stays awake while any
    /// job moves, and a waiter for a job's end gives up on a live job that does not.
    pub fn moves_on_its_own(&self, job: &Job) -> bool {
        job.status.has_runner() || job.status == Status::Queued && !self.blocked(job)
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
    pub fn hear_accounts<'a>(&mut self, others: impl IntoIterator<Item = &'a World>) {
        for (key, theirs) in others.into_iter().flat_map(|w| &w.accounts) {
            let ours = self.accounts.entry(key.clone()).or_default();
            *ours = merge_account(ours, theirs);
        }
    }

    /// The account and meters `s` read, when it is a reading this fold took in, just after
    /// [`World::apply`] applied it: a quota command's reading, or a live session's rate (one from an
    /// attempt that is not its job's current one is not taken in). The meters are the account's as
    /// the fold now holds them.
    pub(crate) fn meters_read<'s>(&self, s: &'s Stamped) -> Option<(&'s str, &Meters)> {
        let (Event::Quota(QuotaEvent::Reading { account, .. }) | Event::Session(SessionEvent::Rate { account, .. })) =
            &s.event
        else {
            return None;
        };
        let a = self.accounts.get(account).filter(|a| a.read_at == Some(s.t))?;
        Some((account, &a.meters))
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
    pub(crate) fn current(&self, s: &Stamped, e: &SessionEvent) -> bool {
        let Some(o) = &s.from else { return true };
        e.job().is_none_or(|j| j == o.job)
            && self
                .jobs
                .get(&o.job)
                .is_some_and(|j| j.attempts == o.attempt && o.created.is_none_or(|c| c == j.created_at))
    }

    pub(crate) fn apply(&mut self, s: &Stamped) {
        // Every event may change the jobs: the analysis is built again on the next query.
        self.deps = Deps::default();
        match &s.event {
            Event::Queue(e) => self.apply_queue(s.t, e),
            Event::Dispatch(e) => self.apply_dispatch(s.t, e),
            Event::Session(e) if self.current(s, e) => self.apply_session(s.t, e),
            Event::Session(_) => {}
            Event::Quota(e) => self.apply_quota(s.t, e),
            Event::Decide(e) => self.apply_decide(s.t, e),
            Event::DevShell(e) => self.apply_devshell(s.t, e),
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
                    // The scratch directory is a temporary directory, not the clone: it says nothing about it.
                    if what != "scratch" {
                        j.collected = Some(what.clone());
                    }
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
                acceptance,
            } => {
                let j = Job {
                    name: job.clone(),
                    task: task.clone(),
                    kind: acceptance.kind(*readonly),
                    harnesses: harnesses.clone(),
                    models: models.clone(),
                    effort: effort.clone(),
                    budget_usd: *budget_usd,
                    pinned: pinned.clone(),
                    reuse_key: reuse_key.clone(),
                    // The gate is for relays that cannot judge the job; this one can.
                    after: after.iter().filter(|a| *a != bus::ACCEPTANCE_GATE).cloned().collect(),
                    profile: profile.clone(),
                    fork: fork.as_deref().cloned(),
                    parent: parent.clone(),
                    depth: *depth,
                    acceptance: acceptance.clone(),
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
                    j.requeue(t);
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
                // A job that started meanwhile was not held; a later queueing needs a decision of its own.
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.status == Status::Queued) {
                    j.forced = true;
                }
            }
            QueueEvent::Dequeued { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    (j.status, j.reason, j.ended_at) = (Status::Stopped, Some("dequeued".into()), Some(t));
                }
            }
            QueueEvent::Unchained { job, by } => self.unchain(t, job, by.as_ref()),
            QueueEvent::ReviewBriefed { job, brief } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.rebrief = Some(brief.clone());
                }
            }
            QueueEvent::StopRequested { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.stop_requested = true;
                }
            }
        }
    }

    /// The review `job` leaves its build job's chain.
    fn unchain(&mut self, t: u64, job: &str, by: Option<&String>) {
        let builder = self.jobs.get(job).and_then(|r| r.review_of()).map(str::to_owned);
        if let Some(b) = builder.and_then(|b| self.jobs.get_mut(&b)) {
            // Its verdicts are history now, in the log; the board shows the chain that is.
            b.verdicts.retain(|v| v.reviewer != job);
        }
        if let Some(j) = self.jobs.get_mut(job) {
            let left = Unchained { by: by.cloned() };
            j.rebrief = None;
            // A review that has not started, or waits for a re-review, never runs again.
            if j.status == Status::Queued {
                (j.status, j.reason, j.ended_at) = (Status::Stopped, Some(left.summary()), Some(t));
            }
            j.unchained = Some(left);
        }
    }

    fn apply_dispatch(&mut self, t: u64, e: &DispatchEvent) {
        match e {
            DispatchEvent::Dispatched { job, account, model, provider, runner_pid, runner_started } => {
                if let Some(profile) = self.jobs.get(job).and_then(|j| j.profile.clone()) {
                    let starts = self.profile_starts.entry(profile).or_default();
                    starts.push(t);
                    starts.retain(|&s| s + DAY_MICROS > t);
                }
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Running;
                    j.account = Some(account.clone());
                    (j.model, j.provider) = (model.clone(), provider.clone());
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

    /// A warm-up for another key than the one on record starts that key's record afresh.
    fn apply_devshell(&mut self, t: u64, e: &DevShellEvent) {
        let key = match e {
            DevShellEvent::Warming { key, .. }
            | DevShellEvent::Warmed { key }
            | DevShellEvent::WarmFailed { key, .. } => key,
        };
        let d = self.devshell.get_or_insert_with(DevShell::default);
        if d.key != *key {
            *d = DevShell { key: key.clone(), ..DevShell::default() };
        }
        match e {
            DevShellEvent::Warming { .. } => {
                d.warming_since.get_or_insert(t);
            }
            DevShellEvent::Warmed { .. } => {
                (d.ready_at, d.warming_since, d.failures, d.failed) = (Some(t), None, 0, None);
            }
            DevShellEvent::WarmFailed { why, tail, timed_out, .. } => {
                d.failures += 1;
                d.failed = Some(WarmFailure { at: t, why: why.clone(), tail: tail.clone(), timed_out: *timed_out });
                // A timed-out attempt is one step of a warm-up that goes on; anything else ends it.
                if !timed_out {
                    d.warming_since = None;
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
                (a.unreadable_at, a.failing) = (None, None);
            }
            QuotaEvent::Reading { account, meters, sign_in } => {
                let a = self.account(account);
                a.meters = in_reach(meters, t);
                a.read_at = Some(t);
                a.failing = None;
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
                (a.unreadable_at, a.failing) = (Some(t), None);
                a.sign_in.clone_from(sign_in);
            }
            QuotaEvent::Unreadable { account, why } => {
                let a = self.account(account);
                a.unreadable_at = Some(t);
                // A streak goes on from the failure before it, unless a reading came between.
                let before = a.failure();
                a.failing = why.as_ref().map(|why| Failing {
                    why: why.clone(),
                    tries: before.as_ref().map_or(1, |f| f.tries.saturating_add(1)),
                    since: before.as_ref().map_or(t, |f| f.since),
                    last: t,
                });
            }
        }
    }

    fn apply_session(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::Cloned { job, workdir, branch, base } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    // A clone made again from the job's own work keeps the base it first started from.
                    if base.is_some() {
                        j.base.clone_from(base);
                    }
                    j.workdir = Some(workdir.clone());
                    j.branch = Some(branch.clone());
                    j.collected = None;
                }
            }
            SessionEvent::Started { job, account, session_id, commit, family } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.ran_at.clone_from(commit);
                    j.family.clone_from(family);
                    j.transcript_account = Some(account.clone());
                    j.session_id = Some(session_id.clone());
                    j.message = None;
                    j.rebrief = None;
                    j.forced = false;
                    j.overran = None;
                }
            }
            SessionEvent::Disk { job, bytes } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.disk_bytes = Some(*bytes);
                }
            }
            // Told in the log only: the job's state does not change.
            SessionEvent::CleanupFailed { .. } => {}
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
                    j.failed_ends = 0;
                }
            }
            SessionEvent::Overran { job, spent_usd, cap_usd } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.overran = Some((*spent_usd, *cap_usd));
                }
            }
            SessionEvent::BarChecked { .. } | SessionEvent::Reviewed { .. } => self.apply_judgement(e),
            SessionEvent::Checking { .. } | SessionEvent::Checked { .. } => self.apply_checkpoint(e),
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
                        j.requeue(t);
                    }
                    if *outcome == Outcome::Rerun {
                        j.reruns += 1;
                    }
                    if *outcome == Outcome::Failed {
                        j.failed_ends += 1;
                    }
                }
            }
        }
    }

    /// A run that reached its spend cap, and what was decided there.
    fn apply_checkpoint(&mut self, e: &SessionEvent) {
        match e {
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
            _ => {}
        }
    }

    /// The acceptance checks and reviews a job went through.
    fn apply_judgement(&mut self, e: &SessionEvent) {
        match e {
            SessionEvent::BarChecked { job, commit, exit, tail, base_exit, result, failing, no_progress } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.bars.iter_mut().for_each(|b| b.tail.clear());
                    j.bars.push(Bar {
                        commit: commit.clone(),
                        result: *result,
                        exit: *exit,
                        base_exit: *base_exit,
                        failing: failing.clone(),
                        no_progress: *no_progress,
                        tail: tail.clone(),
                    });
                }
            }
            SessionEvent::Reviewed { job, of, commit, verdict, reason, family, stale_head } => {
                // A review that left the chain still wrote its verdict to the log; the board does not keep it.
                let chained = self.jobs.get(job).is_some_and(|r| r.unchained.is_none());
                if let Some(j) = self.jobs.get_mut(of).filter(|_| chained) {
                    let r = Reviewed {
                        reviewer: job.clone(),
                        commit: commit.clone(),
                        verdict: *verdict,
                        reason: *reason,
                        family: family.clone(),
                        stale_head: stale_head.clone(),
                    };
                    j.verdicts.push(r);
                }
            }
            _ => {}
        }
    }

    /// What a session learned of its account.
    fn apply_account_fact(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::Rate { account, meters } => {
                let a = self.account(account);
                a.meters = in_reach(meters, t);
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
            DecideEvent::DecisionRequested { id, job, point, created, attempt, waiter } => {
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
                        waiter: *waiter,
                        requested_at: t,
                        parked_until: None,
                        answer: None,
                        answer_why: String::new(),
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
            DecideEvent::Answered { id, action, why } => {
                // An answer about an attempt that has moved on (resumed, reaped, ended) is not heard.
                if self.decisions.get(id).is_some_and(|d| self.awaiting(d, t))
                    && let Some(d) = self.decisions.get_mut(id)
                {
                    (d.answer, d.answer_why) = (Some(action.clone()), why.clone());
                }
            }
            DecideEvent::Decided { id, handler, action, why, cost_usd, refused } => {
                let Some(d) = self.decisions.get_mut(id) else { return };
                if let Some(j) = self.jobs.get_mut(&d.job).filter(|j| j.created_at == d.created) {
                    j.cost_usd += cost_usd;
                }
                if handler == "orchestrator" {
                    d.parked_until = None;
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

    #[test]
    fn an_acceptance_sets_the_kind_and_the_gate_is_dropped_from_what_a_job_waits_for() {
        use bus::{ACCEPTANCE_GATE, Acceptance, Kind};
        let with = |acceptance: Acceptance, readonly: bool| {
            let Event::Queue(QueueEvent::Queued {
                job,
                task,
                harnesses,
                models,
                effort,
                budget_usd,
                pinned,
                reuse_key,
                profile,
                ..
            }) = queued("j")
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
                after: vec!["dep".into(), ACCEPTANCE_GATE.into()],
                profile,
                fork: None,
                parent: None,
                depth: 0,
                acceptance,
            })
        };
        let kind = |a: Acceptance, ro| World::fold(&[st(1, with(a, ro))]).jobs["j"].clone();
        assert_eq!(kind(Acceptance::None, false).kind, Kind::Build);
        assert_eq!(kind(Acceptance::Check("c".into()), false).kind, Kind::Build);
        assert_eq!(kind(Acceptance::None, true).kind, Kind::Read);
        let review = kind(Acceptance::ReviewOf("b".into()), true);
        assert_eq!((review.kind, review.review_of()), (Kind::Review, Some("b")));
        assert!(review.readonly());
        assert_eq!(review.after, ["dep"], "only a relay that cannot judge the job waits on the gate");
    }
    use super::*;
    use crate::accounts::Draws;

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
            acceptance: bus::Acceptance::None,
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
            acceptance: bus::Acceptance::None,
        })
    }

    fn ended(job: &str, outcome: Outcome) -> Event {
        Event::Session(SessionEvent::Ended { job: job.into(), outcome, cost_usd: 0.0, turns: 1, reason: None })
    }

    /// FNV-1a: a digest that is the same on every build, unlike `std`'s hasher.
    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
    }

    /// Copilot review of PR #41, finding 3: a fold that changes what it makes of the bus without a
    /// `FOLD_VERSION` bump leaves old checkpoints standing. This pins each version to the digest of
    /// the fold of a fixed bus (a dispatch, a run that ends, retries and is resumed by hand, a
    /// limit, a reading). When the fold changes, the digest does: bump `FOLD_VERSION` and add the
    /// new pair; never change a pair already recorded.
    /// 2026-11-13: the bus holds `claude:tj.pro`'s five-hour reset read as 1826049540 at
    /// 1794513593, a year out. The fold takes no reset time that is further ahead than its window.
    #[test]
    fn a_stored_reset_past_its_window_is_no_reset_time() {
        use crate::harness::{Meter, Window};
        let read = |t_s: u64, r: i64| {
            let meters = BTreeMap::from([
                ("five_hour".to_owned(), Meter::new(Window::FiveHours, 1.0, Some(r))),
                ("other".to_owned(), Meter::new(Window::Other(None), 0.5, Some(r))),
            ]);
            let rate = Event::Session(SessionEvent::Rate { account: "claude:x".into(), meters: meters.clone() });
            let reading = Event::Quota(QuotaEvent::Reading { account: "claude:y".into(), meters, sign_in: None });
            World::fold(&[st(t_s * 1_000_000, rate), st(t_s * 1_000_000 + 1, reading)])
        };
        let w = read(1_794_513_593, 1_826_049_540);
        for account in ["claude:x", "claude:y"] {
            let m = &w.accounts[account].meters;
            assert_eq!(m["five_hour"].resets_at, None, "{account}");
            assert_eq!(m["other"].resets_at, Some(1_826_049_540), "a window of no said length has no limit");
        }
        let w = read(1_794_513_593, 1_794_513_540);
        assert_eq!(
            w.accounts["claude:y"].meters["five_hour"].resets_at,
            Some(1_794_513_540),
            "a reset a minute old stays"
        );
    }

    #[test]
    fn the_fold_version_moves_with_the_fold() {
        const PINNED: &[(u32, u64)] = &[
            (13, 0x1d75_bf1b_971c_dfd9),
            (14, 0x0599_7a27_1a95_7bbd),
            (15, 0x5351_e6de_0d8d_bfd8),
            (16, 0x5351_e6de_0d8d_bfd8),
            (18, 0xcaf0_805c_bb93_9777),
        ];
        let dispatched = |t, model: Option<&str>| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "omp:w".into(),
                    model: model.map(Into::into),
                    provider: model.and_then(|m| m.split_once('/')).map_or("", |(p, _)| p).into(),
                    runner_pid: 7,
                    runner_started: 1,
                }),
            )
        };
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "go on".into(),
            budget_usd: Some(2.0),
            models: BTreeMap::from([("omp".to_owned(), "minimax-code/M3".to_owned())]),
        });
        let reading = Event::Quota(QuotaEvent::Reading {
            account: "omp:w".into(),
            meters: BTreeMap::from([(
                "openai-codex/5 hours".to_owned(),
                crate::harness::Meter { utilization: 0.5, resets_at: Some(99), ..Default::default() },
            )]),
            sign_in: None,
        });
        let events = [
            st(1, queued("a")),
            st(2, queued("b")),
            dispatched(3, Some("openai-codex/gpt-6.1")),
            st(4, Event::Session(SessionEvent::Limited { account: "omp:w".into(), until: 50 })),
            st(5, ended("a", Outcome::Retry)),
            dispatched(6, None),
            st(7, ended("a", Outcome::Failed)),
            st(8, resumed),
            st(9, reading),
            st(10, Event::DevShell(DevShellEvent::Warming { key: "k".into(), shell: "nix develop -c".into() })),
            st(
                11,
                Event::DevShell(DevShellEvent::WarmFailed {
                    key: "k".into(),
                    why: "exited 1".into(),
                    tail: "error".into(),
                    timed_out: false,
                }),
            ),
        ];
        let digest = fnv(serde_json::to_string(&World::fold(&events)).unwrap().as_bytes());
        let (version, pinned) = *PINNED.last().unwrap();
        assert_eq!(version, FOLD_VERSION, "pin the digest of FOLD_VERSION {FOLD_VERSION}: {digest:#x}");
        assert_eq!(
            digest,
            pinned,
            "the fold changed ({digest:#x}): bump FOLD_VERSION and add ({}, {digest:#x}) to PINNED",
            FOLD_VERSION + 1
        );
        let mut versions: Vec<u32> = PINNED.iter().map(|p| p.0).collect();
        versions.dedup();
        assert_eq!(versions.len(), PINNED.len(), "one digest per version");
    }

    #[test]
    fn a_collected_clone_leaves_the_branch_and_the_size_is_kept() {
        let dir = crate::ctx::SessionRepo::new("/c/a");
        let events = [
            st(1, queued("a")),
            st(
                2,
                Event::Session(SessionEvent::Cloned {
                    job: "a".into(),
                    workdir: dir.clone(),
                    branch: "relay/a".into(),
                    base: None,
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

    /// Audit findings 10 and 15: a dispatch records the model and provider its seat is on, and a
    /// job that goes back in the queue has no account until the next dispatch chooses one.
    #[test]
    fn a_dispatch_records_its_seat_and_a_requeue_gives_the_account_back() {
        let dispatched = st(
            2,
            Event::Dispatch(DispatchEvent::Dispatched {
                job: "a".into(),
                account: "omp:default".into(),
                model: Some("openai-codex/gpt-6.1".into()),
                provider: "openai-codex".into(),
                runner_pid: 1,
                runner_started: 1,
            }),
        );
        let w = World::fold(&[st(1, queued("a")), dispatched.clone()]);
        let j = &w.jobs["a"];
        assert_eq!(j.seat(), Some(("omp:default", Draws::Provider("openai-codex".into()))));
        assert_eq!(j.model.as_deref(), Some("openai-codex/gpt-6.1"));
        for back in [ended("a", Outcome::Retry), ended("a", Outcome::Rerun)] {
            let w = World::fold(&[st(1, queued("a")), dispatched.clone(), st(3, back)]);
            let j = &w.jobs["a"];
            assert_eq!((j.status, j.account.as_deref(), j.seat()), (Status::Queued, None, None), "{j:?}");
            assert_eq!(j.model.as_deref(), Some("openai-codex/gpt-6.1"), "what it last ran stays known");
        }
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "go on".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        });
        let w = World::fold(&[st(1, queued("a")), dispatched, st(3, ended("a", Outcome::Failed)), st(4, resumed)]);
        assert_eq!(w.jobs["a"].account, None, "resumed by hand: queued, no account");
        // An event from before the seat was recorded still folds; its seat is the job's own model's provider.
        let old = r#"{"type":"dispatched","job":"a","account":"claude:w","runner_pid":1,"runner_started":1}"#;
        let old: DispatchEvent = serde_json::from_str(old).unwrap();
        let w = World::fold(&[st(1, queued("a")), st(2, Event::Dispatch(old))]);
        assert_eq!((w.jobs["a"].seat(), w.jobs["a"].model.as_deref()), (Some(("claude:w", Draws::Account)), None));
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
                    model: None,
                    provider: String::new(),
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
                model: None,
                provider: String::new(),
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
                    model: None,
                    provider: String::new(),
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
                    family: None,
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

    /// Review of #64: every way an account gets a good reading ends what a failed one said, and a
    /// failure after it starts a streak of its own.
    #[test]
    fn a_session_rate_ends_the_streak_of_failed_reads_too() {
        let bad = |t| {
            let why = Some(ReadFailure::TimedOut { secs: 180 });
            st(t, Event::Quota(QuotaEvent::Unreadable { account: "x".into(), why }))
        };
        let rate = st(5, Event::Session(SessionEvent::Rate { account: "x".into(), meters: Meters::new() }));
        let w = World::fold(&[bad(2), bad(3), rate.clone()]);
        assert_eq!(w.accounts["x"].failure(), None, "a session that streamed the account's meters read it");
        let w = World::fold(&[bad(2), bad(3), rate, bad(7)]);
        let f = w.accounts["x"].failure().unwrap();
        assert_eq!((f.tries, f.since), (1, 7), "the earlier failures are before the reading");
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
                    model: None,
                    provider: String::new(),
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
    fn an_answer_counts_only_in_time_and_only_the_runner_records_the_action() {
        let d = |t, e| st(t, Event::Decide(e));
        let asked = DecideEvent::DecisionRequested {
            id: "d".into(),
            job: "a".into(),
            point: Point::Ended,
            created: 1,
            attempt: 1,
            waiter: None,
        };
        let dispatched = Event::Dispatch(DispatchEvent::Dispatched {
            job: "a".into(),
            account: "x".into(),
            model: None,
            provider: String::new(),
            runner_pid: 9,
            runner_started: 1,
        });
        let start = || {
            vec![
                st(1, queued("a")),
                st(1, dispatched.clone()),
                d(2, asked.clone()),
                d(3, DecideEvent::Parked { id: "d".into(), until: 100 }),
            ]
        };
        let answered = |a| DecideEvent::Answered { id: "d".into(), action: a, why: "w".into() };
        let decided = |action, refused: Option<&str>| DecideEvent::Decided {
            id: "d".into(),
            handler: "orchestrator".into(),
            action,
            why: String::new(),
            cost_usd: 0.0,
            refused: refused.map(Into::into),
        };
        let fold = |more: Vec<Stamped>| World::fold(&[start(), more].concat()).decisions["d"].clone();
        let late = fold(vec![d(100, answered(Action::Accept))]);
        assert_eq!(late.answer, None, "past its deadline");
        let w = World::fold(&start());
        assert!(w.awaiting(&w.decisions["d"], 99) && !w.awaiting(&w.decisions["d"], 100));
        let stop = Action::Stop { why: "x".into() };
        let refused = fold(vec![d(50, answered(stop.clone())), d(60, decided(stop.clone(), Some("no")))]);
        assert_eq!((refused.answer, refused.action), (Some(stop.clone()), None), "answered, never carried out");
        let closed = fold(vec![d(60, decided(Action::Defer, None)), d(70, answered(Action::Accept))]);
        assert_eq!((closed.answer, closed.action), (None, None), "the timeout closed it");
        let carried = fold(vec![d(50, answered(stop.clone())), d(51, decided(stop.clone(), None))]);
        assert_eq!(carried.action, Some(stop));
    }

    /// Audit finding 18: a decision is about one attempt of its job; once the job moves on, an
    /// answer to it is not heard, though its deadline has not passed.
    #[test]
    fn an_answer_to_a_decision_about_an_attempt_that_moved_on_is_not_heard() {
        let d = |t, e| st(t, Event::Decide(e));
        let asked = |id: &str, point, attempt| DecideEvent::DecisionRequested {
            id: id.into(),
            job: "a".into(),
            point,
            created: 1,
            attempt,
            waiter: None,
        };
        let parked = |id: &str| DecideEvent::Parked { id: id.into(), until: 1_000 };
        let dispatched = |t| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "x".into(),
                    model: None,
                    provider: String::new(),
                    runner_pid: 9,
                    runner_started: t,
                }),
            )
        };
        let follow_up = Action::FollowUp { brief: "b".into(), profile: None, after: true, fork: false, name: None };
        let answered = |id: &str| DecideEvent::Answered { id: id.into(), action: follow_up.clone(), why: "w".into() };
        let resumed = st(
            5,
            Event::Queue(QueueEvent::Resumed {
                job: "a".into(),
                message: "mine".into(),
                budget_usd: None,
                models: BTreeMap::new(),
            }),
        );
        let parked_end = vec![st(1, queued("a")), dispatched(2), d(3, asked("d", Point::Ended, 1)), d(4, parked("d"))];
        let w = World::fold(&parked_end);
        assert!(w.awaiting(&w.decisions["d"], 10), "parked about the current attempt");

        // Resumed by hand while parked: queued, the same attempt number, and the decision is stale.
        let w = World::fold(&[parked_end.clone(), vec![resumed.clone(), d(6, answered("d"))]].concat());
        assert!(!w.awaiting(&w.decisions["d"], 10));
        assert_eq!(w.decisions["d"].answer, None, "the follow-up is not heard");
        // Started again: a new attempt; still stale.
        let w = World::fold(&[parked_end.clone(), vec![resumed, dispatched(6), d(7, answered("d"))]].concat());
        assert_eq!((w.jobs["a"].status, w.decisions["d"].answer.clone()), (Status::Running, None));
        // Reaped while parked.
        let reaped = st(5, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() }));
        let w = World::fold(&[parked_end, vec![reaped, d(6, answered("d"))]].concat());
        assert_eq!(w.decisions["d"].answer, None);

        // A `held` decision is about a queued job, and stale once it starts.
        let held = vec![st(1, queued("a")), d(2, asked("h", Point::Held, 0)), d(3, parked("h"))];
        let w = World::fold(&held);
        assert!(w.awaiting(&w.decisions["h"], 10));
        let w = World::fold(&[held, vec![dispatched(4)]].concat());
        assert!(!w.awaiting(&w.decisions["h"], 10));
    }

    #[test]
    fn a_force_lands_only_on_a_queued_job() {
        let dispatched = st(
            2,
            Event::Dispatch(DispatchEvent::Dispatched {
                job: "a".into(),
                account: "x".into(),
                model: None,
                provider: String::new(),
                runner_pid: 9,
                runner_started: 2,
            }),
        );
        let forced = st(3, Event::Queue(QueueEvent::Forced { job: "a".into() }));
        assert!(!World::fold(&[st(1, queued("a")), dispatched, forced.clone()]).jobs["a"].forced, "it had started");
        assert!(World::fold(&[st(1, queued("a")), forced]).jobs["a"].forced);
    }

    #[test]
    fn only_the_current_attempt_is_heard() {
        let dispatched = |t| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "x".into(),
                    model: None,
                    provider: String::new(),
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

    /// Audit finding 8: a job parked at the orchestrator is `checking`, and its runner can die there.
    #[test]
    fn a_job_held_by_a_runner_moves_on_its_own_even_while_checking() {
        let dispatched = Event::Dispatch(DispatchEvent::Dispatched {
            job: "a".into(),
            account: "x".into(),
            model: None,
            provider: String::new(),
            runner_pid: 9,
            runner_started: 1,
        });
        let parked = Event::Decide(DecideEvent::DecisionRequested {
            id: "d".into(),
            job: "a".into(),
            point: Point::Ended,
            created: 1,
            attempt: 1,
            waiter: None,
        });
        let moves = |evs: &[Stamped]| {
            let w = World::fold(evs);
            (w.jobs["a"].status, w.moves_on_its_own(&w.jobs["a"]))
        };
        let base = [st(1, queued("a")), st(2, dispatched)];
        assert_eq!(moves(&base[..1]), (Status::Queued, true));
        assert_eq!(moves(&base), (Status::Running, true));
        assert_eq!(
            moves(&[base.to_vec(), vec![st(3, parked)]].concat()),
            (Status::Checking, true),
            "a runner holds it"
        );
        let reaped = st(4, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() }));
        let failed = [base.to_vec(), vec![reaped]].concat();
        assert_eq!(moves(&failed), (Status::Failed, false));
        // Blocked behind the failed job: live, but it waits for the operator.
        let w = World::fold(&[failed, vec![st(5, queued_after("b", &["a"]))]].concat());
        assert!(w.blocked(&w.jobs["b"]) && !w.moves_on_its_own(&w.jobs["b"]));
    }

    /// PR #39 review: blocking reaches through a chain of queued jobs, and a cycle never starts.
    #[test]
    fn a_job_behind_a_blocked_job_is_blocked_too() {
        let reaped = Event::Dispatch(DispatchEvent::Reaped { job: "a".into() });
        let dispatched = Event::Dispatch(DispatchEvent::Dispatched {
            job: "a".into(),
            account: "x".into(),
            model: None,
            provider: String::new(),
            runner_pid: 9,
            runner_started: 1,
        });
        let chain = vec![
            st(1, queued("a")),
            st(2, queued_after("b", &["a"])),
            st(3, queued_after("c", &["b"])),
            st(4, queued_after("d", &["a", "c"])),
        ];
        let w = World::fold(&chain);
        assert!(["b", "c", "d"].iter().all(|n| !w.blocked(&w.jobs[*n])), "a is queued: all of them wait");
        let w = World::fold(&[chain, vec![st(5, dispatched), st(6, reaped)]].concat());
        assert!(w.blocked(&w.jobs["b"]));
        let c = &w.jobs["c"];
        assert!(w.blocked(c) && !w.moves_on_its_own(c), "c waits for b, which waits for a failed job");
        assert_eq!(Wait::summary(&w.waits(c)).as_deref(), Some("waits for b (blocked)"));
        assert_eq!(Wait::summary(&w.waits(&w.jobs["d"])).as_deref(), Some("waits for a (failed), c (blocked)"));
        // Two queued jobs that wait for each other never start.
        let w = World::fold(&[st(1, queued_after("x", &["y"])), st(2, queued_after("y", &["x"]))]);
        assert!(w.blocked(&w.jobs["x"]) && w.blocked(&w.jobs["y"]));
    }

    /// PR #39 fourth review: the walk keeps its own stack, so a chain of any depth cannot exhaust the
    /// thread's. A hundred thousand jobs, each after the one before, behind a failed one.
    #[test]
    fn a_very_deep_chain_is_walked_without_recursion() {
        let n = 100_000;
        let mut w = World::default();
        let jobs = w.jobs_mut();
        // Named in reverse, so the walk meets the deepest job first and must go all the way down.
        let name = move |i: usize| format!("d{:06}", n - 1 - i);
        jobs.insert(name(0), Job { name: name(0), status: Status::Failed, ..Job::default() });
        for i in 1..n {
            jobs.insert(
                name(i),
                Job { name: name(i), status: Status::Queued, after: vec![name(i - 1)], ..Job::default() },
            );
        }
        // On a thread with a small stack, which a walk that recursed per job would overflow.
        let walked = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let last = &w.jobs()[&name(n - 1)];
                (w.blocked(last), w.moves_on_its_own(last), Wait::summary(&w.waits(last)))
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(walked, (true, false, Some(format!("waits for {} (blocked)", name(n - 2)))));
    }

    /// PR #39 reviews 2 and 3: the dependency analysis is built once per state of the jobs and shared by
    /// every query, in work linear in the jobs and their `after` lists. Bounded for a whole status pass
    /// (every query the board, dispatch and the wait make, for every job) over a long chain and a dense
    /// graph, counting every step of every build, not wall time.
    #[test]
    fn a_whole_pass_of_dependency_queries_costs_one_linear_walk() {
        let pass = |w: &World| {
            BUILD_STEPS.with(|c| c.set(0));
            for j in w.jobs().values() {
                let _ = (w.waits(j), w.blocked(j), w.startable(j), w.moves_on_its_own(j));
            }
            BUILD_STEPS.with(std::cell::Cell::get)
        };
        let fail_first = |evs: &mut Vec<Stamped>, first: &str| {
            let dispatched = Event::Dispatch(DispatchEvent::Dispatched {
                job: first.into(),
                account: "x".into(),
                model: None,
                provider: String::new(),
                runner_pid: 9,
                runner_started: 1,
            });
            evs.push(st(10_000, dispatched));
            evs.push(st(10_001, Event::Dispatch(DispatchEvent::Reaped { job: first.into() })));
        };
        // A chain of 300: each after the one before.
        let names: Vec<String> = (0..300).map(|i| format!("c{i:03}")).collect();
        let mut evs = vec![st(1, queued(&names[0]))];
        for (i, pair) in names.windows(2).enumerate() {
            evs.push(st(2 + i as u64, queued_after(&pair[1], &[pair[0].as_str()])));
        }
        let (jobs, edges) = (names.len(), names.len() - 1);
        for failed in [false, true] {
            if failed {
                fail_first(&mut evs, &names[0]);
            }
            let w = World::fold(&evs);
            let steps = pass(&w);
            assert!(steps <= 2 * (jobs + edges), "a chain of {jobs}: {steps} steps");
            assert_eq!(w.blocked(&w.jobs()[&names[299]]), failed);
        }
        // Twenty, each after every earlier one: 2^19 paths from the last to the first.
        let names: Vec<String> = (0..20).map(|i| format!("m{i:02}")).collect();
        let mut evs = vec![st(1, queued(&names[0]))];
        for i in 1..names.len() {
            let after: Vec<&str> = names[..i].iter().map(String::as_str).collect();
            evs.push(st(1 + i as u64, queued_after(&names[i], &after)));
        }
        let (jobs, edges) = (names.len(), names.len() * (names.len() - 1) / 2);
        fail_first(&mut evs, &names[0]);
        let w = World::fold(&evs);
        let steps = pass(&w);
        assert!(steps <= 2 * (jobs + edges), "a dense graph of {jobs}: {steps} steps");
        assert!(w.jobs().values().skip(1).all(|j| w.blocked(j)));
        // A change drops the analysis; the next query builds it again, once.
        let mut w = w;
        w.jobs_mut().remove(&names[0]);
        let steps = pass(&w);
        assert!(steps <= 2 * (jobs + edges), "{steps} steps after a change");
        assert_eq!(Wait::summary(&w.waits(&w.jobs()[&names[1]])).as_deref(), Some("waits for m00 (gone)"));
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
            model: None,
            provider: String::new(),
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
                model: None,
                provider: String::new(),
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

    /// PR #39 sixth review, issue 3: a `held` decision of a queued job asks about it until the job is
    /// forgotten; then it asks about nothing, not even a new job of the same name.
    #[test]
    fn a_forgotten_jobs_decision_no_longer_asks() {
        let asked = Event::Decide(DecideEvent::DecisionRequested {
            id: "d".into(),
            job: "a".into(),
            point: Point::Held,
            created: 1,
            attempt: 0,
            waiter: None,
        });
        let mut evs = vec![st(1, queued("a")), st(2, asked)];
        assert!(World::fold(&evs).still_asks("d"), "asked about the queued job as it is");
        evs.push(st(3, Event::Land(LandEvent::Forgotten { job: "a".into() })));
        assert!(!World::fold(&evs).still_asks("d"), "forgotten, it asks about nothing");
        evs.push(st(4, queued("a")));
        assert!(!World::fold(&evs).still_asks("d"), "nor about a new job of the same name");
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
            a.meters.insert("five_hour".into(), Meter { utilization, resets_at: None, ..Meter::default() });
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

        /// Review of #64: A fails at 10 and 30, B reads fine at 20. The failure at 30 is after the reading,
        /// so it stands, as one try (the one at 10 is before the reading and no longer counts).
        #[test]
        fn a_failure_after_the_other_projects_reading_keeps_its_reason_and_restarts_the_streak() {
            let timed_out = ReadFailure::TimedOut { secs: 180 };
            let a = World::fold(&[
                super::st(
                    10,
                    Event::Quota(QuotaEvent::Unreadable { account: "x".into(), why: Some(timed_out.clone()) }),
                ),
                super::st(
                    30,
                    Event::Quota(QuotaEvent::Unreadable { account: "x".into(), why: Some(timed_out.clone()) }),
                ),
            ]);
            let b = World::fold(&[super::st(
                20,
                Event::Quota(QuotaEvent::Reading { account: "x".into(), meters: Meters::new(), sign_in: None }),
            )]);
            for merged in
                [merge_account(&a.accounts["x"], &b.accounts["x"]), merge_account(&b.accounts["x"], &a.accounts["x"])]
            {
                let f = merged.failure().expect("the failure at 30 is after the reading at 20");
                assert_eq!((f.tries, f.since, f.last, &f.why), (1, 30, 30, &timed_out));
            }
            // A reading after the last failure ends the streak whichever project took it.
            let late = World::fold(&[super::st(
                40,
                Event::Quota(QuotaEvent::Reading { account: "x".into(), meters: Meters::new(), sign_in: None }),
            )]);
            assert_eq!(merge_account(&a.accounts["x"], &late.accounts["x"]).failure(), None);
            // No reading in between: the streak is as it was.
            let f = merge_account(&a.accounts["x"], &AccountState::default()).failing.unwrap();
            assert_eq!((f.tries, f.since, f.last), (2, 10, 30));
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
        use std::path::PathBuf;

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

        /// Copilot review of PR #41, finding 3: a checkpoint folded by fold 12, before a job back in
        /// the queue lost its account, is folded again from the bus rather than advanced.
        #[test]
        fn a_checkpoint_from_before_requeues_cleared_the_account_is_folded_again() {
            let b = Fixture::new();
            let retried = r#"{"t":3,"type":"ended","job":"a","outcome":"retry","cost_usd":0.0,"turns":1,"reason":null}
"#;
            b.append("queue.ndjson", &queued(1, "a"));
            b.append("dispatch.ndjson", &dispatched(2, "a"));
            b.append("session/a.1.ndjson", retried);
            assert_eq!(b.load().jobs["a"].account, None);
            // What fold 12 made of the same bus: queued again, its old account kept.
            let mut c = b.checkpoint();
            c["version"] = format!("{}+fold12", env!("CARGO_PKG_VERSION")).into();
            c["world"]["jobs"]["a"]["account"] = "c:w".into();
            std::fs::write(&b.file, serde_json::to_vec(&c).unwrap()).unwrap();
            let w = b.load();
            assert_eq!((w.jobs["a"].status, w.jobs["a"].account.as_deref()), (Status::Queued, None));
            assert_eq!(w, b.full());
            assert_eq!(b.checkpoint()["version"], fold_version());
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
