//! The state every system works from, derived by folding the bus. The only
//! thing stored is a checkpoint of that fold (`world.json`): a cache, so a
//! command reads only what was written since, never the source of truth.

use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{
    self, DispatchEvent, Event, LandEvent, Meters, Offsets, Outcome, QueueEvent, QuotaEvent, SessionEvent, Stamped,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Stopped,
}

impl Status {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    pub enabled: bool,
    pub signed_out: bool,
    pub meters: Meters,
    /// Unix microseconds of the last reading.
    pub read_at: Option<u64>,
    /// Unix microseconds of the last attempt that read nothing.
    pub unreadable_at: Option<u64>,
    /// Unix seconds until which the account refuses sessions.
    pub limited_until: Option<i64>,
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            enabled: true,
            signed_out: false,
            meters: Meters::new(),
            read_at: None,
            unreadable_at: None,
            limited_until: None,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub jobs: BTreeMap<String, Job>,
    pub accounts: BTreeMap<String, AccountState>,
}

/// Bump whenever the fold's meaning changes (`apply`, `current`, what `World`
/// holds, how a mark is kept), so checkpoints folded the old way are discarded.
const FOLD_VERSION: u32 = 2;

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

impl World {
    /// The fold of the bus at `bus`, from the checkpoint at `file` when it is
    /// usable, which is then brought up to date. Any process may do this: the
    /// checkpoint is a cache of a deterministic fold, replaced atomically, and
    /// rewritten only when it changed. A failure to write it costs only time.
    pub fn load(bus: &Path, file: &Path) -> Result<Self> {
        let old = std::fs::read(file).unwrap_or_default();
        let cached = serde_json::from_slice::<Checkpoint>(&old).ok().filter(|c| c.version == fold_version());
        let cp = match cached {
            Some(c) => c.advance(bus)?,
            None => None,
        };
        let cp = match cp {
            Some(c) => c,
            None => Checkpoint::full(bus)?,
        };
        if let Ok(bytes) = serde_json::to_vec(&cp)
            && bytes != old
        {
            let _ = write_atomic(file, &bytes);
        }
        Ok(cp.world)
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
        let Some((job, attempt)) = &s.from else { return true };
        e.job().is_none_or(|j| j == job) && self.jobs.get(job).is_some_and(|j| j.attempts == *attempt)
    }

