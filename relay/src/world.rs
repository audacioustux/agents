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

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if takes a reference")]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools, reason = "each is a fact of its own about the job, recorded by its own event")]
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
    /// Stopped by the disk floor, not by a person: the dispatcher resumes it when free space is back
    /// (`reason` says what it saw). A person's stop or resume clears it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// When the dispatcher last resumed it from that hold (bus time): the next held job waits while it is queued (an
    /// automatic requeue keeps this) or has run less than a disk sample. A hand resume clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unheld_at: Option<u64>,
    /// How long after `unheld_at` that resume settles (bus time); 0 for one recorded before it was.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub settle_us: u64,
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
    /// Queued as compiling nothing (its profile's `compiles = false`, or `--compiles false`): the one
    /// thing that puts a job outside the machine's `max_compiling` ([`Job::compiles`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_compile: bool,
    /// In line for a unit of a machine-wide pool, as its project's dispatcher last recorded
    /// (`DispatchEvent::InLine`): other projects' younger jobs wait behind it ([`Job::in_line_for`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_line: Option<InLine>,
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
    /// For a reviewed job: its review chain stopped short of an accept, and why
    /// (`QueueEvent::ReviewHalted`), until the builder or its reviewer is resumed or judged again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_halted: Option<String>,
    /// For a build job: its branch's tip as its latest run ended (`SessionEvent::Ended::head`). An accept of
    /// another commit is not an accept of this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
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
    /// When it came in (bus time): a builder run that ended after it answered it, whatever its tip.
    #[serde(default)]
    pub at: u64,
}

impl Reviewed {
    /// A reject, or a verdict on a commit the branch has moved past: the reviewer is to judge the branch again
    /// once its builder is done.
    pub fn wants_again(&self) -> bool {
        self.stale_head.is_some() || self.verdict == bus::Verdict::Reject
    }

    /// Whether the reviewer is to judge `b`, its builder, again now: `b` is done, and the verdict does not stand
    /// for what `b` ended with. A reject `b` ran after (it answered, with a commit or without one); a verdict on
    /// a head the branch had left while it was judged; an accept of a commit other than the tip `b` ended at.
    /// Whichever end comes last (the builder's, or the review's when it was still ending) asks by this alone.
    pub fn due_again(&self, b: &Job) -> bool {
        b.status == Status::Done
            && (self.stale_head.is_some()
                || self.verdict == bus::Verdict::Reject && b.ended_at.is_some_and(|e| e > self.at)
                || b.head.as_ref().is_some_and(|h| *h != self.commit))
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

/// A runner process's claim on the machine's pools: recorded at its dispatch, ended by its recorded
/// exit ([`World::claims`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The job it was dispatched for (which may since have been cleaned away).
    pub job: String,
    /// Its pid and start time, together naming one process.
    pub runner: (u32, u64),
    /// The job was queued as compiling nothing: the claim is on no unit of `max_compiling`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_compile: bool,
}

/// The lock a runner's supervisors hold for as long as anything they started lives
/// (`supervised::holding`): `<state root>/claims/<pid>-<start>.lock`, in the machine's state, where
/// no project's cleanup reaches. A runner killed outright leaves its session's or its check's
/// process tree to its supervisor, which holds this until the last of it is reaped.
pub fn claim_lock(runner: (u32, u64)) -> std::path::PathBuf {
    claim_lock_in(&crate::ctx::state_root(), runner)
}

/// As [`claim_lock`], under the state root `root`.
fn claim_lock_in(root: &Path, runner: (u32, u64)) -> std::path::PathBuf {
    root.join("claims").join(format!("{}-{}.lock", runner.0, runner.1))
}

impl Claim {
    /// Whether the claim stands: its runner process lives (by pid and start time, as a seat's
    /// holder is judged), or something it started does (a supervisor still holds its lock).
    pub fn live(&self) -> bool {
        self.live_in(&crate::ctx::state_root())
    }

    /// As [`Claim::live`], with the claim locks under the state root `root`.
    fn live_in(&self, root: &Path) -> bool {
        crate::os::same_process(self.runner.0, self.runner.1) || crate::os::seat_held(&claim_lock_in(root, self.runner))
    }
}

/// A queued job's place in the line for a machine-wide pool, and the dispatcher that vouches for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InLine {
    /// The pool, as `Pool::key` names it.
    pub pool: String,
    /// The pid and start time of the dispatcher that judged it, together naming one process.
    pub planner: (u32, u64),
}

impl Job {
    pub fn readonly(&self) -> bool {
        self.kind.readonly()
    }

    /// Whether the job compiles: every job does, a read-only review too (it rebuilds the branch and
    /// runs its tests), unless it was queued as compiling nothing. Never inferred from its kind.
    pub fn compiles(&self) -> bool {
        !self.no_compile
    }

    /// Since when (bus time) it has waited in the queue: what orders the line for a machine-wide
    /// pool across projects. Its creation, for a job recorded before queueing times were.
    pub fn waiting_since(&self) -> u64 {
        if self.queued_at > 0 { self.queued_at } else { self.created_at }
    }

    /// Whether the job is in line for `pool` now, whatever its own project's fold last heard: still
    /// queued, and the dispatcher that put it in line still alive, since nobody else would start it
    /// or take it out of the line.
    pub fn in_line_for(&self, pool: &str) -> bool {
        self.status == Status::Queued
            && self
                .in_line
                .as_ref()
                .is_some_and(|l| l.pool == pool && crate::os::same_process(l.planner.0, l.planner.1))
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

    /// Back in the queue at `t`: no account until a dispatch gives it one, and at the end of any line.
    fn requeue(&mut self, t: u64) {
        (self.status, self.queued_at, self.account, self.in_line) = (Status::Queued, t, None, None);
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
    /// The seats decisions' `agent:` sessions hold, by decision id, from `seated` until `unseated`:
    /// kept apart from the decisions, which a job's cleanup forgets while its session may still run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seats: BTreeMap<String, SessionSeat>,
    /// Every runner process this project dispatched that is not recorded gone, each with what it
    /// claims of the machine's pools. Kept apart from the jobs, so nothing that happens to a job
    /// (its end, a hand resume, a new dispatch beside it, `land clean`, a dequeue, its name queued
    /// again) can erase the claim of a process that still runs. An entry leaves on one event alone,
    /// `runner-exited`, which the dispatcher writes once it has seen the process gone and its claim
    /// lock free: the fold never probes a process, and never drops an entry by count or by age.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<Claim>,
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
/// build; beside it, what the jobs that wait need kept (`World::needed`), built on its first query. Nothing
/// can leave either stale: `World::jobs` is private, and `jobs_mut` and the fold drop them.
#[derive(Default)]
struct Deps(std::sync::OnceLock<Analysis>, std::sync::OnceLock<std::collections::HashSet<String>>);

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

/// A seat a session that is no job's own run holds (a decision's `agent:` session), as its booking
/// recorded it: the account, the model and provider it was judged on there, and the process that
/// runs the session (its decision's waiter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSeat {
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    pub waiter: Option<(u32, u64)>,
    /// Its lock file, held while its booker or any process of its session's tree lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<std::path::PathBuf>,
}

impl SessionSeat {
    /// The seat it holds while its session may run: while anything holds its lock (its booker, or
    /// the session's supervised tree, which outlives a booker killed outright until the supervisor
    /// sees and ends it), or, booked before seats had locks, while its booker lives. One whose
    /// every holder died without a word holds nothing, since nobody is left to give it back.
    pub fn held(&self) -> Option<(&str, crate::accounts::Draws)> {
        let live = match &self.lock {
            Some(lock) => crate::os::seat_held(lock),
            None => self.waiter.is_some_and(|(pid, started)| crate::os::same_process(pid, started)),
        };
        if !live {
            return None;
        }
        let harness = self.account.split_once(':').map_or(self.account.as_str(), |(h, _)| h);
        Some((&self.account, crate::accounts::draws(self.model.as_deref(), &self.provider, harness)))
    }
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
/// 15: a quota read that fails records why (`failing`).
/// 16: a failure streak stands only after the last good reading (`Failing::last`).
/// 17: a decision's `agent:` session holds a seat (`seated`, `unseated`), kept apart from the
/// decisions (`seats`) so a job's cleanup keeps it.
/// 18: a meter says its window, provider and scope; a stored reset past its window reads as none.
/// 19: a queued job is in line for a machine-wide pool (`in-line`, `out-of-line`) and says whether it
/// compiles (`no_compile`); every runner dispatched claims its place until its recorded exit
/// (`claims`, `runner-exited`).
/// 20: a review chain that stopped short of an accept (`review_halted`), for the jobs that wait on it.
/// 21: a build job's tip as its run ended (`head`), which an accept must name.
/// 22: a verdict keeps when it came in (`Reviewed::at`), so a builder that answered it is told apart.
const FOLD_VERSION: u32 = 22;

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
    /// It is done, and the wait is for its review chain's accept (`state` says where the chain is).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub review: bool,
}

impl Wait {
    /// One line for a board: `after a, b` while they work, `after a (rejected; b at work)` while a review chain
    /// has yet to accept, `waits for a (failed)` once one cannot finish.
    pub fn summary(waits: &[Self]) -> Option<String> {
        let blocked: Vec<_> = waits.iter().filter(|w| w.blocked).collect();
        if blocked.is_empty() {
            let all: Vec<_> = waits
                .iter()
                .map(|w| if w.review { format!("{} ({})", w.job, w.state) } else { w.job.clone() })
                .collect();
            return (!all.is_empty()).then(|| format!("after {}", all.join(", ")));
        }
        let reasons: Vec<_> = blocked.iter().map(|w| format!("{} ({})", w.job, w.state)).collect();
        Some(format!("waits for {}", reasons.join(", ")))
    }
}

/// One edge of the effective dependency graph ([`Graph::deps`]): a job waits on `on`, because of its
/// `--after` entry `after` (the name a board shows), for `need`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub after: String,
    pub on: String,
    pub need: Need,
}

/// What a [`Dep`] waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// `on` is done.
    Done,
    /// `on`, a review of `builder`, has accepted the tip `builder` ended at, and `builder` is done. `via` is how
    /// the wait reached it.
    Accept { builder: String, via: Via },
    /// `on`, a builder that `review` has not accepted (a reject, a stale verdict, or an accept of a tip it has
    /// left), runs again: it is requeued behind that review, which then judges anew.
    Answer { review: String },
    /// `on` is a review replaced since: the wait went on to its replacement, and it still holds the chain's word.
    Replaced,
}

