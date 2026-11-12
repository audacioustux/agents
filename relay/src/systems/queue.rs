//! queue: takes work in and lets it be withdrawn.
//! Emits: queued, released, resumed, rerouted, forced, dequeued, stop-requested.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

mod declare;

use crate::bus::{ACCEPTANCE_GATE, Fork, QueueEvent};
use crate::config::{Profile, valid_name};
use crate::ctx::Ctx;
use crate::os;
use crate::world::{Job, Status, World};

/// The capability to write queue's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> std::path::PathBuf {
        crate::bus::system_file("queue")
    }
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Queue a task. The brief is the only context the session gets.
    Add(Box<Add>),
    /// Continue an ended or checking session with a new instruction.
    Resume {
        name: String,
        #[arg(long)]
        message: String,
        /// Spend cap for this run alone, in USD; not the job's total, which `limits.job_ceiling_usd` bounds.
        #[arg(long, value_parser = usd)]
        budget: Option<f64>,
        /// `harness=model` (repeatable), or a bare model when the job runs on one harness:
        /// replaces the job's model there from this run on.
        #[arg(long = "model")]
        models: Vec<String>,
        /// Replace the brief of the review chained to this job (`--review-of`), as the next thing its
        /// reviewer is told when it judges the branch again. A file.
        #[arg(long, value_name = "FILE")]
        review_brief: Option<PathBuf>,
    },
    /// Detach the review chained to a build job: its reviewer no longer re-reviews or resumes it.
    Unreview { name: String },
    /// Stop a queued job waiting for the jobs it was queued `--after`.
    Release {
        name: String,
        /// Drop only these (repeat, or comma-separate). Default: all of them.
        #[arg(long = "from", value_delimiter = ',')]
        from: Vec<String>,
    },
    /// Withdraw a job that has not started.
    Dequeue { name: String },
    /// End a running session.
    Stop { name: String },
}

#[derive(Debug, Args)]
pub struct Add {
    name: String,
    /// The brief, as a file.
    #[arg(long, conflicts_with = "text", required_unless_present = "text")]
    prompt: Option<PathBuf>,
    /// The brief, inline.
    #[arg(long)]
    text: Option<String>,
    /// A short title for the board. Defaults to the brief's first line.
    #[arg(long)]
    task: Option<String>,
    /// Fill what this command line leaves unset from `[profiles.NAME]` (see: relay profiles). Flags win.
    #[arg(long)]
    profile: Option<String>,
    /// Mount the repository read-only and make no clone.
    #[arg(long)]
    readonly: bool,
    /// Make a build job even when the profile says read-only.
    #[arg(long, conflicts_with = "readonly")]
    no_readonly: bool,
    /// Run only on these harnesses (repeat, or comma-separate). Default: any enabled harness.
    #[arg(long = "harness", value_delimiter = ',')]
    harnesses: Vec<String>,
    /// Run only on this account (`harness:label`), and wait for it.
    #[arg(long)]
    account: Option<String>,
    /// `harness=model` (repeatable), or a bare model when exactly one harness applies.
    #[arg(long = "model")]
    models: Vec<String>,
    #[arg(long)]
    effort: Option<String>,
    /// Spend cap for each attempt, in USD.
    #[arg(long, value_parser = usd)]
    budget: Option<f64>,
    /// Queue a read-only job even when the same question was already answered at this commit.
    #[arg(long)]
    again: bool,
    /// Start only once these jobs are done (repeat, or comma-separate). Archived jobs count; landing is a later step.
    #[arg(long = "after", value_delimiter = ',')]
    after: Vec<String>,
    /// Start as a fork of this job's session (`--resume ID --fork-session`), on a harness that can
    /// fork; a build job also starts from its branch. Never fork a builder for its review.
    #[arg(long)]
    fork: Option<String>,
    #[command(flatten)]
    declare: declare::Declare,
}

