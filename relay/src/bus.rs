//! The event bus. Each system owns its output port: an append-only file,
//! `bus/<system>.ndjson`, holding only that system's event type. A session
//! runner is a system of its own for one attempt at one job, so it writes
//! `bus/session/<job>.<created>.<attempt>.ndjson`, and nothing else writes there.
//!
//! Ownership is enforced three times. A [`Port`] is built only from its
//! system's [`Owned::Source`], a capability whose constructor is private to
//! that system's module, so a second writer cannot be built (`TheGrid`'s
//! linear source capability). A line in a file that does not parse as that
//! file's event type is dropped on read. And the fold applies a session
//! event only from the job's current attempt: a runner that outlived its
//! attempt writes to a file nobody listens to, which is the fence.
//!
//! The files are the lossless record. Readers learn that something changed
//! from an inotify watch on the directory, which is only a doorbell.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use crate::harness::Meters;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QueueEvent {
    Queued {
        job: String,
        task: String,
        readonly: bool,
        /// Harness ids the job may run on. Empty: any enabled harness.
        #[serde(default)]
        harnesses: Vec<String>,
        /// Model per harness id; a harness not named here uses its configured default.
        #[serde(default)]
        models: BTreeMap<String, String>,
        effort: Option<String>,
        budget_usd: f64,
        /// An account (`harness:label`) the owner chose. A pinned job never moves.
        pinned: Option<String>,
        /// A read-only job's question at a clean commit; equal keys get the same kind of answer.
        #[serde(default)]
        reuse_key: Option<String>,
        /// Jobs that must be done before this one starts.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        after: Vec<String>,
        /// The profile the job was queued under; it stays with the job across resumes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// The session the job starts as a fork of.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<Box<Fork>>,
        /// The job a decision queued this one to follow up.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// How many follow-ups deep it is: its parent's depth and one, or `0`.
        #[serde(default, skip_serializing_if = "is_zero")]
        depth: u32,
        /// How the job's result is accepted. Part of the record, so changing it means queuing a new job.
        #[serde(default, skip_serializing_if = "Acceptance::is_none")]
        acceptance: Acceptance,
    },
    /// A held job sent elsewhere by a decision: where it may run from now on.
    Rerouted {
        job: String,
        harnesses: Vec<String>,
        models: BTreeMap<String, String>,
        #[serde(default)]
        profile: Option<String>,
    },
    /// A held job let past its profile's limits by a decision.
    Forced {
        job: String,
    },
    /// The job no longer waits for these jobs (empty: for any of them).
    Released {
        job: String,
        #[serde(default)]
        from: Vec<String>,
    },
    Resumed {
        job: String,
        message: String,
        budget_usd: Option<f64>,
        /// Models that replace the job's own, by harness, from this run on.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        models: std::collections::BTreeMap<String, String>,
    },
    Dequeued {
        job: String,
    },
    /// The review `job` no longer belongs to the chain of the build job it reviews: it does not
    /// re-review it and its verdicts do not resume it. `by` is the review that replaced it; none for
    /// `queue unreview`. What it judged stays in the event log.
    Unchained {
        job: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    /// The brief the chained review `job` is judged by from its next re-review on.
    ReviewBriefed {
        job: String,
        brief: String,
    },
    StopRequested {
        job: String,
    },
}

