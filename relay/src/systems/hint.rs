//! hint: the one place that knows which command a job in a given state accepts, and so the one place a
//! recovery hint naming a command comes from. A command checks a job with [`accepts`] before it acts, and a
//! hint ([`hint`]) names a plan only when every step of it is accepted, each on the state the step before
//! leaves: so a hint relay prints is never refused by the job it names.

use crate::reply::{Fail, Kind};
use crate::world::{Job, Status, World};

/// A command that acts on one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `queue add NAME` again: the name is free.
    Add,
    Resume,
    /// `queue release NAME --from ...` (all of its waits when empty).
    Release(Vec<String>),
    Dequeue,
    Stop,
    /// `land clean NAME`.
    Clean,
    /// `queue unreview NAME`, NAME a build with a review chain.
    Unreview,
}

/// A step on the job `job`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub job: String,
    pub step: Step,
}

impl Cmd {
    pub fn new(job: &str, step: Step) -> Self {
        Self { job: job.to_owned(), step }
    }

    /// The command line a person types.
    pub fn line(&self) -> String {
        let n = &self.job;
        match &self.step {
            Step::Add => format!("relay queue add {n} ..."),
            // `--message` is required: a hint is a command that runs as printed.
            Step::Resume => format!("relay queue resume {n} --message continue"),
            Step::Release(from) if from.is_empty() => format!("relay queue release {n}"),
            Step::Release(from) => format!("relay queue release {n} --from {}", from.join(",")),
            Step::Dequeue => format!("relay queue dequeue {n}"),
            Step::Stop => format!("relay queue stop {n}"),
            Step::Clean => format!("relay land clean {n}"),
            Step::Unreview => format!("relay queue unreview {n}"),
        }
    }
}

fn refuse(kind: Kind, msg: String) -> Fail {
    Fail { kind, msg }
}

/// Whether `cmd` takes its job as `world` has it: the check the command itself makes before it acts.
pub fn accepts(world: &World, cmd: &Cmd) -> Result<(), Fail> {
    let name = &cmd.job;
    let job = world.jobs().get(name);
    if cmd.step == Step::Add {
        // A job that ended before it ever started (dequeued) has nothing to keep: its name is free again.
        return match job {
            Some(j) if j.status.is_live() || j.attempts > 0 => {
                Err(refuse(Kind::Conflict, format!("job {name} exists")))
            }
            _ => Ok(()),
        };
    }
    let Some(j) = job else { return Err(refuse(Kind::NotFound, format!("no job named {name}"))) };
    let state = || refuse(Kind::Conflict, format!("{name} is {}", j.status.as_str()));
    match &cmd.step {
        Step::Add => Ok(()),
        Step::Resume if !j.status.is_live() || j.status == Status::Checking => Ok(()),
        Step::Release(from) => {
            // A queued job, or one the disk floor holds, waits for its prerequisites; one that ended without
            // being done waits for them again when it is resumed.
            if !(j.status == Status::Queued || j.held || matches!(j.status, Status::Failed | Status::Stopped)) {
                return Err(state());
            }
            if j.after.is_empty() {
                return Err(refuse(Kind::Conflict, format!("{name} waits for no job")));
            }
            match from.iter().find(|f| !j.after.contains(f)) {
                Some(other) => Err(refuse(
                    Kind::Usage,
                    format!("{name} does not wait for {other} (it waits for {})", j.after.join(", ")),
                )),
                None => Ok(()),
            }
        }
        Step::Dequeue if j.status == Status::Queued => Ok(()),
        Step::Stop if j.status == Status::Running || j.held => Ok(()),
        Step::Resume | Step::Dequeue | Step::Stop => Err(state()),
        Step::Clean if j.status.is_live() => {
            Err(refuse(Kind::Conflict, format!("{name} is {}; stop it first", j.status.as_str())))
        }
        Step::Clean => match world.waiters(name) {
            w if w.is_empty() => Ok(()),
            w => Err(refuse(Kind::Conflict, format!("{} wait on {name}", w.join(", ")))),
        },
        Step::Unreview if world.jobs().values().any(|r| r.review_of() == Some(name) && r.unchained.is_none()) => Ok(()),
        Step::Unreview => Err(refuse(Kind::Conflict, format!("{name} has no review chained to it"))),
    }
}