fn usd(s: &str) -> Result<f64, String> {
    s.parse().ok().filter(|x| crate::config::positive(*x)).ok_or_else(|| format!("{s:?} is not a positive amount"))
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    // From the check to the record, so two adds of one name cannot both pass.
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    let name = match &cmd {
        Cmd::Add(add) => add.name.clone(),
        Cmd::Resume { name, .. }
        | Cmd::Unreview { name }
        | Cmd::Release { name, .. }
        | Cmd::Dequeue { name }
        | Cmd::Stop { name } => name.clone(),
    };
    let job = world.jobs().get(&name);
    // The job, when its status is one the command applies to.
    let when = |ok: fn(Status) -> bool| match job {
        None => fail!(NotFound, "no job named {name}"),
        Some(j) if !ok(j.status) => fail!(Conflict, "{name} is {}", j.status.as_str()),
        Some(j) => anyhow::Ok(j),
    };
    // What is recorded, in order, files the brief changes, and for a stop the runner to signal once it is recorded.
    let mut events = Vec::new();
    let mut briefs: Vec<(String, String)> = Vec::new();
    let (event, runner) = match cmd {
        Cmd::Add(add) => {
            // A job that ended before it ever started (dequeued) has nothing to keep:
            // its name is free again.
            if job.is_some_and(|j| j.status.is_live() || j.attempts > 0) {
                fail!(Conflict, "job {name} exists; pick another name, or: relay land clean {name}");
            }
            let replaced = add.declare.review_of().map(|b| chain(world.jobs(), b)).unwrap_or_default();
            let queued = add.queued(ctx, world.jobs())?;
            // One live chain per job: the new review takes the old one's place, in the same breath.
            events.extend(
                replaced
                    .into_iter()
                    .filter(|r| *r != name)
                    .map(|job| QueueEvent::Unchained { job, by: Some(name.clone()) }),
            );
            (queued, None)
        }
        Cmd::Unreview { .. } => {
            let j = when(|_| true)?;
            let mut chained = chain(world.jobs(), &j.name);
            let Some(last) = chained.pop() else { fail!(Conflict, "{name} has no review chained to it") };
            events.extend(chained.into_iter().map(|job| QueueEvent::Unchained { job, by: None }));
            (QueueEvent::Unchained { job: last, by: None }, None)
        }
        Cmd::Resume { message, budget, models, review_brief, .. } => {
            let j = when(|s| !s.is_live() || s == Status::Checking)?;
            let models = parse_models(ctx, &j.harnesses, &models)?;
            // The model it will run on, checked now: a banned one would only fail at dispatch.
            let mut next = j.models.clone();
            next.extend(models.clone());
            if let Some((h, m, reason)) = next.iter().find_map(|(h, m)| ctx.cfg.session.banned(m).map(|r| (h, m, r))) {
                fail!(Refused, "model {m} is banned: {reason}; resume with --model {h}=<model>");
            }
            if let Some(path) = review_brief {
                let brief = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                if brief.trim().is_empty() {
                    fail!(Usage, "the review brief is empty");
                }
                let chained = chain(world.jobs(), &name);
                if chained.is_empty() {
                    fail!(
                        Conflict,
                        "--review-brief: {name} has no review chained to it (queue add --review-of {name})"
                    );
                }
                for reviewer in chained {
                    events.push(QueueEvent::ReviewBriefed { job: reviewer.clone(), brief: brief.clone() });
                    briefs.push((reviewer, brief.clone()));
                }
            }
            (QueueEvent::Resumed { job: name.clone(), message, budget_usd: budget, models }, None)
        }
        Cmd::Release { from, .. } => {
            let waiting = &when(|s| s == Status::Queued)?.after;
            if waiting.is_empty() {
                fail!(Conflict, "{name} waits for no job");
            }
            if let Some(other) = from.iter().find(|f| !waiting.contains(f)) {
                fail!(Usage, "{name} does not wait for {other} (it waits for {})", waiting.join(", "));
            }
            (QueueEvent::Released { job: name.clone(), from }, None)
        }
        Cmd::Dequeue { .. } => {
            when(|s| s == Status::Queued)?;
            (QueueEvent::Dequeued { job: name.clone() }, None)
        }
        Cmd::Stop { .. } => (QueueEvent::StopRequested { job: name.clone() }, stop_target(job, &name)?),
    };
    let recorded = serde_json::to_value(&event)?["type"].clone();
    // A reviewer started afresh reads the new brief as its own.
    for (reviewer, brief) in &briefs {
        Ctx::write_private(&ctx.job_file(reviewer, "prompt.md"), brief.as_bytes())?;
    }
    let port = ctx.port(&Source(()));
    for before in events {
        port.emit(before)?;
    }
    port.emit(event)?;
    if let Some(runner) = runner {
        stop_runner(runner)?;
    }
    ctx.wake_dispatch()?;
    ctx.reply(&serde_json::json!({ "job": name, "recorded": recorded }), &format!("{name}: recorded"))
}