/// How a wait reached the review it needs an accept from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The job was queued `--after` the review itself.
    Direct,
    /// `--after` a review that a later one replaced.
    Superseded,
    /// `--after` the build job the review is chained to.
    Chain,
}

/// The effective dependency graph of a state of the jobs: the one answer to "what does this job wait on",
/// read by readiness (`Deps`), by the cycle check every transition that adds an edge or revives a job makes
/// (`World::closes_cycle`), and by cleanup (`World::waiters`), so none of them can disagree.
pub struct Graph<'w> {
    jobs: &'w BTreeMap<String, Job>,
    /// Each build job's live review chain: its reviews that were not replaced or detached.
    chains: std::collections::HashMap<&'w str, Vec<&'w Job>>,
}

impl<'w> Graph<'w> {
    pub fn new(jobs: &'w BTreeMap<String, Job>) -> Self {
        let mut chains: std::collections::HashMap<&str, Vec<&Job>> = std::collections::HashMap::new();
        for r in jobs.values().filter(|r| r.unchained.is_none()) {
            if let Some(b) = r.review_of() {
                chains.entry(b).or_default().push(r);
            }
        }
        Self { jobs, chains }
    }

    /// What `j` waits on. For each `--after` entry `a`:
    /// - `a` a review: every review that replaced it, in turn (`Replaced`), the one the replacements end at
    ///   (`Accept`), and its builder, always: `Answer` while that review is done without an accept of the
    ///   builder's tip, else `Done`;
    /// - else `a` (`Done`), and unless `j` reviews `a`, each review chained to `a` (`Accept`) with, while it has
    ///   not accepted, `a` again (`Answer`).
    ///
    /// A name no job has is a `Done` edge to nothing.
    pub fn deps(&self, j: &Job) -> Vec<Dep> {
        let mut out = Vec::new();
        let dep = |after: &str, on: &str, need: Need| Dep { after: after.to_owned(), on: on.to_owned(), need };
        for a in &j.after {
            let Some(d) = self.jobs.get(a) else {
                out.push(dep(a, a, Need::Done));
                continue;
            };
            if let Some(builder) = d.review_of() {
                let mut at = d;
                // A chain of replacements is as long as the reviews of one job: bounded by the jobs.
                for _ in 0..self.jobs.len() {
                    match at.unchained.as_ref().and_then(|u| u.by.as_ref()).and_then(|n| self.jobs.get(n)) {
                        Some(n) => {
                            out.push(dep(a, &at.name, Need::Replaced));
                            at = n;
                        }
                        None => break,
                    }
                }
                let via = if at.name == *a { Via::Direct } else { Via::Superseded };
                out.push(dep(a, &at.name, Need::Accept { builder: builder.to_owned(), via }));
                // The builder, through the review's whole life: an accept is of its tip, so the wait needs it
                // whatever the review is doing (`Answer` while the review wants it to run again).
                let need = if self.unresolved(at) { Need::Answer { review: at.name.clone() } } else { Need::Done };
                out.push(dep(a, builder, need));
                continue;
            }
            out.push(dep(a, a, Need::Done));
            if j.review_of() == Some(a.as_str()) {
                continue;
            }
            for r in self.chains.get(a.as_str()).map_or(&[][..], Vec::as_slice) {
                out.push(dep(a, &r.name, Need::Accept { builder: a.clone(), via: Via::Chain }));
                if self.unresolved(r) {
                    out.push(dep(a, a, Need::Answer { review: r.name.clone() }));
                }
            }
        }
        out
    }

    /// The last verdict of the review `r` on its builder, and the builder.
    fn verdict(&self, r: &Job) -> Option<(&'w Job, &'w Reviewed)> {
        let b = self.jobs.get(r.review_of()?)?;
        Some((b, b.verdicts.iter().rev().find(|v| v.reviewer == r.name)?))
    }

    /// An accept by `r` of the tip its builder ended at, the builder done: what releases a wait on the chain. A
    /// build that ended before its tip was recorded has only the commit the review judged.
    fn accepts(&self, r: &Job) -> bool {
        r.status == Status::Done
            && self.verdict(r).is_some_and(|(b, v)| {
                b.status == Status::Done && !v.wants_again() && b.head.as_ref().is_none_or(|h| *h == v.commit)
            })
    }

    /// `r`, a review still in its builder's chain, is done with a verdict that does not release a wait: it is
    /// judged again once its builder runs again.
    fn unresolved(&self, r: &Job) -> bool {
        r.status == Status::Done && r.unchained.is_none() && self.verdict(r).is_some() && !self.accepts(r)
    }

    /// Whether `j` waits, so its edges hold: it has not started (queued, or held by the disk floor), or it is a
    /// review done without an accept, which its builder's next end revives.
    pub fn waiting(&self, j: &Job) -> bool {
        j.status == Status::Queued || j.held || self.unresolved(j)
    }

    /// What `dep` means for readiness: done, live, queued (whose blocking the walk settles) or blocked.
    fn state(&self, dep: &Dep) -> Edge<'w> {
        let Some(on) = self.jobs.get(&dep.on) else { return Edge::Blocked("gone".into(), false) };
        match &dep.need {
            Need::Done => plain(on),
            Need::Replaced => Edge::Done,
            Need::Answer { review } => {
                let said =
                    self.jobs.get(review).and_then(|r| self.verdict(r)).map_or_else(String::new, |(_, v)| {
                        if v.wants_again() { v.summary() } else { "accept".to_owned() }
                    });
                match plain(on) {
                    Edge::Done => Edge::Done,
                    e => e.for_review(|s| format!("{said}; {} {s}", on.name)),
                }
            }
            Need::Accept { builder, via } => {
                match on.unchained.as_ref().map(|u| u.by.as_ref()) {
                    Some(None) => return Edge::Blocked("review detached".into(), true),
                    Some(Some(n)) => return Edge::Blocked(format!("superseded by {n}, gone"), true),
                    None => {}
                }
                match (plain(on), via) {
                    (Edge::Done, _) => self.verdict_state(on, builder),
                    (e, Via::Direct) => e,
                    (e, Via::Superseded) => e.for_review(|s| format!("superseded by {} {s}", on.name)),
                    (e, Via::Chain) => e.for_review(|s| format!("in review: {} {s}", on.name)),
                }
            }
        }
    }

    /// What the verdict of `r`, a chained review that ended done, means for a wait on its chain: only an accept
    /// of the tip its builder ended at releases it; until the chain judges again it waits while the builder works
    /// and the re-review is due, and blocks once nothing will move: the builder failed or was stopped, or the
    /// chain halted (`review_halted`).
    fn verdict_state(&self, r: &Job, of: &str) -> Edge<'w> {
        let blocked = |s: String| Edge::Blocked(s, true);
        let Some(b) = self.jobs.get(of) else { return blocked(format!("{of} gone")) };
        let Some((_, v)) = self.verdict(r) else { return blocked("no verdict".into()) };
        let current = !v.wants_again() && b.head.as_ref().is_none_or(|h| *h == v.commit);
        let said = match () {
            () if v.wants_again() => v.summary(),
            () if current => "accept".to_owned(),
            () => "accept of an earlier tip".to_owned(),
        };
        match plain(b) {
            Edge::Done if current => Edge::Done,
            Edge::Done => match &b.review_halted {
                Some(why) => blocked(format!("{said}; {why}")),
                // The builder's end resumes the review in the same breath.
                None => Edge::Live(format!("{said}; re-review due"), true),
            },
            e => e.for_review(|s| format!("{said}; {of} {s}")),
        }
    }

    /// A shortest cycle through `start` among the edges of jobs that wait: each step `(job, why)`, `why` saying
    /// how the step before waits on it when that is not by name. `None` when there is none.
    pub fn cycle_through(&self, start: &str) -> Option<Vec<(String, Option<String>)>> {
        let mut parent: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
        let mut queue = std::collections::VecDeque::from([start.to_owned()]);
        while let Some(at) = queue.pop_front() {
            let Some(j) = self.jobs.get(&at).filter(|j| self.waiting(j)) else { continue };
            for d in self.deps(j) {
                let why = Self::why(&at, &d);
                if d.on == start {
                    let mut steps = vec![(start.to_owned(), why)];
                    let mut cur = at.clone();
                    while cur != start {
                        let (prev, why) = parent[&cur].clone();
                        steps.push((cur, why));
                        cur = prev;
                    }
                    steps.push((start.to_owned(), None));
                    steps.reverse();
                    return Some(steps);
                }
                if !parent.contains_key(&d.on) {
                    parent.insert(d.on.clone(), (at.clone(), why));
                    queue.push_back(d.on);
                }
            }
        }
        None
    }

    /// How `from` waits on `d.on`, when not by naming it.
    fn why(from: &str, d: &Dep) -> Option<String> {
        match &d.need {
            Need::Done | Need::Accept { via: Via::Direct, .. } => None,
            Need::Accept { via: Via::Chain, .. } => {
                Some(format!("{from} waits for {}, which {} reviews", d.after, d.on))
            }
            Need::Accept { via: Via::Superseded, .. } | Need::Replaced => {
                Some(format!("{from} waits for {}, which {} replaced", d.after, d.on))
            }
            Need::Answer { review } => Some(format!("{from} waits for {review}, which {} must answer", d.on)),
        }
    }

    /// What the jobs that wait need kept: every job an edge of theirs ends at ([`Graph::deps`]), and every job a
    /// queued one forks. gc keeps their clones, as cleanup keeps their records ([`Graph::waiters`]).
    pub fn needed(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for j in self.jobs.values().filter(|j| self.waiting(j)) {
            out.extend(self.deps(j).into_iter().map(|d| d.on));
            out.extend(j.fork.as_ref().map(|f| f.job.clone()));
        }
        out
    }

    /// The jobs that wait on `name`, directly or through other waiting jobs, by any edge: what removing `name`
    /// would release onto work it no longer holds, or block for good.
    pub fn waiters(&self, name: &str) -> Vec<String> {
        let mut back: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        let deps: Vec<(&str, Vec<Dep>)> =
            self.jobs.values().filter(|j| self.waiting(j)).map(|j| (j.name.as_str(), self.deps(j))).collect();
        for (from, ds) in &deps {
            for d in ds {
                back.entry(d.on.as_str()).or_default().push(from);
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = std::collections::VecDeque::from([name]);
        while let Some(at) = queue.pop_front() {
            for from in back.get(at).map_or(&[][..], Vec::as_slice) {
                if *from != name && seen.insert((*from).to_owned()) {
                    queue.push_back(from);
                }
            }
        }
        seen.into_iter().collect()
    }
}

/// The one walk that builds `Deps`, in two passes and no recursion, so no depth of chain can exhaust
/// the stack. First every queued job's blocking (`stuck`), by a depth-first walk on an explicit stack,
/// each job once however many paths reach it: linear in the jobs and their edges ([`Graph::deps`]). The
/// jobs on the stack are `path`, kept for constant-time membership: a job met on it is in a cycle of queued
/// jobs, which never starts, so that answer is a fact and is kept like any other. Then each job's
/// waits, read off those answers.
struct Walk<'w> {
    graph: Graph<'w>,
    stuck: std::collections::HashMap<&'w str, bool>,
    path: std::collections::HashSet<&'w str>,
    steps: usize,
}