/// `world` as `cmd`, accepted, leaves it: enough of each command's effect to judge the next step of a plan.
fn after(world: &World, cmd: &Cmd) -> World {
    let mut next = world.clone();
    let jobs = next.jobs_mut();
    let name = &cmd.job;
    match &cmd.step {
        Step::Add => {}
        Step::Clean => {
            jobs.remove(name);
        }
        Step::Unreview => {
            for r in jobs.values_mut().filter(|r| r.review_of() == Some(name) && r.unchained.is_none()) {
                r.unchained = Some(crate::world::Unchained { by: None });
            }
        }
        step => {
            let Some(j) = jobs.get_mut(name) else { return next };
            match step {
                Step::Resume => (j.status, j.held) = (Status::Queued, false),
                Step::Release(from) => j.after.retain(|a| !from.is_empty() && !from.contains(a)),
                Step::Dequeue => (j.status, j.reason) = (Status::Stopped, Some("dequeued".into())),
                Step::Stop => (j.status, j.held) = (Status::Stopped, false),
                _ => {}
            }
        }
    }
    next
}

/// Whether every step of `plan` is accepted, each on the state the one before leaves.
pub fn works(world: &World, plan: &[Cmd]) -> bool {
    let mut at = world.clone();
    for cmd in plan {
        if accepts(&at, cmd).is_err() {
            return false;
        }
        at = after(&at, cmd);
    }
    true
}

/// The plans of `plans` that `world` accepts, as one hint: each plan's commands joined by ", then ", the
/// plans by " or ". `None` when no plan works: then relay names no command at all.
pub fn hint(world: &World, plans: &[Vec<Cmd>]) -> Option<String> {
    let ok: Vec<String> = plans
        .iter()
        .filter(|p| !p.is_empty() && works(world, p))
        .map(|p| p.iter().map(Cmd::line).collect::<Vec<_>>().join(", then "))
        .collect();
    (!ok.is_empty()).then(|| ok.join(" or "))
}

/// What frees the name of `job` for a new `queue add`, by its state: a queued job dequeued (it never
/// started), a live one stopped and cleaned, an ended one cleaned.
pub fn free_name(world: &World, job: &str) -> Option<String> {
    let c = |s| Cmd::new(job, s);
    hint(world, &[vec![c(Step::Dequeue)], vec![c(Step::Clean)], vec![c(Step::Stop), c(Step::Clean)]])
}

/// What lets `waiter` go on without `on`: released from the `--after` entries through which it waits on
/// `on`, or, for a review waiting on its builder, its chain detached.
pub fn release_from(world: &World, waiter: &Job, on: &str) -> Option<String> {
    let entries: Vec<String> = world.graph().deps(waiter).into_iter().filter(|d| d.on == on).map(|d| d.after).collect();
    let mut from: Vec<String> = Vec::new();
    for e in entries {
        if !from.contains(&e) {
            from.push(e);
        }
    }
    let mut plans = vec![vec![Cmd::new(&waiter.name, Step::Release(from))]];
    plans.extend(waiter.review_of().map(|b| vec![Cmd::new(b, Step::Unreview)]));
    hint(world, &plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(jobs: Vec<Job>) -> World {
        let mut w = World::default();
        w.jobs_mut().extend(jobs.into_iter().map(|j| (j.name.clone(), j)));
        w
    }

    /// Copilot on PR #69 (session.rs:1389): a hint names a command only where the job's state accepts it,
    /// each step on the state the one before leaves. For every state: what frees a name, and what starts a
    /// job without a base that is gone.
    #[test]
    fn every_hint_names_commands_the_jobs_state_accepts() {
        let job = |status, held, attempts| Job {
            name: "x".into(),
            status,
            held,
            attempts,
            after: vec!["base".into()],
            ..Job::default()
        };
        let cases = [
            ("queued", job(Status::Queued, false, 0), Some("relay queue dequeue x"), None),
            ("running", job(Status::Running, false, 1), Some("relay queue stop x, then relay land clean x"), None),
            ("checking", job(Status::Checking, false, 1), None, None),
            (
                "held",
                job(Status::Stopped, true, 1),
                Some("relay land clean x or relay queue stop x, then relay land clean x"),
                Some("relay queue release x --from base, then relay queue resume x --message continue"),
            ),
            (
                "failed",
                job(Status::Failed, false, 1),
                Some("relay land clean x"),
                Some("relay queue release x --from base, then relay queue resume x --message continue"),
            ),
            ("done", job(Status::Done, false, 1), Some("relay land clean x"), None),
        ];
        for (state, j, free, start) in cases {
            let w = world(vec![j]);
            assert_eq!(free_name(&w, "x").as_deref(), free, "{state}: freeing the name");
            let plan = vec![Cmd::new("x", Step::Release(vec!["base".into()])), Cmd::new("x", Step::Resume)];
            assert_eq!(hint(&w, &[plan]).as_deref(), start, "{state}: starting without base");
        }
    }
}