/// The runner a stop of `job` signals: a running job's. A job the disk floor holds has none, and is stopped
/// by the record alone (it then stays stopped); any other job is not running, so there is nothing to stop.
fn stop_target(job: Option<&Job>, name: &str) -> Result<Option<(u32, u64)>> {
    match job {
        None => fail!(NotFound, "no job named {name}"),
        Some(j) if j.status == Status::Running => Ok(j.runner),
        Some(j) if j.held => Ok(None),
        Some(j) => fail!(Conflict, "{name} is {}", j.status.as_str()),
    }
}

/// Ends a running job's runner with a TERM, sent only while the recorded pid is still the recorded
/// process (`os::signal_if_same`), never to a group id that may have been reused. The runner passes
/// the TERM on to its own group (an acceptance check it runs) and ends, and every session and handler
/// it started ends with it, by its supervisor (`supervised`).
fn stop_runner((pid, started): (u32, u64)) -> Result<()> {
    os::signal_if_same(pid, started, rustix::process::Signal::TERM)?;
    Ok(())
}

/// The reviews chained to the build job `builder`: the ones that have not been replaced or detached.
fn chain(jobs: &BTreeMap<String, Job>, builder: &str) -> Vec<String> {
    jobs.values().filter(|r| r.review_of() == Some(builder) && r.unchained.is_none()).map(|r| r.name.clone()).collect()
}

impl Add {
    /// Checks the request, stores the brief, and says what to record.
    fn queued(self, ctx: &Ctx, jobs: &BTreeMap<String, Job>) -> Result<QueueEvent> {
        if !valid_name(&self.name) || self.name.starts_with(RESERVED) {
            fail!(Usage, "job names are lowercase letters, digits and dashes, and do not start with {RESERVED}");
        }
        let profile = self.profile.as_deref().map(|p| ctx.cfg.profile(p)).transpose()?;
        let fill = Filled::new(profile, &self);
        let declared = self.declare.resolve(ctx, profile, fill.readonly, jobs)?;
        let name = self.name;
        let brief = match (self.prompt, self.text) {
            (Some(p), _) => std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
            (None, Some(t)) => t,
            (None, None) => unreachable!("clap requires one"),
        };
        if brief.trim().is_empty() {
            fail!(Usage, "the brief is empty");
        }
        let mut after = dependencies(&name, self.after, jobs)?;
        if let Some(builder) = declared.review.filter(|b| !after.contains(&b.name)) {
            after.push(builder.name.clone());
        }
        // A relay that cannot judge the job must not run it: it waits for a job that is gone.
        if !declared.acceptance.is_none() {
            after.push(ACCEPTANCE_GATE.to_owned());
        }
        let fork = self.fork.as_deref().map(|f| fork_of(f, jobs)).transpose()?;
        if let Some(parent) = self.fork.as_deref().and_then(|f| jobs.get(f)).filter(|p| fill.readonly && !p.readonly())
        {
            fail!(
                Refused,
                "--fork {}: a read-only job may not fork a build job; a review stays independent",
                parent.name
            );
        }
        let mut harnesses = allowed_harnesses(ctx, fill.harnesses.clone(), self.account.as_deref())?;
        if let Some((_, h)) = &fork {
            if !harnesses.is_empty() && !harnesses.contains(h) {
                fail!(Usage, "--fork: the session to fork is on {h}, which this job may not use");
            }
            harnesses = vec![h.clone()];
        }
        let models = parse_models(ctx, &harnesses, &fill.models(&harnesses))?;
        if let Some((m, reason)) = models.values().find_map(|m| ctx.cfg.session.banned(m).map(|r| (m, r))) {
            fail!(Refused, "model {m} is banned: {reason}");
        }
        if let Some(builder) = declared.review {
            declare::refuse_same_family(ctx, builder, &harnesses, &models)?;
        }
        // A review reads a branch that can move under the same brief, so the question is never the same twice.
        let commit = if fill.readonly && declared.review.is_none() { Ctx::clean_head(&ctx.cfg.repo) } else { None };
        let reuse_key = commit.as_deref().map(|c| reuse_key(&brief, &harnesses, &models, fill.effort.as_deref(), c));
        if let (Some(key), Some(commit), false) = (&reuse_key, &commit, self.again) {
            refuse_repeat(jobs, key, commit, |job| {
                std::fs::metadata(ctx.job_file(job, "result.md")).is_ok_and(|m| m.len() > 0)
            })?;
        }
        // The brief reaches the session on stdin or as a file, never in argv (/proc/<pid>/cmdline is public).
        // A name whose job was dequeued starts clean: its files are the old job's, and the logs append.
        ctx.wipe_job_files(&name)?;
        Ctx::write_private(&ctx.job_file(&name, "prompt.md"), brief.as_bytes())?;
        let task =
            self.task.unwrap_or_else(|| brief.trim().lines().next().unwrap_or_default().chars().take(80).collect());
        Ok(QueueEvent::Queued {
            job: name,
            task,
            readonly: fill.readonly,
            harnesses,
            models,
            effort: fill.effort,
            budget_usd: fill.budget.unwrap_or(ctx.cfg.limits.budget_usd),
            pinned: self.account,
            reuse_key,
            after,
            profile: self.profile,
            fork: fork.map(|(f, _)| Box::new(f)),
            parent: None,
            depth: 0,
            acceptance: declared.acceptance,
        })
    }
}

