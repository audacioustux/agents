//! The state every system works from, derived by folding the bus. Nothing
//! here is stored; it is recomputed from the events each time.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::bus::{DispatchEvent, Event, LandEvent, Meters, Outcome, QueueEvent, QuotaEvent, SessionEvent, Stamped};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Default, Clone, Serialize)]
pub struct World {
    pub jobs: BTreeMap<String, Job>,
    pub accounts: BTreeMap<String, AccountState>,
}

impl World {
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

    fn apply(&mut self, s: &Stamped) {
        let t = s.t;
        match &s.event {
            Event::Queue(QueueEvent::Queued { job, task, readonly, harnesses, models, effort, budget_usd, pinned }) => {
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
                if let Some(j) = self.jobs.get_mut(job) {
                    // A stop kills the runner, so its silent death is the stop.
                    (j.status, j.reason) = if j.stop_requested {
                        (Status::Stopped, Some("stopped by request".into()))
                    } else {
                        (Status::Failed, Some("session runner died without reporting".into()))
                    };
                    j.ended_at = Some(t);
                }
            }
            Event::Session(e) => self.apply_session(t, e),
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
        Stamped { t, event }
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
}