/// A job on the walk's stack: its edges, the next one to look at, and whether it is blocked so far.
type Frame<'w> = (&'w Job, Vec<(String, Edge<'w>)>, usize, bool);

/// What one edge of a job means: blocked for good, waiting on a queued job (whose blocking is still to be
/// known), or neither. `review` marks a wait on a review chain's accept, `label` where the chain is.
enum Edge<'w> {
    Done,
    Blocked(String, bool),
    Live(String, bool),
    Queued(&'w Job, Option<String>),
}

impl Edge<'_> {
    /// The same answer, as a wait for a review chain whose state is `label`.
    fn for_review(self, label: impl Fn(&str) -> String) -> Self {
        match self {
            Edge::Done => Edge::Done,
            Edge::Blocked(s, _) => Edge::Blocked(label(&s), true),
            Edge::Live(s, _) => Edge::Live(label(&s), true),
            Edge::Queued(d, l) => {
                let s = l.unwrap_or_else(|| if d.held { "held".into() } else { d.status.as_str().into() });
                Edge::Queued(d, Some(label(&s)))
            }
        }
    }
}

/// What a dependency on `d` means by its own status alone.
fn plain(d: &Job) -> Edge<'_> {
    match d {
        d if d.status == Status::Done => Edge::Done,
        // A job the disk floor holds is the dispatcher's to resume once its own prerequisites can progress:
        // it is blocked as a queued job is, and so are the jobs that wait for it.
        d if d.status == Status::Queued || d.held => Edge::Queued(d, None),
        d if d.status.is_live() => Edge::Live(d.status.as_str().into(), false),
        d if d.reason.as_deref() == Some("dequeued") => Edge::Blocked("dequeued".into(), false),
        d => Edge::Blocked(d.status.as_str().into(), false),
    }
}

impl Deps {
    fn build(jobs: &BTreeMap<String, Job>) -> Analysis {
        let mut walk = Walk {
            graph: Graph::new(jobs),
            stuck: std::collections::HashMap::new(),
            path: std::collections::HashSet::new(),
            steps: 0,
        };
        for j in jobs.values().filter(|j| j.status == Status::Queued || j.held) {
            walk.settle(j);
        }
        let waits = jobs.values().map(|j| (j.name.clone(), walk.waits(j))).collect();
        #[cfg(test)]
        BUILD_STEPS.with(|c| c.set(c.get() + walk.steps));
        Analysis { waits }
    }
}

impl<'w> Walk<'w> {
    /// The edges of `job`, each with what it means now.
    fn edges(&self, job: &Job) -> Vec<(String, Edge<'w>)> {
        self.graph.deps(job).into_iter().map(|d| (d.after.clone(), self.graph.state(&d))).collect()
    }

    /// Works out whether `root` and every queued job under it is stuck, on an explicit stack of
    /// (job, its edges, the next one, blocked so far).
    fn settle(&mut self, root: &'w Job) {
        if self.stuck.contains_key(root.name.as_str()) {
            return;
        }
        let mut stack: Vec<Frame<'w>> = vec![(root, self.edges(root), 0, false)];
        self.path.insert(&root.name);
        self.steps += 1;
        while let Some(top) = stack.len().checked_sub(1) {
            let next = stack[top].2;
            if next < stack[top].1.len() {
                stack[top].2 += 1;
                self.steps += 1;
                let blocked = match &stack[top].1[next].1 {
                    Edge::Done | Edge::Live(..) => false,
                    Edge::Blocked(..) => true,
                    Edge::Queued(d, _) if self.path.contains(d.name.as_str()) => true,
                    Edge::Queued(d, _) => {
                        let d: &'w Job = d;
                        if let Some(&known) = self.stuck.get(d.name.as_str()) {
                            known
                        } else {
                            self.path.insert(&d.name);
                            self.steps += 1;
                            let edges = self.edges(d);
                            stack.push((d, edges, 0, false));
                            continue;
                        }
                    }
                };
                stack[top].3 |= blocked;
            } else {
                let (job, _, _, blocked) = stack.pop().expect("the stack has a top");
                self.path.remove(job.name.as_str());
                self.stuck.insert(&job.name, blocked);
                if let Some(parent) = stack.last_mut() {
                    parent.3 |= blocked;
                }
            }
        }
    }

    /// What `job` waits for: per `--after` entry, the most pressing of its edges (blocked, then queued, then
    /// live), read off the settled blocking of every queued job.
    fn waits(&mut self, job: &'w Job) -> Vec<Wait> {
        // One slot per `--after` entry, in the order the entries come, found by name in one step.
        let mut out: Vec<(Wait, u8)> = Vec::new();
        let mut slots: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (after, edge) in self.edges(job) {
            self.steps += 1;
            let wait = |state: &str, blocked, review| Wait { job: after.clone(), state: state.into(), blocked, review };
            let w = match edge {
                Edge::Done => None,
                Edge::Queued(d, _) if d.name == job.name => Some((wait("a cycle", true, false), 3)),
                Edge::Queued(d, label) if self.stuck.get(d.name.as_str()).copied().unwrap_or(false) => {
                    let review = label.is_some();
                    let state = label.map_or_else(|| "blocked".into(), |l| format!("{l}, blocked"));
                    Some((wait(&state, true, review), 3))
                }
                Edge::Queued(_, Some(label)) => Some((wait(&label, false, true), 2)),
                Edge::Queued(d, None) => Some((wait(if d.held { "held" } else { d.status.as_str() }, false, false), 2)),
                Edge::Live(state, review) => Some((wait(&state, false, review), 1)),
                Edge::Blocked(state, review) => Some((wait(&state, true, review), 3)),
            };
            let Some((w, rank)) = w else { continue };
            // The slot lookup is a step of its own: every comparison a pass makes is counted.
            self.steps += 1;
            match slots.get(&w.job) {
                Some(&i) if rank > out[i].1 => out[i] = (w, rank),
                Some(_) => {}
                None => {
                    slots.insert(w.job.clone(), out.len());
                    out.push((w, rank));
                }
            }
        }
        out.into_iter().map(|(w, _)| w).collect()
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

    /// The units of the machine's `max_compiling` this project holds now: one for each live claim
    /// of a runner whose job compiles. The process is the holder, never a job's status, so a unit
    /// can neither be lost while its work goes on nor kept once all of it has ended.
    pub fn compiling_units(&self) -> usize {
        self.claims.iter().filter(|c| !c.no_compile && c.live()).count()
    }

    /// Whether a compiling unit is held by a runner on its way out: alive, and no longer the
    /// runner its job has (past the job's `ended`, set aside by a resume, or the job gone). It
    /// exits with no event to say so.
    pub fn lingering(&self) -> bool {
        let current =
            |c: &Claim| self.jobs.get(&c.job).is_some_and(|j| j.status.has_runner() && j.runner == Some(c.runner));
        self.claims.iter().any(|c| !c.no_compile && !current(c) && c.live())
    }

    /// Whether `job` is queued with every dependency done: the dispatcher may give it a seat.
    pub fn startable(&self, job: &Job) -> bool {
        job.status == Status::Queued && self.waits(job).is_empty()
    }

    /// The effective dependency graph of these jobs ([`Graph`]).
    pub fn graph(&self) -> Graph<'_> {
        Graph::new(&self.jobs)
    }