/// The session `name` has to fork, and its harness, which must be able to fork.
fn fork_of(name: &str, jobs: &BTreeMap<String, Job>) -> Result<(Fork, String)> {
    let Some(j) = jobs.get(name) else { fail!(NotFound, "--fork {name}: no job named {name}") };
    let (Some(session_id), Some(account)) = (&j.session_id, &j.transcript_account) else {
        fail!(Conflict, "--fork {name}: {name} has no session yet");
    };
    let harness = account.split(':').next().unwrap_or_default().to_owned();
    if !crate::harness::by_id(&harness).is_ok_and(|h| h.caps().fork) {
        fail!(Refused, "--fork {name}: {harness} cannot fork a session");
    }
    Ok((Fork { job: name.to_owned(), session_id: session_id.clone(), account: account.clone() }, harness))
}

/// A job a decision queues after another.
#[derive(Debug, Clone)]
pub struct FollowUp {
    /// Default: `<parent>-next`, numbered when taken.
    pub name: Option<String>,
    pub brief: String,
    /// Default: the parent's.
    pub profile: Option<String>,
    /// Wait for the parent to be done.
    pub after: bool,
    /// Start as a fork of the parent's session.
    pub fork: bool,
}

/// Queues `f` as `queue add` would, and returns its name. Only a decision queues a follow-up, so this
/// runs inside `Decided::carry`: `world` was read under the jobs lock carry holds, after it checked that
/// `parent` is still at the attempt the decision was about.
pub fn follow_up(ctx: &Ctx, world: &World, parent: &Job, f: FollowUp) -> Result<String> {
    let base = f.name.unwrap_or_else(|| format!("{}-next", parent.name));
    let name = (1..1000)
        .map(|i| if i == 1 { base.clone() } else { format!("{base}-{i}") })
        .find(|n| !world.jobs().contains_key(n))
        .unwrap_or(base);
    let add = Add {
        name: name.clone(),
        prompt: None,
        text: Some(f.brief),
        task: None,
        profile: f.profile.or_else(|| parent.profile.clone()),
        readonly: false,
        no_readonly: false,
        harnesses: Vec::new(),
        account: None,
        models: Vec::new(),
        effort: None,
        budget: None,
        again: true,
        after: if f.after { vec![parent.name.clone()] } else { Vec::new() },
        fork: f.fork.then(|| parent.name.clone()),
        declare: declare::Declare::default(),
    };
    let mut event = add.queued(ctx, world.jobs())?;
    if let QueueEvent::Queued { parent: of, depth, .. } = &mut event {
        (*of, *depth) = (Some(parent.name.clone()), parent.depth + 1);
    }
    ctx.port(&Source(())).emit(event)?;
    ctx.wake_dispatch()?;
    Ok(name)
}

