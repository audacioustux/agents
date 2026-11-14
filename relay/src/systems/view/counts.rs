//! The totals a board states over its rows: how many jobs run and wait, and why the waiting ones wait.
//!
//! They are counted from the very jobs the rows are drawn from ([`super::listed`]), each job once, by
//! the judgment its row shows ([`super::reason`]): a queued job waits on another job, or is blocked by
//! one that ended unfinished, or is held by one named cause, or is simply next in line. The numbers
//! therefore cannot disagree with the rows.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::Config;
use crate::systems::dispatch::{Cause, Room};
use crate::systems::view::machine::Project;
use crate::world::{Job, Status};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Jobs with status `running`.
    pub running: usize,
    /// Jobs stopped at their spend cap while their progress is checked.
    pub checking: usize,
    /// Jobs with status `queued`, whatever holds them.
    pub queued: usize,
    /// Of the queued: those waiting on another job that has not ended.
    pub waiting_on_job: usize,
    /// Of the queued: those whose dependency ended unfinished, so they never start on their own.
    pub blocked: usize,
    /// Of the queued: those held by one named cause (`w2-build cap`, `disk floor`, ...), each once.
    /// Only the project relay runs in can be judged, so only its jobs appear here.
    pub held: BTreeMap<String, usize>,
    /// Stopped jobs the disk floor paused; they resume by themselves.
    pub paused: usize,
    /// Failed jobs.
    pub failed: usize,
    /// Jobs only a human moves on: the failed, and the queued that are blocked.
    pub need_human: usize,
    /// `limits.max_running` of the project these are the counts of, when there is one.
    pub max_running: Option<usize>,
}

impl Counts {
    /// The counts of `jobs`, each with its project; ours are judged in `room` by `cfg`'s rules, as
    /// their rows are, and another project's are judged by what their world says alone.
    pub fn of<'a>(jobs: impl IntoIterator<Item = (&'a Project, &'a Job)>, cfg: &Config, room: &Room, now: u64) -> Self {
        let mut c = Self::default();
        for (p, j) in jobs {
            match j.status {
                Status::Running => c.running += 1,
                Status::Checking => c.checking += 1,
                Status::Failed => {
                    c.failed += 1;
                    c.need_human += 1;
                }
                Status::Stopped if j.held => c.paused += 1,
                Status::Stopped | Status::Done => {}
                Status::Queued => {
                    c.queued += 1;
                    let waits = p.world.waits(j);
                    if waits.iter().any(|w| w.blocked) {
                        c.blocked += 1;
                        c.need_human += 1;
                    } else if !waits.is_empty() {
                        c.waiting_on_job += 1;
                    } else if let Some(h) = p.own.then(|| room.hold(cfg, j, now)).flatten() {
                        *c.held.entry(name_of(h.cause, j)).or_default() += 1;
                    }
                }
            }
        }
        c
    }

    /// The cap of the project these count, for the line to state.
    #[must_use]
    pub fn capped(mut self, max_running: usize) -> Self {
        self.max_running = Some(max_running);
        self
    }

    /// The jobs held by a cause, all causes together.
    pub fn held_total(&self) -> usize {
        self.held.values().sum()
    }

    /// The line a status board states: `10 running of at most 10 · 19 queued (4 waiting on another job,
    /// 3 held by w2-build cap) · 2 need a human`.
    pub fn line(&self) -> String {
        let mut parts = vec![match self.max_running {
            Some(max) => format!("{} running of at most {max}", self.running),
            None => format!("{} running", self.running),
        }];
        if self.checking > 0 {
            parts.push(format!("{} checking", self.checking));
        }
        let mut why = Vec::new();
        if self.waiting_on_job > 0 {
            why.push(format!("{} waiting on another job", self.waiting_on_job));
        }
        why.extend(self.held.iter().map(|(by, n)| format!("{n} held by {by}")));
        parts.push(if why.is_empty() {
            format!("{} queued", self.queued)
        } else {
            format!("{} queued ({})", self.queued, why.join(", "))
        });
        if self.paused > 0 {
            parts.push(format!("{} stopped by the disk floor", self.paused));
        }
        if self.need_human > 0 {
            parts.push(format!("{} need a human", self.need_human));
        }
        parts.join(" · ")
    }

    /// The same, short enough for a title: `10/10 running, 19 queued (4 after, 3 held), 2 need a human`.
    pub fn short(&self) -> String {
        let mut parts = vec![match self.max_running {
            Some(max) => format!("{}/{max} running", self.running),
            None => format!("{} running", self.running),
        }];
        if self.checking > 0 {
            parts.push(format!("{} checking", self.checking));
        }
        let mut why = Vec::new();
        if self.waiting_on_job > 0 {
            why.push(format!("{} after", self.waiting_on_job));
        }
        if self.held_total() > 0 {
            why.push(format!("{} held", self.held_total()));
        }
        parts.push(if why.is_empty() {
            format!("{} queued", self.queued)
        } else {
            format!("{} queued ({})", self.queued, why.join(", "))
        });
        if self.paused > 0 {
            parts.push(format!("{} paused", self.paused));
        }
        if self.need_human > 0 {
            parts.push(format!("{} need a human", self.need_human));
        }
        parts.join(", ")
    }
}

/// What a held job is said to be held by.
fn name_of(cause: Cause, j: &Job) -> String {
    match cause {
        Cause::Profile => format!("{} cap", j.profile.as_deref().unwrap_or("profile")),
        Cause::Disk => "disk floor".into(),
        Cause::Setup => "dev shell".into(),
        Cause::Accounts => "account limits".into(),
    }
}
