//! The totals a board states over its rows: how many jobs run and wait, and why the waiting ones wait.
//!
//! They are counted from the very jobs the rows are drawn from ([`super::listed`]), each job once, by
//! the judgment its row shows ([`super::reason`]): a queued job waits on another job, or is blocked by
//! one that ended unfinished, or is held by one named cause, or is simply next in line. The numbers
//! therefore cannot disagree with the rows.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::Config;
use crate::systems::dispatch::{Constraint, Pool, Room};
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
    /// Jobs only a human moves on: the failed, and those blocked behind a dependency that ended
    /// unfinished, queued or held by the disk floor (the latter are also in `paused`).
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
            let (bucket, human) = classify(p, j, cfg, room, now);
            c.need_human += usize::from(human);
            match bucket {
                Bucket::Running => c.running += 1,
                Bucket::Checking => c.checking += 1,
                Bucket::Failed => c.failed += 1,
                Bucket::Paused => c.paused += 1,
                Bucket::Ended => {}
                Bucket::Blocked => {
                    c.queued += 1;
                    c.blocked += 1;
                }
                Bucket::WaitingOnJob => {
                    c.queued += 1;
                    c.waiting_on_job += 1;
                }
                Bucket::Held(by) => {
                    c.queued += 1;
                    *c.held.entry(by).or_default() += 1;
                }
                Bucket::Next => c.queued += 1,
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

/// What a held job is said to be held by: the constraint that binds it ([`crate::systems::dispatch::Hold::by`]),
/// the one concept dispatch names a hold by, so every cap, gate and pool has a bucket of its own.
fn name_of(by: &Constraint, j: &Job) -> String {
    let profile = j.profile.as_deref().unwrap_or("profile");
    match by {
        Constraint::Pool(Pool::Running) => "project cap".into(),
        Constraint::Pool(Pool::Profile(name)) => format!("{name} cap"),
        Constraint::Pool(Pool::Compiling) => "machine compiling cap".into(),
        Constraint::Pool(Pool::Seat(..)) => "account seats".into(),
        Constraint::PerDay => format!("{profile} daily cap"),
        Constraint::Load => "load gate".into(),
        Constraint::Memory => "memory gate".into(),
        Constraint::Uncounted => "unread project".into(),
        Constraint::Disk => "disk floor".into(),
        Constraint::DevShell => "dev shell".into(),
        Constraint::Accounts => "account limits".into(),
        Constraint::Reading => "account reading".into(),
    }
}

/// The one count a job belongs to.
#[derive(Debug, PartialEq, Eq)]
enum Bucket {
    Running,
    Checking,
    Failed,
    /// Stopped by the disk floor, to resume by itself.
    Paused,
    /// Done, or stopped by a person: counted nowhere.
    Ended,
    /// Queued, behind a dependency that ended unfinished.
    Blocked,
    /// Queued, behind a dependency still live.
    WaitingOnJob,
    /// Queued, held by this named cause.
    Held(String),
    /// Queued and next in line.
    Next,
}

/// A job's one bucket and whether only a human moves it on, decided together here and nowhere else, so
/// no path can count a job's need of a human differently from the bucket it is in. A human is needed
/// when the job failed or can never start on its own ([`crate::world::World::blocked`]): a queued job
/// and a disk-held stopped one alike, whatever bucket the latter is counted in.
fn classify(p: &Project, j: &Job, cfg: &Config, room: &Room, now: u64) -> (Bucket, bool) {
    let human = j.status == Status::Failed || p.world.blocked(j);
    let bucket = match j.status {
        Status::Running => Bucket::Running,
        Status::Checking => Bucket::Checking,
        Status::Failed => Bucket::Failed,
        Status::Stopped if j.held => Bucket::Paused,
        Status::Stopped | Status::Done => Bucket::Ended,
        Status::Queued if human => Bucket::Blocked,
        Status::Queued if !p.world.waits(j).is_empty() => Bucket::WaitingOnJob,
        Status::Queued => {
            p.own.then(|| room.hold(cfg, j, now)).flatten().map_or(Bucket::Next, |h| Bucket::Held(name_of(&h.by, j)))
        }
    };
    (bucket, human)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;
    use std::collections::BTreeMap;

    /// A job the disk floor stopped, whose prerequisite then failed, can never be resumed by the
    /// dispatcher: it needs a human, whichever bucket it is counted in.
    #[test]
    fn a_disk_held_job_behind_a_failed_one_needs_a_human() {
        let dir = crate::config::tempdir::Dir::new();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"demo\"\n", dir.path()).unwrap();
        let job = |name: &str, status, after: &[&str], held| Job {
            name: name.into(),
            status,
            held,
            after: after.iter().map(|a| (*a).to_owned()).collect(),
            ..Job::default()
        };
        let mut world = World::default();
        for j in [
            job("base", Status::Failed, &[], false),
            job("paused-ok", Status::Stopped, &[], true),
            job("paused-blocked", Status::Stopped, &["base"], true),
            job("stopped-by-hand", Status::Stopped, &["base"], false),
            job("queued-blocked", Status::Queued, &["base"], false),
        ] {
            world.jobs_mut().insert(j.name.clone(), j);
        }
        let p = Project { name: "demo".into(), jobs: dir.path().into(), world, own: true };
        let jobs: Vec<_> = p.world.jobs().values().map(|j| (&p, j)).collect();
        let c = Counts::of(jobs, &cfg, &Room::default(), 0);
        // One count each: the failed base, the two paused, the blocked queued one.
        assert_eq!((c.failed, c.paused, c.queued, c.blocked), (1, 2, 1, 1), "{c:?}");
        assert_eq!(c.need_human, 3, "base, paused-blocked, queued-blocked: {c:?}");
        assert!(c.line().contains("3 need a human"), "{}", c.line());
    }

    fn project(dir: &crate::config::tempdir::Dir, jobs: &[Job]) -> Project {
        let mut world = World::default();
        for j in jobs {
            world.jobs_mut().insert(j.name.clone(), j.clone());
        }
        Project { name: "demo".into(), jobs: dir.path().into(), world, own: true }
    }

    /// The machine's and the project's caps are holds like any: each queued job is counted once, under
    /// the constraint dispatch says binds it, and the names come from that one concept.
    #[test]
    fn caps_and_gates_are_named_holds() {
        use crate::config::Machine;
        let dir = crate::config::tempdir::Dir::new();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"demo\"\n[limits]\nmax_running = 1\n", dir.path()).unwrap();
        let job = |name: &str, status| Job { name: name.into(), status, ..Job::default() };
        let p = project(&dir, &[job("run", Status::Running), job("wait", Status::Queued)]);
        let counts = |room: &Room| Counts::of(p.world.jobs().values().map(|j| (&p, j)), &cfg, room, 0);
        let room = Room::of(Vec::new(), BTreeMap::new(), &p.world, &[], true);
        let c = counts(&room);
        assert_eq!((c.running, c.queued, c.held.clone()), (1, 1, BTreeMap::from([("project cap".to_owned(), 1)])));
        // Below the project's cap, the machine's compiling pool (here with no unit at all) is next.
        let roomy = Config::parse("name = \"demo\"\n", dir.path()).unwrap();
        let machine = Machine { max_compiling: Some(0), ..Machine::default() };
        // It is asked last, so an account with room is there for the job to start on but for the pool.
        let now = crate::bus::now_micros();
        let mut p = p;
        p.world
            .accounts
            .insert("claude:a".into(), crate::world::AccountState { read_at: Some(now - 1), ..Default::default() });
        let home = crate::harness::Home { harness: "claude", label: "a".into(), dir: "/x".into() };
        let room = Room::of(vec![home], p.world.accounts.clone(), &p.world, &[], true).with_machine(machine);
        let c = Counts::of(p.world.jobs().values().map(|j| (&p, j)), &roomy, &room, now);
        assert_eq!(c.held, BTreeMap::from([("machine compiling cap".to_owned(), 1)]), "{c:?}");
        assert_eq!(c.held_total(), c.queued, "held once, under one name");
        // The gates that read the machine's state have names of their own.
        let named = |by| name_of(&by, &job("x", Status::Queued));
        assert_eq!(named(Constraint::Memory), "memory gate");
        assert_eq!(named(Constraint::Load), "load gate");
        assert_eq!(named(Constraint::Uncounted), "unread project");
    }
}