/// Resumes the build job `builder` with a reviewer's findings, as `queue resume` would: its session goes on
/// on any account with room. Not while it works, not once `hooks.max_decisions` reviews have rejected it,
/// and not on a banned model. `None` when resumed, else why not.
/// Only a review's decision does this, so it runs inside `Decided::carry`, which holds the jobs lock.
pub fn resume_reviewed(ctx: &Ctx, builder: &str, message: String) -> Result<Option<String>> {
    let world = ctx.world()?;
    let Some(j) = world.jobs().get(builder) else { return Ok(Some(format!("{builder} is gone"))) };
    // This reject is already among the verdicts: `hooks.max_decisions` of them resume the builder.
    let rejected =
        j.verdicts.iter().filter(|v| v.verdict == crate::bus::Verdict::Reject && v.stale_head.is_none()).count();
    resume_by_review(ctx, j, rejected, message)
}

/// Resumes the reviewer `reviewer` of a branch whose builder has answered its reject, so its verdict is
/// for the new tip. Held to the same bounds as the builder's resume: at most `hooks.max_decisions` rejects
/// by this reviewer already, a reviewer that can no longer have a decision act on it, the job's ceiling,
/// and a banned model. `None` when resumed, else why not.
pub fn resume_reviewer(ctx: &Ctx, reviewer: &str, builder: &str, message: String) -> Result<Option<String>> {
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    let Some(j) = world.jobs().get(reviewer) else { return Ok(Some(format!("{reviewer} is gone"))) };
    if let Some(u) = &j.unchained {
        return Ok(Some(format!("{reviewer} is no longer {builder}'s reviewer ({})", u.summary())));
    }
    // A review of a branch that is still being worked on would judge a head that moves under it.
    if world.jobs().get(builder).is_some_and(|b| b.status.is_live()) {
        return Ok(Some(format!("{builder} is at work again; its end asks for the review")));
    }
    // A run that could not record its verdict (the cap on decisions per job) would leave the stale one.
    if !crate::systems::decide::can_decide(&ctx.cfg, &world, j) {
        return Ok(Some(format!(
            "{reviewer} has had {} decisions already (hooks.max_decisions)",
            ctx.cfg.hooks.max_decisions
        )));
    }
    let rejected = world.jobs().get(builder).map_or(0, |b| {
        b.verdicts
            .iter()
            .filter(|v| v.reviewer == reviewer && v.verdict == crate::bus::Verdict::Reject && v.stale_head.is_none())
            .count()
    });
    resume_by_review(ctx, j, rejected, message)
}

/// The shared bounds of a resume that a review causes: `j` is not live, `rejected` is within
/// `hooks.max_decisions`, another run fits its ceiling, and its model is not banned.
fn resume_by_review(ctx: &Ctx, j: &crate::world::Job, rejected: usize, message: String) -> Result<Option<String>> {
    let builder = j.name.as_str();
    if j.status.is_live() {
        return Ok(Some(format!("{builder} is {}", j.status.as_str())));
    }
    let max = usize::try_from(ctx.cfg.hooks.max_decisions).unwrap_or(usize::MAX);
    if rejected > max {
        return Ok(Some(format!("{max} reviews have resumed it already (hooks.max_decisions)")));
    }
    if let Some(ceiling) = ctx.cfg.limits.job_ceiling_usd.filter(|c| j.cost_usd + j.budget_usd > *c) {
        return Ok(Some(format!("another ${:.2} would pass the job's ceiling of ${ceiling:.2}", j.budget_usd)));
    }
    if let Some((_, m, why)) = j.models.iter().find_map(|(h, m)| ctx.cfg.session.banned(m).map(|r| (h, m, r))) {
        return Ok(Some(format!("model {m} is banned: {why}")));
    }
    ctx.port(&Source(())).emit(QueueEvent::Resumed {
        job: builder.to_owned(),
        message,
        budget_usd: None,
        models: BTreeMap::new(),
    })?;
    ctx.wake_dispatch()?;
    Ok(None)
}