/// Where a forked job's session comes from: another job's, as it stood when the fork was queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub job: String,
    pub session_id: String,
    /// The account (`harness:label`) that last wrote that session.
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DispatchEvent {
    Dispatched {
        job: String,
        account: String,
        /// The model the job runs on `account`, as the dispatching project's config resolved it.
        /// Every reader of the run (its session, the seats it holds, the boards) takes it from
        /// here, so another project never resolves it through a config that is not the job's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// The provider whose seat the run holds (omp's model prefix); empty for a harness whose
        /// account has one seat budget. Absent from events written before it was recorded.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        provider: String,
        runner_pid: u32,
        runner_started: u64,
    },
    /// The runner is gone without an `ended` event.
    Reaped { job: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Done,
    Failed,
    Stopped,
    /// Back in the queue, for another account or a later attempt.
    Retry,
    /// Back in the queue after a death with no output; counted against `limits.max_retries`.
    Rerun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionEvent {
    Cloned {
        job: String,
        workdir: crate::ctx::SessionRepo,
        branch: String,
        /// The commit the branch started from, for a fresh start: where a revert-control check runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// An attempt began, on this account, in this session (new or resumed).
    Started {
        job: String,
        account: String,
        session_id: String,
        /// For a read-only job, the clean commit it reads; absent when the tree is dirty.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        /// For a review, the model family it runs on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<String>,
    },
    /// The harness named the session itself (omp chooses its own ids).
    Identified {
        job: String,
        session_id: String,
    },
    Rate {
        account: String,
        #[serde(alias = "windows")]
        meters: Meters,
    },
    Limited {
        account: String,
        until: i64,
    },
    SignedOut {
        account: String,
    },
    /// The session needs one more turn on its own account, such as a missing final report.
    FollowUp {
        job: String,
        message: String,
        /// The next run's cap, when a decision set one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_usd: Option<f64>,
    },
    /// A decision sent the job back to the queue under another profile, to start afresh.
    Requeued {
        job: String,
        profile: String,
        harnesses: Vec<String>,
        models: BTreeMap<String, String>,
        effort: Option<String>,
        budget_usd: f64,
    },
    /// A run that had already ended with its final report, `status: done`, spent past its cap
    /// before the cap could stop it. Nothing more is asked of anyone: the job ends as its report says.
    Overran {
        job: String,
        spent_usd: f64,
        cap_usd: f64,
    },
    /// The run reached its cap and its progress is being checked before more is spent.
    Checking {
        job: String,
        checkpoint: u32,
    },
    /// A checkpoint's judgement, from the progress check or the zero-progress rule.
    Checked {
        job: String,
        check: Check,
    },
    /// The job's acceptance check ran in its clone at `commit`: its exit status and the
    /// tail of its output (bounded, as worker output is before it reaches a handler).
    BarChecked {
        job: String,
        commit: Option<String>,
        exit: i32,
        tail: String,
        /// The exit status at the base commit, when the job asks for a revert control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_exit: Option<i32>,
        result: BarResult,
        /// For `base-red`: the failing tests, all failing at the base commit too (empty when the
        /// output names none to compare).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failing: Vec<String>,
    },
    /// A review was carried out: `job` judged `of`'s branch at `commit`.
    Reviewed {
        job: String,
        of: String,
        commit: String,
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<Reason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<String>,
        /// The builder's head when the verdict came in, when that was not `commit`: the branch moved while the
        /// review ran, so the verdict is about a commit that is no longer the branch's tip and acts on nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stale_head: Option<String>,
    },
    Ended {
        job: String,
        outcome: Outcome,
        cost_usd: f64,
        turns: u32,
        reason: Option<String>,
    },
    /// The job's clone was measured while it ran: `bytes` allocated.
    Disk {
        job: String,
        bytes: u64,
    },
}

/// What a job is, by what it may touch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Works in a private clone on a branch of its own.
    #[default]
    Build,
    /// Reads the repository, mounted read-only.
    Read,
    /// A read-only job that judges a build job's branch ([`Acceptance::ReviewOf`]).
    Review,
}

impl Kind {
    pub fn readonly(self) -> bool {
        self != Self::Build
    }
}

/// How a job's result is accepted, declared when it is queued.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Acceptance {
    #[default]
    None,
    /// A command that must pass on the committed tip of the job's branch.
    Check(String),
    /// As `Check`, and it must also fail at the commit the branch started from.
    CheckRedFirst(String),
    /// Reviews the build job of this name.
    ReviewOf(String),
}

impl Acceptance {
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }

    /// The job's kind that goes with it: a review is a read-only job of its own kind.
    pub fn kind(&self, readonly: bool) -> Kind {
        match (self, readonly) {
            (Self::ReviewOf(_), _) => Kind::Review,
            (_, true) => Kind::Read,
            _ => Kind::Build,
        }
    }

    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Check(c) | Self::CheckRedFirst(c) => Some(c),
            _ => None,
        }
    }

    pub fn review_of(&self) -> Option<&str> {
        match self {
            Self::ReviewOf(job) => Some(job),
            _ => None,
        }
    }
}