    /// Whether a job that waits needs `name` kept ([`Graph::needed`]): built once per state of the jobs.
    pub fn needed(&self, name: &str) -> bool {
        self.deps.1.get_or_init(|| self.graph().needed()).contains(name)
    }

    /// Whether `name` may be cleaned away: the jobs that wait on it, directly or through others, by any edge
    /// of the graph (a review replaced since, a builder requeued behind its review), none when it may.
    /// Removing it would take the verdicts or the branch they wait on with it, and a forgotten job that was
    /// done counts as waited for: they would start on work no review accepted.
    pub fn waiters(&self, name: &str) -> Vec<String> {
        self.graph().waiters(name)
    }

    /// A cycle through `start` in the graph, as [`Graph::cycle_through`].
    pub fn cycle_through(&self, start: &str) -> Option<Vec<(String, Option<String>)>> {
        self.graph().cycle_through(start)
    }

    /// The cycle that `events` (a transition: a job added, resumed, requeued or replaced) would close through
    /// `node`, the job they add or revive, as one line naming it: `a → b → a (b waits for c, which a reviews)`.
    /// Every edge such a transition adds is out of `node` or into a review that replaces one, so a new cycle
    /// runs through `node`.
    pub fn closes_cycle(&self, events: impl IntoIterator<Item = Event>, node: &str) -> Option<String> {
        let mut next = self.clone();
        let t = bus::now_micros();
        for event in events {
            next.apply(&Stamped { t, event, from: None });
        }
        let cycle = next.cycle_through(node)?;
        let path: Vec<&str> = cycle.iter().map(|(j, _)| j.as_str()).collect();
        let why: Vec<&str> = cycle.iter().filter_map(|(_, w)| w.as_deref()).collect();
        let why = if why.is_empty() { String::new() } else { format!(" ({})", why.join("; ")) };
        Some(format!("{}{why}", path.join(" → ")))
    }