/// Sends a queued job to another harness, model or profile, as a `held` decision said.
pub fn reroute(ctx: &Ctx, job: &Job, harness: Option<&str>, model: Option<&str>, profile: Option<&str>) -> Result<()> {
    let p = profile.map(|p| ctx.cfg.profile(p)).transpose()?;
    let harness = harness.map(str::to_owned).or_else(|| p.and_then(|p| p.harness.clone()));
    let harnesses = harness.map_or_else(|| job.harnesses.clone(), |h| vec![h]);
    let mut models: BTreeMap<String, String> =
        job.models.clone().into_iter().filter(|(h, _)| harnesses.is_empty() || harnesses.contains(h)).collect();
    if let Some(m) = model.map(str::to_owned).or_else(|| p.and_then(|p| p.model.clone())) {
        models.extend(parse_models(ctx, &harnesses, &[m])?);
    }
    let profile = profile.map(str::to_owned).or_else(|| job.profile.clone());
    ctx.port(&Source(())).emit(QueueEvent::Rerouted { job: job.name.clone(), harnesses, models, profile })
}

/// Lets a queued job past its profile's limits, as a `held` decision said.
pub fn force(ctx: &Ctx, job: &str) -> Result<()> {
    ctx.port(&Source(())).emit(QueueEvent::Forced { job: job.to_owned() })
}

/// The command line's settings with a profile's under them: each field the line sets
/// itself stays, the rest is the profile's.
struct Filled {
    harnesses: Vec<String>,
    /// The `--model`s as given.
    given: Vec<String>,
    /// The profile's `(harness, model)`, used when the line gives no model.
    profile_model: Option<(Option<String>, String)>,
    effort: Option<String>,
    budget: Option<f64>,
    readonly: bool,
}

impl Filled {
    fn new(profile: Option<&Profile>, add: &Add) -> Self {
        let mut f = Self {
            harnesses: add.harnesses.clone(),
            given: add.models.clone(),
            profile_model: None,
            effort: add.effort.clone(),
            budget: add.budget,
            readonly: add.readonly || add.declare.is_review(),
        };
        let Some(p) = profile else { return f };
        // The harness is the profile's unless the line names harnesses or pins an account.
        if let (Some(h), true) = (&p.harness, add.harnesses.is_empty() && add.account.is_none()) {
            f.harnesses = vec![h.clone()];
        }
        f.profile_model = p.model.clone().map(|m| (p.harness.clone(), m));
        f.effort = f.effort.or_else(|| p.effort.clone());
        f.budget = f.budget.or(p.budget);
        f.readonly = f.readonly || (!add.no_readonly && p.readonly == Some(true));
        f
    }

    /// The `--model` values for a job allowed on `harnesses`. The profile's model is for
    /// its harness, so it is dropped when the line sent the job elsewhere.
    fn models(&self, harnesses: &[String]) -> Vec<String> {
        match &self.profile_model {
            Some((h, m)) if self.given.is_empty() && h.as_ref().is_none_or(|h| harnesses.contains(h)) => {
                vec![h.as_ref().map_or_else(|| m.clone(), |h| format!("{h}={m}"))]
            }
            _ => self.given.clone(),
        }
    }
}

/// Job names that start so are relay's own (see `bus::ACCEPTANCE_GATE`).
const RESERVED: &str = "needs-relay-";

/// The jobs to wait for, each once. Only existing jobs can be named, so no cycle can form;
/// a job naming itself is refused all the same.
fn dependencies(name: &str, asked: Vec<String>, jobs: &BTreeMap<String, Job>) -> Result<Vec<String>> {
    let mut after = Vec::new();
    for a in asked {
        if a == name {
            fail!(Usage, "{name} cannot wait for itself");
        }
        if !jobs.contains_key(&a) {
            fail!(NotFound, "--after {a}: no job named {a}");
        }
        if !after.contains(&a) {
            after.push(a);
        }
    }
    Ok(after)
}