    #[allow(clippy::too_many_lines)] // one arm per event
    fn apply(&mut self, s: &Stamped) {
        let t = s.t;
        match &s.event {
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
            }) => {
                self.jobs.insert(
                    job.clone(),
                    Job {
                        name: job.clone(),
                        task: task.clone(),
                        status: Status::Queued,
                        readonly: *readonly,
                        harnesses: harnesses.clone(),
                        models: models.clone(),
                        effort: effort.clone(),
                        budget_usd: *budget_usd,
                        pinned: pinned.clone(),
                        reuse_key: reuse_key.clone(),
                        account: None,
                        session_id: None,
                        transcript_account: None,
                        message: None,
                        follow_ups: 0,
                        reruns: 0,
                        attempts: 0,
                        runner: None,
                        workdir: None,
                        branch: None,
                        created_at: t,
                        started_at: None,
                        ended_at: None,
                        cost_usd: 0.0,
                        turns: 0,
                        reason: None,
                        stop_requested: false,
                    },
                );
            }
            Event::Queue(QueueEvent::Resumed { job, message, budget_usd }) => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Queued;
                    j.message = Some(message.clone());
                    j.reason = None;
                    j.stop_requested = false;
                    j.follow_ups = 0;
                    j.reruns = 0;
                    if let Some(b) = budget_usd {
                        j.budget_usd = *b;
                    }
                }
            }
            Event::Queue(QueueEvent::Dequeued { job }) => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Stopped;
                    j.reason = Some("dequeued".into());
                    j.ended_at = Some(t);
                }
            }
            Event::Queue(QueueEvent::StopRequested { job }) => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.stop_requested = true;
                }
            }
            Event::Dispatch(DispatchEvent::Dispatched { job, account, runner_pid, runner_started }) => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.status = Status::Running;
                    j.account = Some(account.clone());
                    j.runner = Some((*runner_pid, *runner_started));
                    j.started_at = Some(t);
                    j.attempts += 1;
                    j.reason = None;
                }
            }
            Event::Dispatch(DispatchEvent::Reaped { job }) => {
                // A runner can report and exit between dispatch's snapshot and its liveness check.
                if let Some(j) = self.jobs.get_mut(job).filter(|j| j.status == Status::Running) {
                    // A stop kills the runner, so its silent death is the stop.
                    (j.status, j.reason) = if j.stop_requested {
                        (Status::Stopped, Some("stopped by request".into()))
                    } else {
                        (Status::Failed, Some("session runner died without reporting".into()))
                    };
                    j.ended_at = Some(t);
                }
            }
            Event::Session(e) if self.current(s, e) => self.apply_session(t, e),
            Event::Session(_) => {}
            Event::Quota(QuotaEvent::AccountEnabled { account, enabled }) => {
                let a = self.account(account);
                a.enabled = *enabled;
                a.signed_out = false;
            }
            Event::Quota(QuotaEvent::Reading { account, meters }) => {
                let a = self.account(account);
                a.meters.clone_from(meters);
                a.read_at = Some(t);
            }
            Event::Quota(QuotaEvent::Unreadable { account }) => {
                self.account(account).unreadable_at = Some(t);
            }
            Event::Land(LandEvent::Forgotten { job }) => {
                self.jobs.remove(job);
            }
        }
    }

    fn apply_session(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::Cloned { job, workdir, branch } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.workdir = Some(workdir.clone());
                    j.branch = Some(branch.clone());
                }
            }
            SessionEvent::Started { job, account, session_id } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.transcript_account = Some(account.clone());
                    j.session_id = Some(session_id.clone());
                    j.message = None;
                }
            }
            SessionEvent::Identified { job, session_id } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.session_id = Some(session_id.clone());
                }
            }
            SessionEvent::Rate { account, meters } => {
                let a = self.account(account);
                a.meters.clone_from(meters);
                a.read_at = Some(t);
            }
            SessionEvent::Limited { account, until } => {
                let a = self.account(account);
                a.limited_until = Some(a.limited_until.map_or(*until, |u| u.max(*until)));
            }
            SessionEvent::SignedOut { account } => self.account(account).signed_out = true,
            SessionEvent::FollowUp { job, message } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.message = Some(message.clone());
                    j.follow_ups += 1;
                }
            }
            SessionEvent::Ended { job, outcome, cost_usd, turns, reason } => {
                if let Some(j) = self.jobs.get_mut(job) {
                    j.cost_usd += cost_usd;
                    j.turns += turns;
                    j.reason.clone_from(reason);
                    j.ended_at = Some(t);
                    j.status = match outcome {
                        Outcome::Done => Status::Done,
                        Outcome::Failed => Status::Failed,
                        Outcome::Stopped => Status::Stopped,
                        Outcome::Retry | Outcome::Rerun => Status::Queued,
                    };
                    if *outcome == Outcome::Rerun {
                        j.reruns += 1;
                    }
                }
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
        })
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
                Event::Session(SessionEvent::Started { job: "a".into(), account: "x".into(), session_id: "s".into() }),
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
            from: Some(("a".into(), attempt)),
        };
        // Attempt 1's runner outlived its attempt and reports after attempt 2 started: not heard.
        let w = World::fold(&[st(1, queued("a")), dispatched(2), dispatched(3), ended(4, 1)]);
        assert_eq!(w.jobs["a"].status, Status::Running);
        let w = World::fold(&[st(1, queued("a")), dispatched(2), dispatched(3), ended(4, 2)]);
        assert_eq!(w.jobs["a"].status, Status::Done);
        // A file speaks only for its own job.
        let mut other = ended(4, 2);
        other.from = Some(("b".into(), 2));
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
            // A job cleaned and queued again writes a new session/<job>.1 at least as long as the old one.
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