/// A job that waits for this name is never started by a relay that does not know acceptance: it
/// shows as waiting for a job that is gone. The fold drops it, so a relay that does know ignores it.
pub const ACCEPTANCE_GATE: &str = "needs-relay-acceptance";

/// How an acceptance check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarResult {
    Pass,
    /// It ran and the code under test failed it.
    Fail,
    /// It could not say anything: it timed out, was not found or was killed.
    CheckCouldNotRun,
    /// It passed, but it also passes at the base commit, so it does not show the change did anything.
    CheckProvesNothing,
    /// It failed, and it fails at the base commit the same way: the failure is not the job's.
    BaseRed,
}

impl BarResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::CheckCouldNotRun => "check-could-not-run",
            Self::CheckProvesNothing => "check-proves-nothing",
            Self::BaseRed => "base-red",
        }
    }
}

/// A reviewer's judgement of a build job's branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Accept,
    Reject,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        }
    }
}

/// Why a reviewer rejected, as a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    TestFailure,
    BehaviorMismatch,
    InsufficientEvidence,
    StaleHead,
    Other,
}

impl Reason {
    pub const ALL: [Self; 5] =
        [Self::TestFailure, Self::BehaviorMismatch, Self::InsufficientEvidence, Self::StaleHead, Self::Other];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TestFailure => "test-failure",
            Self::BehaviorMismatch => "behavior-mismatch",
            Self::InsufficientEvidence => "insufficient-evidence",
            Self::StaleHead => "stale-head",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// What a progress check decided at a spend cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckVerdict {
    /// On track: resume with a larger cap.
    Continue,
    /// Useful but off course: resume with a larger cap and the check's correction.
    Redirect,
    /// Not worth more: wait for a human.
    Stop,
}

impl CheckVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Redirect => "redirect",
            Self::Stop => "stop",
        }
    }
}

/// One checkpoint of a job: where its branch stood, and what was decided there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Counts from 1 over the job's whole life.
    pub checkpoint: u32,
    pub verdict: CheckVerdict,
    pub why: String,
    /// What the resumed session is told; empty on a stop.
    #[serde(default)]
    pub message: String,
    /// The next run's cap, when the job goes on.
    pub next_cap_usd: Option<f64>,
    /// The branch's commit at this checkpoint.
    pub head: Option<String>,
    /// The working tree at this checkpoint, committed or not, as a git tree id.
    pub tree: Option<String>,
    /// The account that judged; absent when no model was asked.
    pub checker: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
    /// The decision this checkpoint carried out, whose `decided` events count its cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

impl Check {
    /// One line for the board: `checkpoint 2: continue → $30.00`.
    pub fn summary(&self) -> String {
        match self.next_cap_usd {
            Some(cap) => format!("checkpoint {}: {} → ${cap:.2}", self.checkpoint, self.verdict.as_str()),
            None => format!("checkpoint {}: {} — {}", self.checkpoint, self.verdict.as_str(), self.why),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QuotaEvent {
    AccountEnabled {
        account: String,
        enabled: bool,
    },
    /// A reading from the harness's quota command, which costs no model call.
    Reading {
        account: String,
        #[serde(alias = "windows")]
        meters: Meters,
        /// The home's sign-in (`Identity::stamp`) when it was read, so a new one is read again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sign_in: Option<String>,
    },
    /// The quota command's output says the session is expired, unauthorised or
    /// not signed in. A later reading that succeeds, or enabling the account, clears it.
    SignedOut {
        account: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sign_in: Option<String>,
    },
    /// The quota command gave nothing usable; the account stays unread, and
    /// is not asked again until a reading would be stale.
    Unreadable {
        account: String,
    },
}

/// A moment at which relay asks its hooks what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Point {
    /// A run reached its spend cap.
    Cap,
    /// A run ended done or failed.
    Ended,
    /// A queued job has been held by a limit for a while.
    Held,
    /// A running job's clone is over its disk limit, or the disk is nearly full.
    Disk,
}

impl Point {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cap => "cap",
            Self::Ended => "ended",
            Self::Held => "held",
            Self::Disk => "disk",
        }
    }
}