/// The harnesses a job may use: those asked for (all must be enabled), or the
/// pinned account's, or none (meaning any enabled harness).
fn allowed_harnesses(ctx: &Ctx, asked: Vec<String>, account: Option<&str>) -> Result<Vec<String>> {
    let enabled: Vec<&str> = ctx.cfg.harnesses().iter().map(|h| h.id()).collect();
    let missing = ctx.cfg.missing_harnesses();
    for h in &asked {
        if let Some((_, lacks)) = missing.iter().find(|(id, _)| id == h) {
            fail!(Refused, "{}", lacks.refusal(h, ctx.cfg.programs.as_deref()));
        }
        if !enabled.contains(&h.as_str()) {
            fail!(Refused, "harness {h} is not enabled here (enabled: {})", enabled.join(", "));
        }
    }
    let Some(key) = account else { return Ok(asked) };
    let Some(home) = crate::accounts::discover(&ctx.cfg).into_iter().find(|a| a.key() == key) else {
        fail!(NotFound, "unknown account {key}; accounts are harness:label, see: relay quota show");
    };
    if !asked.is_empty() && !asked.iter().any(|h| h == home.harness) {
        fail!(Usage, "account {key} is not on any of the harnesses asked for");
    }
    Ok(vec![home.harness.to_owned()])
}

/// `harness=model` pairs, or one bare model for the single harness that applies.
fn parse_models(ctx: &Ctx, harnesses: &[String], given: &[String]) -> Result<BTreeMap<String, String>> {
    let applicable: Vec<String> = if harnesses.is_empty() {
        ctx.cfg.harnesses().iter().map(|h| h.id().to_owned()).collect()
    } else {
        harnesses.to_vec()
    };
    let mut out = BTreeMap::new();
    for m in given {
        let (h, model) = match m.split_once('=') {
            Some((h, model)) => (h.to_owned(), model.to_owned()),
            None if applicable.len() == 1 => (applicable[0].clone(), m.clone()),
            None => fail!(
                Usage,
                "--model {m}: say which harness it is for (harness=model); this job may run on {}",
                applicable.join(", ")
            ),
        };
        if !applicable.contains(&h) {
            fail!(Usage, "--model {m}: harness {h} is not one this job may run on");
        }
        out.insert(h, model);
    }
    Ok(out)
}

/// What names a read-only question: the brief, where and how it may run, and
/// the clean commit it reads (a dirty tree gets no key: no commit names it).
fn reuse_key(
    brief: &str,
    harnesses: &[String],
    models: &BTreeMap<String, String>,
    effort: Option<&str>,
    commit: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut harnesses = harnesses.to_vec();
    harnesses.sort();
    harnesses.dedup();
    // A JSON array is an unambiguous encoding; the leading 1 versions it.
    let fields = serde_json::json!([1, brief, harnesses, models, effort, commit]).to_string();
    Sha256::digest(fields).iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().concat()
}