    /// Whether `job` is queued, or held by the disk floor, and can never start on its own until released or its
    /// dependency is queued again.
    pub fn blocked(&self, job: &Job) -> bool {
        (job.status == Status::Queued || job.held) && self.waits(job).iter().any(|w| w.blocked)
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

    /// Whether decision `id`'s `agent:` session holds a seat on record.
    pub fn seated(&self, id: &str) -> bool {
        self.seats.contains_key(id)
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
        job.status.has_runner() || (job.held || job.status == Status::Queued) && !self.blocked(job)
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
                compiles,
            } => {
                let j = Job {
                    no_compile: !compiles,
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
                self.chain_moves(job);
                if let Some(j) = self.jobs.get_mut(job) {
                    // Resumed by hand during its check: the checking runner no longer speaks for it.
                    // Its process is not stopped by this, and its claim on the machine is not the
                    // job's to drop: that stands until the process is recorded gone (`claims`).
                    if j.status == Status::Checking {
                        j.runner = None;
                    }
                    j.requeue(t);
                    j.message = Some(message.clone());
                    j.reason = None;
                    j.stop_requested = false;
                    j.held = false;
                    // A person took it over: it no longer settles as the dispatcher's resume.
                    j.unheld_at = None;
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
            QueueEvent::ReviewHalted { job, review, why } => self.halt_chain(job, review, why),
            QueueEvent::ReviewBriefed { job, brief } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.rebrief = Some(brief.clone());
                }
            }
            QueueEvent::StopRequested { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.stop_requested = true;
                    // A person stopping a held job takes it out of the floor's hands: it stays stopped.
                    if j.held {
                        (j.held, j.reason) = (false, Some("stopped by request".into()));
                    }
                }
            }
        }
    }

    /// `job` was resumed: the review chain it is part of moves again, as the builder answers or its reviewer
    /// judges again. A review that left the chain moves nothing of it.
    fn chain_moves(&mut self, job: &str) {
        let Some(j) = self.jobs.get(job) else { return };
        let builder = match j.review_of() {
            Some(b) if j.unchained.is_none() => b.to_owned(),
            Some(_) => return,
            None => job.to_owned(),
        };
        if let Some(b) = self.jobs.get_mut(&builder) {
            b.review_halted = None;
        }
    }

    /// The chain of `builder` halted where `review` left it. Only the live chain's halt stands: a replaced
    /// review's says nothing of the new one.
    fn halt_chain(&mut self, builder: &str, review: &str, why: &str) {
        let chained = self.jobs.get(review).is_some_and(|r| r.unchained.is_none());
        if let Some(b) = self.jobs.get_mut(builder).filter(|_| chained) {
            b.review_halted = Some(format!("{review}'s chain halted: {why}"));
        }
    }

    /// The review `job` leaves its build job's chain.
    fn unchain(&mut self, t: u64, job: &str, by: Option<&String>) {
        let builder = self.jobs.get(job).and_then(|r| r.review_of()).map(str::to_owned);
        if let Some(b) = builder.and_then(|b| self.jobs.get_mut(&b)) {
            // Its verdicts are history now, in the log; the board shows the chain that is.
            b.verdicts.retain(|v| v.reviewer != job);
            b.review_halted = None;
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
            DispatchEvent::RunnerExited { runner_pid, runner_started } => {
                self.claims.retain(|c| c.runner != (*runner_pid, *runner_started));
            }
            DispatchEvent::Dispatched { job, account, model, provider, runner_pid, runner_started } => {
                // The runner process claims its place on the machine from here until it is recorded
                // gone, whatever becomes of the job meanwhile: ended, resumed, cleaned away.
                if let Some(j) = self.jobs.get(job) {
                    let runner = (*runner_pid, *runner_started);
                    self.claims.retain(|c| c.runner != runner);
                    self.claims.push(Claim { job: job.clone(), runner, no_compile: j.no_compile });
                }
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
                    j.in_line = None;
                }
            }
            DispatchEvent::InLine { job, pool, planner_pid, planner_started } => {
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.status == Status::Queued) {
                    j.in_line = Some(InLine { pool: pool.clone(), planner: (*planner_pid, *planner_started) });
                }
            }
            DispatchEvent::OutOfLine { job } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.in_line = None;
                }
            }
            DispatchEvent::Unheld { job, message, settle_us, budget_usd } => {
                // A person who resumed or stopped it meanwhile was first: nothing is resumed twice.
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.held && j.status == Status::Stopped) {
                    j.requeue(t);
                    j.unheld_at = Some(t);
                    j.settle_us = *settle_us;
                    j.budget_usd = budget_usd.unwrap_or(j.budget_usd);
                    j.message = Some(message.clone());
                    j.reason = None;
                    j.held = false;
                    j.follow_ups = 0;
                    j.reruns = 0;
                }
            }
            DispatchEvent::HoldEnded { job, why } => {
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.held && j.status == Status::Stopped) {
                    (j.held, j.reason) = (false, Some(why.clone()));
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
            SessionEvent::BarChecked { .. } | SessionEvent::Reviewed { .. } => self.apply_judgement(t, e),
            SessionEvent::Checking { .. } | SessionEvent::Checked { .. } => self.apply_checkpoint(e),
            SessionEvent::Ended { job, outcome, cost_usd, turns, reason, head } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.cost_usd += cost_usd;
                    j.turns += turns;
                    if j.let_go() {
                        return;
                    }
                    j.reason.clone_from(reason);
                    j.ended_at = Some(t);
                    if head.is_some() {
                        j.head.clone_from(head);
                    }
                    // A person's stop, asked while the floor was stopping it, is the one that stands.
                    j.held = *outcome == Outcome::Held && !j.stop_requested;
                    j.status = match outcome {
                        Outcome::Done => Status::Done,
                        Outcome::Failed => Status::Failed,
                        Outcome::Stopped | Outcome::Held => Status::Stopped,
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
    fn apply_judgement(&mut self, t: u64, e: &SessionEvent) {
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
                    j.review_halted = None;
                    let r = Reviewed {
                        reviewer: job.clone(),
                        commit: commit.clone(),
                        verdict: *verdict,
                        reason: *reason,
                        family: family.clone(),
                        stale_head: stale_head.clone(),
                        at: t,
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
            // A booking for a decision no longer on record (its job cleaned away) is not one: its
            // booker sees that (`seated`) and stops. Once made, it stands until `unseated`, whatever
            // becomes of the decision, since its session runs until its handler hears and stops.
            DecideEvent::Seated { id, account, model, provider, lock } => {
                if let Some(d) = self.decisions.get(id) {
                    let (account, model, provider, lock) =
                        (account.clone(), model.clone(), provider.clone(), lock.clone());
                    self.seats.insert(id.clone(), SessionSeat { account, model, provider, waiter: d.waiter, lock });
                }
            }
            DecideEvent::Unseated { id } => {
                self.seats.remove(id);
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
                compiles: true,
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
            compiles: true,
        })
    }

    /// `job` resumed by hand: back in the queue.
    fn queued_again(job: &str) -> Event {
        Event::Queue(QueueEvent::Resumed {
            job: job.into(),
            message: "again".into(),
            budget_usd: None,
            models: BTreeMap::new(),
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
            compiles: true,
        })
    }

    fn ended(job: &str, outcome: Outcome) -> Event {
        Event::Session(SessionEvent::Ended {
            job: job.into(),
            outcome,
            cost_usd: 0.0,
            turns: 1,
            reason: None,
            head: None,
        })
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

    /// A queued job is in line for a machine-wide pool from its dispatcher's `in-line` until it is
    /// dispatched, taken out, or queued again; and only while that dispatcher lives.
    #[test]
    fn a_queued_job_is_in_line_until_it_starts_leaves_or_is_queued_again() {
        let me = std::process::id();
        let alive = crate::os::start_time(me).unwrap();
        let in_line = |started| {
            Event::Dispatch(DispatchEvent::InLine {
                job: "a".into(),
                pool: "machine.max_compiling".into(),
                planner_pid: me,
                planner_started: started,
            })
        };
        let lined = vec![st(1, queued("a")), st(2, in_line(alive))];
        let a = &World::fold(&lined).jobs["a"];
        assert!(a.in_line_for("machine.max_compiling") && !a.in_line_for("another"), "{:?}", a.in_line);
        assert_eq!(a.waiting_since(), 1);
        let with = |more: Vec<Stamped>| World::fold(&[lined.clone(), more].concat()).jobs["a"].clone();
        let out = Event::Dispatch(DispatchEvent::OutOfLine { job: "a".into() });
        assert_eq!(with(vec![st(3, out)]).in_line, None);
        let running = with(vec![st(3, dispatched("a"))]);
        assert_eq!(running.in_line, None, "a dispatch takes it out of the line");
        assert_eq!(running.runner, Some((9, 1)), "and its runner process is what holds a unit from here");
        let again = with(vec![st(3, dispatched("a")), st(4, ended("a", Outcome::Retry))]);
        assert_eq!((again.status, again.in_line.clone(), again.waiting_since()), (Status::Queued, None, 4));
        let late = with(vec![st(3, dispatched("a")), st(4, in_line(alive))]);
        assert_eq!(late.in_line, None, "only a queued job stands in line");
        let orphan = World::fold(&[st(1, queued("a")), st(2, in_line(alive + 1))]);
        assert!(!orphan.jobs["a"].in_line_for("machine.max_compiling"), "its dispatcher is gone");
        let quiet = |compiles| {
            let Event::Queue(QueueEvent::Queued {
                compiles: _,
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
            }) = queued("d")
            else {
                unreachable!()
            };
            Event::Queue(QueueEvent::Queued {
                compiles,
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
            })
        };
        assert!(World::fold(&[st(1, quiet(true))]).jobs["d"].compiles());
        assert!(!World::fold(&[st(1, quiet(false))]).jobs["d"].compiles(), "as it was queued");
        let old =
            r#"{"type":"queued","job":"o","task":"t","readonly":true,"effort":null,"budget_usd":1.0,"pinned":null}"#;
        let old: QueueEvent = serde_json::from_str(old).unwrap();
        assert!(World::fold(&[st(1, Event::Queue(old))]).jobs["o"].compiles(), "an event from before it was recorded");
    }

    /// `job` dispatched to the runner process `runner`.
    fn run_by(t: u64, job: &str, runner: (u32, u64)) -> Stamped {
        st(
            t,
            Event::Dispatch(DispatchEvent::Dispatched {
                job: job.into(),
                account: "x".into(),
                model: None,
                provider: String::new(),
                runner_pid: runner.0,
                runner_started: runner.1,
            }),
        )
    }

    /// This process, as a runner that is alive.
    fn live_runner() -> (u32, u64) {
        let me = std::process::id();
        (me, crate::os::start_time(me).unwrap())
    }

    /// The compiling units every project's dispatcher counts for the project whose fold is `w`.
    fn units(w: &World) -> usize {
        use crate::systems::dispatch::{Pool, Room};
        Room::of(Vec::new(), BTreeMap::new(), &World::default(), &[w], true).taken(&Pool::Compiling)
    }

    /// Copilot review of PR #70, finding 2: a compiling unit followed the job's status, so a runner
    /// still at work lost it: after its `ended` (the re-review comes after), and when a hand resume
    /// took a checking job from it. The unit is the runner process's, by pid and start time, from
    /// its dispatch until it exits; a dead one holds none, whatever the status says.
    #[test]
    fn a_compiling_unit_is_held_by_the_runner_process_until_it_exits_whatever_the_status() {
        let live = live_runner();
        let dead = (live.0, live.1 + 1);
        let running = vec![st(1, queued("a")), run_by(2, "a", live)];
        assert_eq!(units(&World::fold(&running)), 1);
        // Its run ended: the runner goes on (a re-review, waking the dispatcher) and keeps the unit.
        let ended = [running.clone(), vec![st(3, ended("a", Outcome::Done))]].concat();
        let w = World::fold(&ended);
        assert_eq!((w.jobs["a"].status, units(&w), w.lingering()), (Status::Done, 1, true));
        // Resumed by hand while it was being checked: the check it interrupts is not stopped.
        let checking = Event::Session(SessionEvent::Checking { job: "a".into(), checkpoint: 1 });
        let taken = [running.clone(), vec![st(3, checking), st(4, queued_again("a"))]].concat();
        let w = World::fold(&taken);
        assert_eq!((w.jobs["a"].status, w.jobs["a"].runner), (Status::Queued, None));
        assert_eq!((units(&w), w.lingering()), (1, true), "the old runner's unit is still held");
        // Dispatched again, to the same process or beside a dead one: one live process, one unit.
        assert_eq!(units(&World::fold(&[taken.clone(), vec![run_by(5, "a", live)]].concat())), 1);
        let w = World::fold(&[ended.clone(), vec![st(4, queued_again("a")), run_by(5, "a", dead)]].concat());
        assert_eq!((w.jobs["a"].status, units(&w), w.lingering()), (Status::Running, 1, true));
        // A dead runner holds nothing, though the fold still says `running`.
        let w = World::fold(&[st(1, queued("a")), run_by(2, "a", dead)]);
        assert_eq!((w.jobs["a"].status, units(&w), w.lingering()), (Status::Running, 0, false));
    }

    /// Copilot review of PR #70 (second round), finding 1: a job's claim lived on the job, so
    /// whatever removed the job from the fold (`land clean`'s `forgotten`, a dequeue, its name queued
    /// again) erased the claim of a runner still at work for it. Claims are the fold's own, apart
    /// from the jobs: the job goes, and the live runner's unit stays.
    #[test]
    fn a_live_runners_unit_outlasts_its_job_being_cleaned_away_dequeued_or_queued_again() {
        let live = live_runner();
        let checking = Event::Session(SessionEvent::Checking { job: "a".into(), checkpoint: 1 });
        // Resumed by hand during its check (the check runs on), then stopped where it stood.
        let set_aside = vec![st(1, queued("a")), run_by(2, "a", live), st(3, checking), st(4, queued_again("a"))];
        let with = |more: Vec<Stamped>| World::fold(&[set_aside.clone(), more].concat());
        let forgotten = with(vec![st(5, Event::Land(LandEvent::Forgotten { job: "a".into() }))]);
        assert!(!forgotten.jobs.contains_key("a"), "cleaned away");
        assert_eq!(units(&forgotten), 1, "its runner still lives, and still holds its unit");
        let dequeued = with(vec![st(5, Event::Queue(QueueEvent::Dequeued { job: "a".into() }))]);
        assert_eq!(units(&dequeued), 1, "dequeued: {:?}", dequeued.jobs.get("a").map(|j| j.status));
        let again = with(vec![st(5, Event::Queue(QueueEvent::Dequeued { job: "a".into() })), st(6, queued("a"))]);
        assert_eq!((again.jobs["a"].attempts, units(&again)), (0, 1), "a new job of the name; the old runner's unit");
        // Ended and cleaned while its runner still works (the re-review comes after `ended`).
        let ended = vec![st(1, queued("a")), run_by(2, "a", live), st(3, ended("a", Outcome::Done))];
        let cleaned = [ended, vec![st(4, Event::Land(LandEvent::Forgotten { job: "a".into() }))]].concat();
        assert_eq!(units(&World::fold(&cleaned)), 1);
    }

    /// Copilot review of PR #70 (second round), finding 3: the fold kept the latest four runners it
    /// had moved on from, so a fifth that was still alive lost its unit. Nothing is dropped by
    /// count: every live runner process of a job holds its unit.
    #[test]
    fn every_live_runner_a_job_was_ever_dispatched_to_holds_its_unit() {
        let mut children: Vec<std::process::Child> =
            (0..7).map(|_| std::process::Command::new("sleep").arg("60").spawn().unwrap()).collect();
        let runners: Vec<(u32, u64)> =
            children.iter().map(|c| (c.id(), crate::os::start_time(c.id()).unwrap())).collect();
        let mut events = vec![st(1, queued("a"))];
        events.extend(runners.iter().zip(2..).map(|(r, t)| run_by(t, "a", *r)));
        let held = units(&World::fold(&events));
        for c in &mut children {
            c.kill().unwrap();
            #[allow(clippy::disallowed_methods, reason = "a test reaps the children it killed")]
            c.wait().unwrap();
        }
        assert_eq!(held, 7, "seven live runner processes, one after another on one job");
        assert_eq!(units(&World::fold(&events)), 0, "all of them gone: nothing is held");
    }

    /// Copilot review of PR #70 (second round), finding 2: a runner killed outright released its
    /// unit while what it had started ran on. Its supervisors hold its claim lock until the last
    /// process of their trees is reaped (`supervised::holding`), and the claim stands that long.
    #[test]
    fn a_dead_runners_claim_stands_while_a_supervisor_of_its_tree_holds_its_lock() {
        let d = crate::config::tempdir::Dir::new();
        let live = live_runner();
        let claim = Claim { job: "a".into(), runner: (live.0, live.1 + 1), no_compile: false };
        assert!(!claim.live_in(d.path()), "its runner is gone, and nothing holds its lock");
        let lock = claim_lock_in(d.path(), claim.runner);
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let hold = crate::os::SeatHold::take(&lock).unwrap();
        assert!(claim.live_in(d.path()), "the check's supervisor still has processes to reap");
        drop(hold);
        assert!(!claim.live_in(d.path()), "the tree is reaped: the unit is free");
        assert!(Claim { runner: live, ..claim }.live_in(d.path()), "a live runner needs no lock");
        assert!(claim_lock((7, 9)).ends_with("claims/7-9.lock"), "{:?}", claim_lock((7, 9)));
    }

    /// The claims are kept until a recorded exit, and only that: not a count, not an age, and never a
    /// look at the process from inside the fold, which must make the same of the same bus anywhere.
    #[test]
    fn a_claim_leaves_the_fold_on_its_runners_recorded_exit_and_on_nothing_else() {
        let exited = |t, r: (u32, u64)| {
            st(t, Event::Dispatch(DispatchEvent::RunnerExited { runner_pid: r.0, runner_started: r.1 }))
        };
        // Runners that never existed: the fold cannot tell, and keeps every one of them.
        let mut events = vec![st(1, queued("a"))];
        events.extend((0..9u64).map(|i| run_by(10 + i, "a", (7, i))));
        events.push(st(30, ended("a", Outcome::Failed)));
        events.push(st(31, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() })));
        let w = World::fold(&events);
        let runners = |w: &World| w.claims.iter().map(|c| c.runner.1).collect::<Vec<_>>();
        assert_eq!(runners(&w), (0..9).collect::<Vec<u64>>(), "nine dispatches, nine claims, none cut off");
        assert!(w.claims.iter().all(|c| c.job == "a" && !c.no_compile));
        events.extend([exited(40, (7, 3)), exited(41, (7, 0)), exited(42, (9, 9))]);
        assert_eq!(runners(&World::fold(&events)), [1, 2, 4, 5, 6, 7, 8], "each recorded exit takes its own");
        // The same process dispatched twice is one claim; a job that compiles nothing claims no unit.
        let twice = World::fold(&[st(1, queued("a")), run_by(2, "a", (7, 1)), run_by(3, "a", (7, 1))]);
        assert_eq!(twice.claims.len(), 1);
        let Event::Queue(mut quiet) = queued("q") else { unreachable!() };
        if let QueueEvent::Queued { compiles, .. } = &mut quiet {
            *compiles = false;
        }
        let w = World::fold(&[st(1, Event::Queue(quiet)), run_by(2, "q", live_runner())]);
        assert_eq!((w.claims[0].no_compile, w.claims[0].live(), w.compiling_units()), (true, true, 0));
        // A dispatch for a job the fold does not have started nothing that could run for long.
        assert_eq!(World::fold(&[run_by(1, "nobody", (7, 1))]).claims.len(), 0);
    }

    #[test]
    fn the_fold_version_moves_with_the_fold() {
        const PINNED: &[(u32, u64)] = &[
            (13, 0x1d75_bf1b_971c_dfd9),
            (14, 0x0599_7a27_1a95_7bbd),
            (15, 0x5351_e6de_0d8d_bfd8),
            (16, 0x5351_e6de_0d8d_bfd8),
            (17, 0x2132_12e7_f06d_d32d),
            (18, 0x4bb9_80c4_5c80_027a),
            (19, 0xff2a_3110_25d7_0f54),
            (20, 0xff2a_3110_25d7_0f54),
            (21, 0xff2a_3110_25d7_0f54),
            (22, 0xff2a_3110_25d7_0f54),
        ];
        let dispatched = |t, model: Option<&str>, runner_pid| {
            st(
                t,
                Event::Dispatch(DispatchEvent::Dispatched {
                    job: "a".into(),
                    account: "omp:w".into(),
                    model: model.map(Into::into),
                    provider: model.and_then(|m| m.split_once('/')).map_or("", |(p, _)| p).into(),
                    runner_pid,
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
            dispatched(3, Some("openai-codex/gpt-6.1"), 7),
            st(4, Event::Session(SessionEvent::Limited { account: "omp:w".into(), until: 50 })),
            st(5, ended("a", Outcome::Retry)),
            dispatched(6, None, 7),
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
            st(
                12,
                Event::Decide(DecideEvent::DecisionRequested {
                    id: "d".into(),
                    job: "a".into(),
                    point: Point::Held,
                    created: 1,
                    attempt: 2,
                    waiter: None,
                }),
            ),
            st(
                13,
                Event::Decide(DecideEvent::Seated {
                    id: "d".into(),
                    account: "omp:w".into(),
                    model: Some("minimax-code/M3".into()),
                    provider: "minimax-code".into(),
                    lock: None,
                }),
            ),
            st(
                14,
                Event::Dispatch(DispatchEvent::InLine {
                    job: "b".into(),
                    pool: "machine.max_compiling".into(),
                    planner_pid: 7,
                    planner_started: 1,
                }),
            ),
            dispatched(15, None, 8), // beside its earlier run's runner, which the fold keeps
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

    fn queued_review(job: &str, of: &str) -> Event {
        let Event::Queue(QueueEvent::Queued { task, harnesses, models, effort, budget_usd, compiles, .. }) =
            queued(job)
        else {
            unreachable!()
        };
        Event::Queue(QueueEvent::Queued {
            job: job.into(),
            task,
            compiles,
            readonly: true,
            harnesses,
            models,
            effort,
            budget_usd,
            pinned: None,
            reuse_key: None,
            after: vec![of.into()],
            profile: None,
            fork: None,
            parent: None,
            depth: 0,
            acceptance: bus::Acceptance::ReviewOf(of.into()),
        })
    }

    fn reviewed(job: &str, of: &str, verdict: bus::Verdict) -> Event {
        let reason = (verdict == bus::Verdict::Reject).then_some(bus::Reason::TestFailure);
        Event::Session(SessionEvent::Reviewed {
            job: job.into(),
            of: of.into(),
            commit: "c1".into(),
            verdict,
            reason,
            family: None,
            stale_head: None,
        })
    }

    fn resumed(job: &str) -> Event {
        Event::Queue(QueueEvent::Resumed {
            job: job.into(),
            message: "again".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        })
    }

    /// `TheGrid`, 2026-11-13: beam/U6, queued after a review that rejected, started on the branch the review had
    /// just sent back. A job after a review waits for its accept, through the builder's answer and the
    /// re-review; so does a job after a build job with a review chain. Each wait says where the chain is.
    #[test]
    fn a_job_after_a_review_or_a_reviewed_build_waits_for_the_accept() {
        use bus::Verdict::{Accept, Reject};
        let mut events = vec![st(1, queued("b")), st(2, ended("b", Outcome::Done))];
        events.extend([
            st(3, queued_review("r", "b")),
            st(4, queued_after("d", &["r"])),
            st(5, queued_after("f", &["b"])),
        ]);
        let state = |events: &[Stamped], job: &str| {
            let w = World::fold(events);
            let j = &w.jobs[job];
            (w.startable(j), w.blocked(j), Wait::summary(&w.waits(j)))
        };
        let waits = |s: &str| (false, false, Some(s.to_owned()));
        let w = World::fold(&events);
        assert!(w.startable(&w.jobs["r"]), "the review waits for its build alone: {:?}", w.waits(&w.jobs["r"]));
        assert_eq!(state(&events, "d"), waits("after r"));
        assert_eq!(state(&events, "f"), waits("after b (in review: r queued)"), "the build is done, not accepted");

        // A reject resumes the builder, then the review ends: both dependents wait while the builder answers.
        events.extend([st(6, reviewed("r", "b", Reject)), st(7, resumed("b")), st(8, ended("r", Outcome::Done))]);
        assert_eq!(state(&events, "d"), waits("after r (reject: test-failure; b queued)"));
        assert_eq!(state(&events, "f"), waits("after b"));
        events.push(st(9, ended("b", Outcome::Done)));
        assert_eq!(state(&events, "d"), waits("after r (reject: test-failure; re-review due)"));
        assert_eq!(state(&events, "f"), waits("after b (reject: test-failure; re-review due)"));
        let answered = events.clone();
        events.push(st(10, resumed("r")));
        assert_eq!(state(&events, "d"), waits("after r"));
        assert_eq!(state(&events, "f"), waits("after b (in review: r queued)"));
        events.extend([st(11, reviewed("r", "b", Accept)), st(12, ended("r", Outcome::Done))]);
        assert_eq!(state(&events, "d"), (true, false, None), "the accept releases it");
        assert_eq!(state(&events, "f"), (true, false, None));

        // Where nothing will move the chain, the wait is blocked and says why.
        let blocked = |s: &str| (false, true, Some(s.to_owned()));
        let halted = [
            &answered[..],
            &[st(
                10,
                Event::Queue(QueueEvent::ReviewHalted {
                    job: "b".into(),
                    review: "r".into(),
                    why: "r has had 2 decisions already (hooks.max_decisions)".into(),
                }),
            )],
        ]
        .concat();
        let why = "reject: test-failure; r's chain halted: r has had 2 decisions already (hooks.max_decisions)";
        assert_eq!(state(&halted, "d"), blocked(&format!("waits for r ({why})")));
        assert_eq!(state(&halted, "f"), blocked(&format!("waits for b ({why})")));
        let again = [&halted[..], &[st(11, resumed("r"))]].concat();
        assert_eq!(state(&again, "d"), waits("after r"), "a resume by hand moves the chain again");

        let failed = [&answered[..8], &[st(9, ended("b", Outcome::Failed))]].concat();
        assert_eq!(state(&failed, "d"), blocked("waits for r (reject: test-failure; b failed)"));
        assert_eq!(state(&failed, "f"), blocked("waits for b (failed)"));

        let detached =
            [&answered[..], &[st(10, Event::Queue(QueueEvent::Unchained { job: "r".into(), by: None }))]].concat();
        assert_eq!(state(&detached, "d"), blocked("waits for r (review detached)"));
        assert_eq!(state(&detached, "f"), (true, false, None), "a build with no chain left is done");

        // A replaced review hands the wait to its replacement.
        let replaced = [
            &answered[..],
            &[
                st(10, queued_review("r2", "b")),
                st(11, Event::Queue(QueueEvent::Unchained { job: "r".into(), by: Some("r2".into()) })),
            ],
        ]
        .concat();
        assert_eq!(state(&replaced, "d"), waits("after r (superseded by r2 queued)"));
        assert_eq!(state(&replaced, "f"), waits("after b (in review: r2 queued)"));
        let accepted =
            [&replaced[..], &[st(12, reviewed("r2", "b", Accept)), st(13, ended("r2", Outcome::Done))]].concat();
        assert_eq!(state(&accepted, "d"), (true, false, None));
        assert_eq!(state(&accepted, "f"), (true, false, None));
    }

    fn unchained(job: &str, by: &str) -> Event {
        Event::Queue(QueueEvent::Unchained { job: job.into(), by: Some(by.into()) })
    }

    /// Copilot on PR #69: a review replaced before it ran is stopped, and so may its replacement be. What waits
    /// on the first follows the replacements to the live review, whatever their status, and its accept releases it.
    #[test]
    fn a_job_after_a_replaced_review_follows_the_replacements_whatever_their_status() {
        let mut events = vec![st(1, queued("b")), st(2, ended("b", Outcome::Done)), st(3, queued_review("r", "b"))];
        events.extend([st(4, queued_after("d", &["r"])), st(5, queued_review("r2", "b")), st(6, unchained("r", "r2"))]);
        let state = |events: &[Stamped]| {
            let w = World::fold(events);
            let j = &w.jobs["d"];
            (w.startable(j), w.blocked(j), Wait::summary(&w.waits(j)))
        };
        let w = World::fold(&events);
        assert_eq!(w.jobs["r"].status, Status::Stopped, "a queued review replaced is stopped");
        assert_eq!(state(&events), (false, false, Some("after r (superseded by r2 queued)".into())));
        events.extend([st(7, queued_review("r3", "b")), st(8, unchained("r2", "r3"))]);
        assert_eq!(state(&events), (false, false, Some("after r (superseded by r3 queued)".into())));
        events.extend([st(9, reviewed("r3", "b", bus::Verdict::Accept)), st(10, ended("r3", Outcome::Done))]);
        assert_eq!(state(&events), (true, false, None), "the last replacement's accept releases it");
    }

    /// Copilot on PR #69: resuming a review that left the chain moves nothing of it, so the live chain's halt stands.
    #[test]
    fn only_the_builder_or_its_chained_review_resumed_clears_a_halt() {
        use bus::Verdict::Reject;
        let events = [
            st(1, queued("b")),
            st(2, ended("b", Outcome::Done)),
            st(3, queued_review("r", "b")),
            st(4, ended("r", Outcome::Done)),
            st(5, queued_review("r2", "b")),
            st(6, unchained("r", "r2")),
            st(7, queued_after("f", &["b"])),
            st(8, reviewed("r2", "b", Reject)),
            st(9, resumed("b")),
            st(10, ended("r2", Outcome::Done)),
            st(11, ended("b", Outcome::Done)),
            st(12, Event::Queue(QueueEvent::ReviewHalted { job: "b".into(), review: "r2".into(), why: "cap".into() })),
        ];
        let blocked = |events: &[Stamped]| {
            let w = World::fold(events);
            (w.blocked(&w.jobs["f"]), Wait::summary(&w.waits(&w.jobs["f"])))
        };
        let halted = (true, Some("waits for b (reject: test-failure; r2's chain halted: cap)".into()));
        assert_eq!(blocked(&events), halted);
        let old = [&events[..], &[st(13, resumed("r"))]].concat();
        assert_eq!(blocked(&old), halted, "the replaced review's resume is not the chain's");
        let chained = [&events[..], &[st(13, resumed("r2"))]].concat();
        assert!(!blocked(&chained).0, "the chained review's resume moves the chain");
        let builder = [&events[..], &[st(13, resumed("b"))]].concat();
        assert!(!blocked(&builder).0, "and so does the builder's");
    }

    fn ended_at(job: &str, head: &str) -> Event {
        Event::Session(SessionEvent::Ended {
            job: job.into(),
            outcome: Outcome::Done,
            cost_usd: 0.0,
            turns: 1,
            reason: None,
            head: Some(head.into()),
        })
    }

    /// Copilot on PR #69: an accept names the tip it accepted. A builder resumed after it has work no review
    /// accepted: what waits on the chain waits while it works and until the chain accepts the tip it ends at.
    #[test]
    fn an_accept_releases_only_while_it_names_the_builders_tip() {
        let mut events = vec![st(1, queued("b")), st(2, ended_at("b", "c1")), st(3, queued_review("r", "b"))];
        events.extend([st(4, reviewed("r", "b", bus::Verdict::Accept)), st(5, ended("r", Outcome::Done))]);
        events.extend([st(6, queued_after("d", &["r"])), st(7, queued_after("f", &["b"]))]);
        let state = |events: &[Stamped], job: &str| {
            let w = World::fold(events);
            let j = &w.jobs[job];
            (w.startable(j), w.blocked(j), Wait::summary(&w.waits(j)))
        };
        let waits = |s: &str| (false, false, Some(s.to_owned()));
        assert_eq!(state(&events, "d"), (true, false, None), "the accept of c1, the tip");
        events.push(st(8, resumed("b")));
        assert_eq!(state(&events, "d"), waits("after r (accept; b queued)"), "a builder resumed by hand");
        assert_eq!(state(&events, "f"), waits("after b"));
        let same = [&events[..], &[st(9, ended_at("b", "c1"))]].concat();
        assert_eq!(state(&same, "d"), (true, false, None), "it ended where the accept was");
        events.push(st(9, ended_at("b", "c2")));
        assert_eq!(state(&events, "d"), waits("after r (accept of an earlier tip; re-review due)"));
        assert_eq!(state(&events, "f"), waits("after b (accept of an earlier tip; re-review due)"));
        let mut again = vec![st(10, resumed("r")), st(11, reviewed("r", "b", bus::Verdict::Accept))];
        if let Event::Session(SessionEvent::Reviewed { commit, .. }) = &mut again[1].event {
            *commit = "c2".into();
        }
        again.push(st(12, ended("r", Outcome::Done)));
        let accepted = [&events[..], &again[..]].concat();
        assert_eq!(state(&accepted, "d"), (true, false, None), "the accept of c2");
        assert_eq!(state(&accepted, "f"), (true, false, None));
    }

    /// Copilot on PR #69 (world.rs:1045): a rejected builder is requeued behind its review, so a job after that
    /// review waits on the builder too. A, B after A, RB rejects B and B is back in the queue; RA, a review of A
    /// after RB, then waits on B, which waits on A's chain, RA: a cycle the graph names and readiness agrees on.
    #[test]
    fn a_review_after_a_review_that_requeued_its_builder_is_a_cycle_through_the_builder() {
        use bus::Verdict::Reject;
        let mut w = World::fold(&[
            st(1, queued("a")),
            st(2, ended("a", Outcome::Done)),
            st(3, queued_after("b", &["a"])),
            st(4, ended("b", Outcome::Done)),
            st(5, queued_review("rb", "b")),
            st(6, reviewed("rb", "b", Reject)),
            st(7, resumed("b")),
            st(8, ended("rb", Outcome::Done)),
            st(9, queued_review("ra", "a")),
        ]);
        // RA is queued `--review-of a --after rb`.
        w.jobs_mut().get_mut("ra").unwrap().after.push("rb".into());
        let cycle = w.cycle_through("ra").expect("ra waits on b, which waits on a's review, ra");
        let path: Vec<&str> = cycle.iter().map(|(j, _)| j.as_str()).collect();
        assert_eq!(path, ["ra", "b", "ra"], "{cycle:?}");
        assert!(w.blocked(&w.jobs()["ra"]) && w.blocked(&w.jobs()["b"]), "readiness says the same");
    }

    /// Copilot on PR #69 (world.rs:811): a wait on a review needs its builder through the review's whole life,
    /// not only while the review wants it to run again. With the review accepted, the builder is still what the
    /// wait stands on, so it is not cleaned away; with the review still running, a builder resumed by its reject
    /// would close the cycle a review after that review makes, before the review's end shows it.
    #[test]
    fn a_wait_on_a_review_needs_its_builder_while_the_review_runs_and_after_it_accepts() {
        use bus::Verdict::{Accept, Reject};
        let accepted = World::fold(&[
            st(1, queued("build")),
            st(2, ended("build", Outcome::Done)),
            st(3, queued_review("rev", "build")),
            st(4, reviewed("rev", "build", Accept)),
            st(5, ended("rev", Outcome::Done)),
            st(6, queued_after("next", &["rev", "gate"])),
            st(7, queued("gate")),
        ]);
        assert_eq!(accepted.waiters("build"), ["next"], "next stands on build's accepted tip");

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
        let mut running = World::fold(&[
            st(1, queued("a")),
            st(2, ended("a", Outcome::Done)),
            st(3, queued_after("b", &["a"])),
            st(4, ended("b", Outcome::Done)),
            st(5, queued_review("rb", "b")),
            st(6, dispatched("rb")),
            st(7, reviewed("rb", "b", Reject)),
            st(8, queued_review("ra", "a")),
        ]);
        // RA is queued `--review-of a --after rb` while rb still runs.
        running.jobs_mut().get_mut("ra").unwrap().after.push("rb".into());
        let cycle = running.closes_cycle([resumed("b")], "b");
        assert_eq!(cycle.as_deref(), Some("b → ra → b (b waits for a, which ra reviews)"), "rb's reject resuming b");
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
                    head: None,
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

    fn dispatched(job: &str) -> Event {
        Event::Dispatch(DispatchEvent::Dispatched {
            job: job.into(),
            account: "x".into(),
            model: None,
            provider: String::new(),
            runner_pid: 9,
            runner_started: 1,
        })
    }

    fn ended_for(job: &str, outcome: Outcome, why: &str) -> Event {
        Event::Session(SessionEvent::Ended {
            job: job.into(),
            outcome,
            cost_usd: 0.0,
            turns: 0,
            reason: Some(why.into()),
            head: None,
        })
    }

    fn unheld(job: &str) -> Event {
        Event::Dispatch(DispatchEvent::Unheld {
            job: job.into(),
            message: "the disk floor paused you".into(),
            settle_us: 0,
            budget_usd: None,
        })
    }

    #[test]
    fn a_job_the_floor_stopped_is_held_and_resumed_once() {
        let mut evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
        ];
        let w = World::fold(&evs);
        let j = &w.jobs["a"];
        assert_eq!((j.status, j.held), (Status::Stopped, true));
        assert_eq!(j.reason.as_deref(), Some("disk: 56G free, under 60G"));
        assert!(w.moves_on_its_own(j), "the dispatcher resumes it: waiters keep waiting");

        evs.push(st(4, unheld("a")));
        let w = World::fold(&evs);
        let j = &w.jobs["a"];
        assert_eq!((j.status, j.held, j.unheld_at, j.queued_at), (Status::Queued, false, Some(4), 4));
        assert_eq!((j.message.as_deref(), j.reason.as_deref()), (Some("the disk floor paused you"), None));

        // A second one, once it has started again, resumes nothing: it is not held.
        evs.extend([st(5, dispatched("a")), st(6, unheld("a"))]);
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.unheld_at), (Status::Running, Some(4)));
    }

    #[test]
    fn a_retry_keeps_the_hold_marker_and_a_hand_resume_clears_it() {
        let retried = Event::Session(SessionEvent::Ended {
            job: "a".into(),
            outcome: Outcome::Retry,
            cost_usd: 0.0,
            turns: 0,
            reason: Some("the development environment changed".into()),
            head: None,
        });
        let mut evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(4, unheld("a")),
            st(5, dispatched("a")),
            st(6, retried),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.unheld_at), (Status::Queued, Some(4)), "an automatic requeue is still the resume");
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "mine".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        });
        evs.push(st(7, resumed));
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.unheld_at), (Status::Queued, None), "a person took it over");
    }

    #[test]
    fn a_stop_by_a_person_is_never_held_and_wins_over_the_floor() {
        // A plain stop by a person.
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, Event::Queue(QueueEvent::StopRequested { job: "a".into() })),
            st(4, Event::Dispatch(DispatchEvent::Reaped { job: "a".into() })),
            st(5, unheld("a")),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.held), (Status::Stopped, false), "a person's stop is not resumed by the dispatcher");

        // The floor and a person at once: the person's stop is the one that stands.
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, Event::Queue(QueueEvent::StopRequested { job: "a".into() })),
            st(4, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(5, unheld("a")),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.held), (Status::Stopped, false));

        // A person stopping a job that is already held takes it from the floor.
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(4, Event::Queue(QueueEvent::StopRequested { job: "a".into() })),
            st(5, unheld("a")),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.held, j.reason.as_deref()), (Status::Stopped, false, Some("stopped by request")));

        // A clone over its ceiling is stopped, not held.
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Stopped, "disk: clone 52G over 40G")),
            st(4, unheld("a")),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.held), (Status::Stopped, false));
    }

    #[test]
    fn a_hand_resume_of_a_held_job_leaves_nothing_for_the_dispatcher() {
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "a".into(),
            message: "mine".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        });
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(4, resumed),
            st(5, unheld("a")),
        ];
        let j = &World::fold(&evs).jobs["a"].clone();
        assert_eq!((j.status, j.held, j.unheld_at, j.message.as_deref()), (Status::Queued, false, None, Some("mine")));
    }

    #[test]
    fn a_held_job_whose_prerequisite_failed_is_blocked_and_so_are_its_dependents() {
        let evs = vec![
            st(1, queued("p")),
            st(2, dispatched("p")),
            st(3, ended("p", Outcome::Done)),
            st(4, queued_after("a", &["p"])),
            st(5, dispatched("a")),
            st(6, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(7, queued_after("b", &["a"])),
        ];
        let w = World::fold(&evs);
        assert!(w.moves_on_its_own(&w.jobs["a"]), "its prerequisite is done: the dispatcher resumes it");
        // The prerequisite is resumed by hand and fails while `a` is held.
        let resumed = Event::Queue(QueueEvent::Resumed {
            job: "p".into(),
            message: "again".into(),
            budget_usd: None,
            models: BTreeMap::new(),
        });
        let evs = [evs, vec![st(8, resumed), st(9, dispatched("p")), st(10, ended("p", Outcome::Failed))]].concat();
        let w = World::fold(&evs);
        let (a, b) = (&w.jobs["a"], &w.jobs["b"]);
        assert!(w.blocked(a) && !w.moves_on_its_own(a), "{:?}", w.waits(a));
        assert!(w.blocked(b) && !w.moves_on_its_own(b), "and what waits for it: {:?}", w.waits(b));
        assert_eq!(Wait::summary(&w.waits(a)).as_deref(), Some("waits for p (failed)"));
    }

    #[test]
    fn the_dispatcher_ends_a_hold_and_caps_a_resume() {
        let held = vec![st(1, queued("a")), st(2, dispatched("a")), st(3, ended_for("a", Outcome::Held, "disk"))];
        let capped = Event::Dispatch(DispatchEvent::Unheld {
            job: "a".into(),
            message: "m".into(),
            settle_us: 7,
            budget_usd: Some(0.25),
        });
        let w = World::fold(&[held.clone(), vec![st(4, capped)]].concat());
        let j = &w.jobs["a"];
        assert_eq!((j.status, j.budget_usd, j.settle_us), (Status::Queued, 0.25, 7));
        let ended = Event::Dispatch(DispatchEvent::HoldEnded { job: "a".into(), why: "ceiling".into() });
        let w = World::fold(&[held, vec![st(4, ended.clone()), st(5, ended)]].concat());
        let j = &w.jobs["a"];
        assert_eq!((j.status, j.held, j.reason.as_deref()), (Status::Stopped, false, Some("ceiling")));
        assert!(!w.moves_on_its_own(j), "it waits for a person now");
    }

    #[test]
    fn a_job_waiting_for_a_held_one_waits_and_is_not_blocked() {
        let evs = vec![
            st(1, queued("a")),
            st(2, dispatched("a")),
            st(3, ended_for("a", Outcome::Held, "disk: 56G free, under 60G")),
            st(4, queued_after("b", &["a"])),
        ];
        let w = World::fold(&evs);
        assert!(!w.blocked(&w.jobs["b"]), "the dispatcher will resume what it waits for");
        assert!(w.moves_on_its_own(&w.jobs["b"]));
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
                head: None,
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
    /// every query, in work linear in the jobs and their `after` lists (a step per job, at most three per
    /// edge). Bounded for a whole status pass (every query the board, dispatch and the wait make, for every
    /// job) over a long chain, a dense graph and a fan-in, counting every step of every build, not wall time.
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
            assert!(steps <= 3 * (jobs + edges), "a chain of {jobs}: {steps} steps");
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
        assert!(steps <= 3 * (jobs + edges), "a dense graph of {jobs}: {steps} steps");
        assert!(w.jobs().values().skip(1).all(|j| w.blocked(j)));
        // A change drops the analysis; the next query builds it again, once.
        let mut w = w;
        w.jobs_mut().remove(&names[0]);
        let steps = pass(&w);
        assert!(steps <= 3 * (jobs + edges), "{steps} steps after a change");
        assert_eq!(Wait::summary(&w.waits(&w.jobs()[&names[1]])).as_deref(), Some("waits for m00 (gone)"));

        // Copilot on PR #69 (world.rs:1124): one job after k unfinished ones. Grouping its waits by `--after`
        // entry must not search what it has grouped so far (k(k-1)/2 comparisons): each edge's slot is found
        // in one counted step. Exactly: one per job settled (k + 1), one per edge settled (k), one per edge
        // waited on (k), one per slot found (k).
        let k = 300;
        let names: Vec<String> = (0..k).map(|i| format!("p{i:03}")).collect();
        let mut evs: Vec<Stamped> = names.iter().enumerate().map(|(i, n)| st(1 + i as u64, queued(n))).collect();
        let after: Vec<&str> = names.iter().map(String::as_str).collect();
        evs.push(st(1_000, queued_after("sink", &after)));
        let w = World::fold(&evs);
        let steps = pass(&w);
        assert_eq!(steps, 4 * k + 1, "a fan-in of {k}: every comparison counted, one per edge");
        let waits = w.waits(&w.jobs()["sink"]);
        assert_eq!(waits.iter().map(|w| w.job.as_str()).collect::<Vec<_>>(), after, "in the order they were named");
    }

    #[test]
    fn a_reap_after_the_runner_reported_changes_nothing() {
        let done = Event::Session(SessionEvent::Ended {
            job: "a".into(),
            outcome: Outcome::Done,
            cost_usd: 0.0,
            turns: 1,
            reason: None,
            head: None,
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
            head: None,
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
            head: None,
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