fn yes() -> bool {
    true
}

/// What a handler may answer: one of a closed set, which relay validates before acting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
    /// `cap`: go on in the same session; `budget` is the next run's cap.
    Continue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<f64>,
    },
    /// `cap`: go on, and tell the worker `message` first.
    Redirect {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<f64>,
    },
    /// `cap`, `ended`: leave the job failed, for a human. `disk`: stop the job, which can be resumed.
    Stop {
        #[serde(default)]
        why: String,
    },
    /// `ended`: the run ends as relay concluded.
    Accept,
    /// `ended`: one more run in the same session, told `message`.
    Resume {
        #[serde(default)]
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<f64>,
    },
    /// `ended`: start the job afresh under another profile.
    Requeue { profile: String },
    /// `ended`, for a review (`--review-of`): the verdict on the reviewed job's branch. A reject
    /// resumes the builder's session with `message`.
    Review {
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<Reason>,
        #[serde(default)]
        message: String,
    },
    /// `ended`: queue a new job with `brief`, by default after this one, optionally as a fork of its session.
    FollowUp {
        brief: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default = "yes")]
        after: bool,
        #[serde(default)]
        fork: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// `held`: keep waiting. `disk`: let the job go on.
    Wait,
    /// `held`: let the job run elsewhere.
    Reroute {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        harness: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    /// `held`: start it past its profile's limits, where policy allows.
    Force,
    /// No decision: the next handler decides.
    Defer,
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Continue { .. } => "continue",
            Self::Redirect { .. } => "redirect",
            Self::Stop { .. } => "stop",
            Self::Accept => "accept",
            Self::Resume { .. } => "resume",
            Self::Requeue { .. } => "requeue",
            Self::Review { .. } => "review",
            Self::FollowUp { .. } => "follow-up",
            Self::Wait => "wait",
            Self::Reroute { .. } => "reroute",
            Self::Force => "force",
            Self::Defer => "defer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DecideEvent {
    /// A decision point was reached for the job's attempt `attempt`; its handlers are asked in order.
    DecisionRequested {
        id: String,
        job: String,
        point: Point,
        /// The job's incarnation (`created_at`).
        created: u64,
        attempt: u32,
        /// The process that waits for the answer and carries it out, as (pid, start time): the
        /// job's runner, or the `decide held` process. An answer for a dead waiter is refused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiter: Option<(u32, u64)>,
    },
    /// The chain reached `orchestrator`: the request waits for `relay decide answer` until `until` (bus time).
    Parked { id: String, until: u64 },
    /// `relay decide answer`: what the orchestrator answered. It counts only if
    /// recorded before the parked request's `until`; the runner then records it
    /// as `decided`, or refuses it.
    Answered {
        id: String,
        action: Action,
        #[serde(default)]
        why: String,
    },
    /// One handler's answer. A `defer`, or an action refused by policy (`refused`), passes to the next.
    Decided {
        id: String,
        handler: String,
        action: Action,
        #[serde(default)]
        why: String,
        #[serde(default)]
        cost_usd: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum LandEvent {
    Forgotten {
        job: String,
    },
    /// A gc pass removed what a job's clone held: its build outputs (`what` is
    /// `outputs`) or the whole clone (`clone`), `bytes` in all.
    Collected {
        job: String,
        what: String,
        bytes: u64,
    },
}

/// The project's development environment (`session.env_shell`), entered once on the host for every
/// session. `key` names what it was entered for: the shell command and the files that define it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DevShellEvent {
    /// A warm-up started: `shell` is being entered in the repository.
    Warming { key: String, shell: String },
    /// It started, and its variables are recorded for the sessions.
    Warmed { key: String },
    /// It did not start: `why` (`exited 1`, `timed out after 600 s`) and the end of its stderr.
    WarmFailed {
        key: String,
        why: String,
        #[serde(default)]
        tail: String,
        /// Stopped at its time limit, not failed: the next attempt goes on at once.
        #[serde(default)]
        timed_out: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Queue(QueueEvent),
    Dispatch(DispatchEvent),
    Session(SessionEvent),
    Quota(QuotaEvent),
    Land(LandEvent),
    Decide(DecideEvent),
    DevShell(DevShellEvent),
}

/// Ties an event enum to the system that owns it and its file.
pub trait Owned: Serialize + DeserializeOwned {
    const SYSTEM: &'static str;
    /// The capability to write this port, constructible only by its system.
    type Source: Source;
    fn wrap(self) -> Event;
}

/// A port's capability names the file it writes, relative to the bus.
pub trait Source {
    fn file(&self) -> PathBuf;
}

macro_rules! owned {
    ($ty:ty, $system:literal, $variant:ident, $source:ty) => {
        impl Owned for $ty {
            const SYSTEM: &'static str = $system;
            type Source = $source;
            fn wrap(self) -> Event {
                Event::$variant(self)
            }
        }
    };
}
owned!(QueueEvent, "queue", Queue, crate::systems::queue::Source);
owned!(DispatchEvent, "dispatch", Dispatch, crate::systems::dispatch::Source);
owned!(SessionEvent, "session", Session, crate::systems::session::Source);
owned!(QuotaEvent, "quota", Quota, crate::systems::quota::Source);
owned!(LandEvent, "land", Land, crate::systems::land::Source);
owned!(DecideEvent, "decide", Decide, crate::systems::decide::Source);
owned!(DevShellEvent, "devshell", DevShell, crate::systems::devshell::Source);

/// The one file a whole-system port writes.
pub fn system_file(system: &str) -> PathBuf {
    PathBuf::from(format!("{system}.ndjson"))
}

/// Which session file an event came from: one attempt at one incarnation of
/// a job (a name cleaned and queued again is a new incarnation, told apart by
/// its `created_at`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub job: String,
    /// Absent in files named before incarnations were.
    pub created: Option<u64>,
    pub attempt: u32,
}

/// One attempt's session file. Job names have no dots, so the name parses back.
pub fn session_file(job: &str, created: u64, attempt: u32) -> PathBuf {
    PathBuf::from(format!("session/{job}.{created}.{attempt}.ndjson"))
}

fn parse_session_file(name: &str) -> Option<Origin> {
    let parts: Vec<&str> = name.strip_suffix(".ndjson")?.split('.').collect();
    let (job, created, attempt) = match parts[..] {
        [job, attempt] => (job, None, attempt),
        [job, created, attempt] => (job, Some(created.parse().ok()?), attempt),
        _ => return None,
    };
    Some(Origin { job: job.to_owned(), created, attempt: attempt.parse().ok()? })
}

impl SessionEvent {
    /// The job the event is about, when it names one.
    pub fn job(&self) -> Option<&str> {
        match self {
            Self::Cloned { job, .. }
            | Self::Started { job, .. }
            | Self::Identified { job, .. }
            | Self::FollowUp { job, .. }
            | Self::Requeued { job, .. }
            | Self::Overran { job, .. }
            | Self::Checking { job, .. }
            | Self::Checked { job, .. }
            | Self::BarChecked { job, .. }
            | Self::Reviewed { job, .. }
            | Self::Ended { job, .. }
            | Self::Disk { job, .. } => Some(job),
            Self::Rate { .. } | Self::Limited { .. } | Self::SignedOut { .. } => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Line<E> {
    /// Unix microseconds, so events from different systems merge in order.
    t: u64,
    #[serde(flatten)]
    event: E,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stamped {
    pub t: u64,
    pub event: Event,
    /// For a session event, the file it came from.
    pub from: Option<Origin>,
}

impl Stamped {
    /// The event as its bus line, with the system that owns it.
    pub fn to_json(&self) -> serde_json::Value {
        fn line<E: Owned>(t: u64, e: &E) -> serde_json::Value {
            let mut v = serde_json::to_value(e).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("t".into(), t.into());
                o.insert("system".into(), E::SYSTEM.into());
            }
            v
        }
        match &self.event {
            Event::Queue(e) => line(self.t, e),
            Event::Dispatch(e) => line(self.t, e),
            Event::Session(e) => line(self.t, e),
            Event::Quota(e) => line(self.t, e),
            Event::Land(e) => line(self.t, e),
            Event::Decide(e) => line(self.t, e),
            Event::DevShell(e) => line(self.t, e),
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

pub fn now_micros() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros()).unwrap_or(u64::MAX)
}

/// Unix seconds, from bus time (microseconds), as meters name their resets.
pub fn secs(us: u64) -> i64 {
    i64::try_from(us / 1_000_000).unwrap_or(i64::MAX)
}

/// A system's output port, built only from its [`Owned::Source`].
#[derive(Debug)]
pub struct Port<E> {
    file: PathBuf,
    _event: PhantomData<E>,
}

impl<E: Owned> Port<E> {
    pub fn new(bus: &Path, source: &E::Source) -> Self {
        Self { file: bus.join(source.file()), _event: PhantomData }
    }

    pub fn emit(&self, event: E) -> Result<()> {
        let mut line = serde_json::to_vec(&Line { t: now_micros(), event })?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.file)
            .with_context(|| format!("opening {}", self.file.display()))?;
        // One write of a short line with O_APPEND: concurrent writers never interleave.
        let written = f.write(&line)?;
        anyhow::ensure!(written == line.len(), "short write to {}", self.file.display());
        Ok(())
    }
}

/// For each bus file (relative to the bus), how many bytes of it have been read.
pub type Offsets = BTreeMap<PathBuf, Mark>;

/// How far a file has been folded, and which file that was: a file replaced
/// under the same name (a job cleaned and queued again) has a new inode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub ino: u64,
    pub len: u64,
}

/// Reads `file`'s complete lines past `offset` into `out`, and returns the
/// mark just past the last `\n`: a torn last line (a writer mid-append) is
/// left for the next read. `None` when the file is not the one read before
/// (shorter, or another inode), so what was read no longer describes it.
/// A missing file reads as empty. A line that is not `E` is skipped.
fn read_file<E: Owned>(file: &Path, from: Option<&Origin>, seen: Mark, out: &mut Vec<Stamped>) -> Result<Option<Mark>> {
    use std::os::unix::fs::MetadataExt;
    let mut f = match File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((seen.len == 0).then_some(Mark::default())),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let meta = f.metadata().with_context(|| format!("reading {}", file.display()))?;
    if seen.len > 0 && (meta.ino() != seen.ino || meta.len() < seen.len) {
        return Ok(None);
    }
    let offset = seen.len;
    let mut bytes = Vec::new();
    f.seek(SeekFrom::Start(offset))
        .and_then(|_| f.read_to_end(&mut bytes))
        .with_context(|| format!("reading {}", file.display()))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&bytes[..complete]);
    out.extend(text.lines().filter_map(|l| {
        let line = serde_json::from_str::<Line<E>>(l).ok()?;
        Some(Stamped { t: line.t, event: line.event.wrap(), from: from.cloned() })
    }));
    Ok(Some(Mark { ino: meta.ino(), len: offset + complete as u64 }))
}

/// Every event on the bus, oldest first.
pub fn read_all(bus: &Path) -> Result<Vec<Stamped>> {
    Ok(read_since(bus, &Offsets::new())?.map(|(events, _)| events).unwrap_or_default())
}

/// The events written since `offsets` were taken, oldest first, with the
/// offsets to read from next time. `None` if a file is not the one read
/// before, when only a full read from empty offsets makes sense.
pub fn read_since(bus: &Path, offsets: &Offsets) -> Result<Option<(Vec<Stamped>, Offsets)>> {
    type Read = fn(&Path, Option<&Origin>, Mark, &mut Vec<Stamped>) -> Result<Option<Mark>>;
    let systems: [(&str, Read); 7] = [
        ("queue", read_file::<QueueEvent>),
        ("dispatch", read_file::<DispatchEvent>),
        ("quota", read_file::<QuotaEvent>),
        ("land", read_file::<LandEvent>),
        ("decide", read_file::<DecideEvent>),
        ("devshell", read_file::<DevShellEvent>),
        // Before sessions had a file per attempt, they shared one; read it as is.
        ("session", read_file::<SessionEvent>),
    ];
    let mut sessions: Vec<(PathBuf, Origin)> = std::fs::read_dir(bus.join("session"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse_session_file(&name).map(|o| (Path::new("session").join(name), o))
        })
        .collect();
    // A fixed file order, so a stable sort by time breaks ties the same way in every read.
    sessions.sort_by(|a, b| a.0.cmp(&b.0));
    let files = systems
        .into_iter()
        .map(|(sys, read)| (system_file(sys), None, read))
        .chain(sessions.into_iter().map(|(rel, o)| (rel, Some(o), read_file::<SessionEvent> as Read)));
    let (mut all, mut next) = (Vec::new(), Offsets::new());
    for (rel, origin, read) in files {
        let seen = offsets.get(&rel).copied().unwrap_or_default();
        let Some(mark) = read(&bus.join(&rel), origin.as_ref(), seen, &mut all)? else { return Ok(None) };
        next.insert(rel, mark);
    }
    all.sort_by_key(|s| s.t);
    Ok(Some((all, next)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    /// Writes a line to a port's file directly: tests stand outside the systems.
    fn emit<E: Owned>(bus: &Path, file: PathBuf, e: E) {
        let p = bus.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let line = serde_json::to_string(&Line { t: now_micros(), event: e }).unwrap();
        let old = std::fs::read_to_string(&p).unwrap_or_default();
        std::fs::write(&p, format!("{old}{line}\n")).unwrap();
    }

    #[test]
    fn round_trip_and_ownership() {
        let d = Dir::new();
        emit(d.path(), system_file("queue"), QueueEvent::Dequeued { job: "a".into() });
        emit(d.path(), system_file("land"), LandEvent::Forgotten { job: "a".into() });
        // A queue event written into the land file is not land's to emit: dropped.
        std::fs::write(
            d.path().join("land.ndjson"),
            format!(
                "{}\n{{\"t\":1,\"type\":\"dequeued\",\"job\":\"x\"}}\n{{\"t\":2,\"type\":",
                std::fs::read_to_string(d.path().join("land.ndjson")).unwrap().trim()
            ),
        )
        .unwrap();
        let evs = read_all(d.path()).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0].event, Event::Queue(QueueEvent::Dequeued { .. })));
        assert!(matches!(evs[1].event, Event::Land(LandEvent::Forgotten { .. })));
    }

    #[test]
    fn session_files_say_their_job_incarnation_and_attempt() {
        let d = Dir::new();
        emit(d.path(), session_file("my-job", 17, 3), SessionEvent::SignedOut { account: "w".into() });
        emit(d.path(), PathBuf::from("session/my-job.4.ndjson"), SessionEvent::SignedOut { account: "w".into() });
        let from: Vec<_> = read_all(d.path()).unwrap().into_iter().map(|e| e.from.unwrap()).collect();
        assert_eq!(from[0], Origin { job: "my-job".into(), created: Some(17), attempt: 3 });
        assert_eq!(from[1], Origin { job: "my-job".into(), created: None, attempt: 4 }, "named before incarnations");
    }

    #[test]
    fn events_print_as_json_lines() {
        let s = Stamped { t: 5, event: Event::Land(LandEvent::Forgotten { job: "j".into() }), from: None };
        assert_eq!(s.to_json(), serde_json::json!({"t":5,"system":"land","type":"forgotten","job":"j"}));
    }

    #[test]
    fn line_format_is_flat() {
        let l = Line { t: 5, event: SessionEvent::SignedOut { account: "w".into() } };
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"{"t":5,"type":"signed-out","account":"w"}"#);
    }
}