/// Refuses a question another job has answered at `commit`, or is answering.
/// An answer counts only if its session read that commit: the job may have
/// waited for quota while the tree moved on.
fn refuse_repeat(
    jobs: &BTreeMap<String, Job>,
    key: &str,
    commit: &str,
    has_result: impl Fn(&str) -> bool,
) -> Result<()> {
    let same = || jobs.values().filter(|j| j.reuse_key.as_deref() == Some(key));
    if let Some(j) = same().find(|j| j.status.is_live()) {
        fail!(Conflict, "already being answered by {} ({}); to ask again anyway: --again", j.name, j.status.as_str());
    }
    if let Some(j) = same()
        .filter(|j| j.status == Status::Done && j.ran_at.as_deref() == Some(commit) && has_result(&j.name))
        .max_by_key(|j| j.created_at)
    {
        fail!(
            Conflict,
            "{} already answered this at this commit; read it with: relay view result {}; to ask again anyway: --again",
            j.name,
            j.name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Event, Stamped};
    use crate::world::World;

    fn key(brief: &str, models: &[(&str, &str)], commit: &str) -> String {
        let models = models.iter().map(|(h, m)| ((*h).to_owned(), (*m).to_owned())).collect();
        reuse_key(brief, &["claude".to_owned()], &models, Some("high"), commit)
    }

    #[test]
    fn key_names_the_question() {
        let base = key("review x", &[("claude", "opus")], "abc");
        assert_eq!(base, key("review x", &[("claude", "opus")], "abc"));
        assert_ne!(base, key("review y", &[("claude", "opus")], "abc"));
        assert_ne!(base, key("review x", &[("claude", "sonnet")], "abc"));
        assert_ne!(base, key("review x", &[("claude", "opus")], "abd"));
        assert_ne!(base, reuse_key("review x", &[], &BTreeMap::new(), Some("high"), "abc"));
        assert_ne!(
            base,
            reuse_key(
                "review x",
                &["claude".to_owned()],
                &[("claude".to_owned(), "opus".to_owned())].into(),
                None,
                "abc"
            )
        );
    }

    #[test]
    fn key_is_stable() {
        // Pinned, so a change to the encoding (or the hash) is a deliberate one.
        assert_eq!(
            reuse_key("b", &[], &BTreeMap::new(), None, "c"),
            "64fbefd3a17fb06003b60075168536c4b6fc46e497de0bfb58c1582c4e4ce878"
        );
        let a = reuse_key("b", &["x".into(), "y".into()], &BTreeMap::new(), None, "c");
        let b = reuse_key("b", &["y".into(), "x".into()], &BTreeMap::new(), None, "c");
        assert_eq!(a, b);
    }

    fn jobs(entries: &[(&str, Option<&str>, Option<crate::bus::Outcome>)]) -> BTreeMap<String, Job> {
        let mut events = Vec::new();
        for (t, (name, key, outcome)) in (1..).zip(entries) {
            events.push(Stamped {
                t,
                from: None,
                event: Event::Queue(QueueEvent::Queued {
                    job: (*name).into(),
                    task: "t".into(),
                    readonly: true,
                    harnesses: Vec::new(),
                    models: BTreeMap::new(),
                    effort: None,
                    budget_usd: 1.0,
                    pinned: None,
                    reuse_key: key.map(Into::into),
                    after: Vec::new(),
                    profile: None,
                    fork: None,
                    parent: None,
                    depth: 0,
                    acceptance: crate::bus::Acceptance::None,
                }),
            });
            if let Some(outcome) = outcome {
                events.push(Stamped {
                    t,
                    from: None,
                    event: Event::Session(crate::bus::SessionEvent::Started {
                        job: (*name).into(),
                        account: "claude:a".into(),
                        session_id: "s".into(),
                        commit: Some("c".into()),
                        family: None,
                    }),
                });
                events.push(Stamped {
                    t,
                    from: None,
                    event: Event::Session(crate::bus::SessionEvent::Ended {
                        job: (*name).into(),
                        outcome: *outcome,
                        cost_usd: 0.1,
                        turns: 1,
                        reason: None,
                    }),
                });
            }
        }
        World::fold(&events).jobs().clone()
    }

    fn refusal(r: Result<()>) -> String {
        let e = r.unwrap_err();
        assert_eq!(e.downcast_ref::<crate::reply::Fail>().map(|f| f.kind), Some(crate::reply::Kind::Conflict));
        e.to_string()
    }

    #[test]
    fn a_done_answer_is_not_bought_twice() {
        use crate::bus::Outcome;
        let js = jobs(&[("old", Some("k"), Some(Outcome::Done)), ("other", Some("j"), Some(Outcome::Done))]);
        let msg = refusal(refuse_repeat(&js, "k", "c", |_| true));
        assert!(msg.contains("relay view result old"), "{msg}");
        // No result file, a failed run, another key, or an answer read at another commit: ask.
        refuse_repeat(&js, "k", "c", |_| false).unwrap();
        refuse_repeat(&jobs(&[("old", Some("k"), Some(Outcome::Failed))]), "k", "c", |_| true).unwrap();
        refuse_repeat(&js, "z", "c", |_| true).unwrap();
        refuse_repeat(&jobs(&[("old", None, Some(Outcome::Done))]), "k", "c", |_| true).unwrap();
        refuse_repeat(&js, "k", "d", |_| true).unwrap();
    }

    #[test]
    fn a_question_being_answered_is_not_asked_again() {
        let msg = refusal(refuse_repeat(&jobs(&[("busy", Some("k"), None)]), "k", "c", |_| false));
        assert!(msg.contains("already being answered by busy"), "{msg}");
    }
}
