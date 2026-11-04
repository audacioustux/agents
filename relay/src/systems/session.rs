//! session: runs one dispatched job on its account's harness and reports
//! what happened. Emits: cloned, started, identified, rate, limited,
//! signed-out, follow-up, requeued, overran, checking, checked, bar-checked, reviewed, ended. One process per running job, started by
//! dispatch. Nothing here names a harness: the account's harness builds the
//! command and reads the output, and the limits are enforced here for all.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{Action, Check, CheckVerdict, Outcome, Point, SessionEvent, now_micros, secs};
use crate::config::HarnessConf;
use crate::ctx::Ctx;
use crate::harness::report::{Ending, Report, Verdict, verdict};
use crate::harness::{Harness, Home, Invocation, Permissions, Reader, Session, Task};
use crate::os;
use crate::sandbox::{self, Mounts};
use crate::systems::checkpoint::{self, Next};
use crate::systems::decide::{self, Request, Reviewing};
use crate::systems::dispatch;
use crate::world::{Job, World};

mod bar;
mod end;
mod review;

use end::{at_end, final_status_done};

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run a dispatched job. Dispatch starts this; it is not for direct use.
    Run { name: String },
}

/// The capability to write one attempt's session port; only this module can make one.
#[derive(Debug)]
pub struct Source {
    job: String,
    created: u64,
    attempt: u32,
}

impl crate::bus::Source for Source {
    fn file(&self) -> PathBuf {
        crate::bus::session_file(&self.job, self.created, self.attempt)
    }
}

/// This attempt's port: the runner is the only writer of its attempt's file.
fn port(ctx: &Ctx, job: &Job) -> crate::bus::Port<SessionEvent> {
    ctx.port(&Source { job: job.name.clone(), created: job.created_at, attempt: job.attempts })
}

pub fn run(ctx: &Ctx, Cmd::Run { name }: Cmd) -> Result<()> {
    Ctx::wait_for_gate();
    let job = ctx.world()?.jobs.remove(&name).with_context(|| format!("no job named {name}"))?;
    let port = port(ctx, &job);
    let mut spend = (0.0, 0);
    let (outcome, reason) =
        attempt(ctx, &job, &mut spend).unwrap_or_else(|e| (Outcome::Failed, Some(format!("{e:#}"))));
    port.emit(SessionEvent::Ended { job: name.clone(), outcome, cost_usd: spend.0, turns: spend.1, reason })?;
    // A reviewed job that answered a reject is judged again, on its new tip.
    if outcome == Outcome::Done {
        let _ = review::rereview(ctx, &name);
        // And a review that was still ending when the builder it rejected finished.
        let _ = review::rereview_if_overtaken(ctx, &name);
    }
    ctx.wake_dispatch()
}

type Ended = (Outcome, Option<String>);

fn workspace_note(ctx: &Ctx, job: &Job) -> String {
    let place = if job.readonly() {
        "The repository is mounted read-only. Report; do not try to change files.".to_owned()
    } else {
        let branch = job.branch.clone().unwrap_or_else(|| format!("{}{}", ctx.cfg.session.branch_prefix, job.name));
        format!(
            "You are on branch {branch} in a private clone. Commit your work there with explicit pathspecs. Never push."
        )
    };
    match ctx.cfg.session.env_shell.join(" ") {
        shell if shell.is_empty() => place,
        shell => format!(
            "{place} The project's development environment (`{shell}`) is already loaded: run its tools \
             directly, never through `{shell}`, which cannot run here."
        ),
    }
}

/// The project's development environment, entered on the host in `dir`
/// through `shell` (`session.env_shell`), minus names `deny` matches.
fn dev_env(shell: &[String], deny: &regex::Regex, dir: &Path) -> Result<Vec<(OsString, Option<OsString>)>> {
    use std::os::unix::ffi::OsStrExt;
    // A marker first, so whatever the environment's own hooks print is not taken for variables.
    const MARK: &str = "\0relay-env\0";
    let Some((program, prefix)) = shell.split_first() else { return Ok(Vec::new()) };
    let out = Command::new("timeout")
        .arg("600")
        .arg(program)
        .args(prefix)
        .args(["sh", "-c", &format!("printf '{}'; env -0", MARK.replace('\0', "\\0"))])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("entering the development environment ({program})"))?;
    let at = out.stdout.windows(MARK.len()).position(|w| w == MARK.as_bytes());
    let Some(at) = at.filter(|_| out.status.success()) else {
        bail!("the development environment ({}) did not start", shell.join(" "));
    };
    Ok(out.stdout[at + MARK.len()..]
        .split(|&b| b == 0)
        .filter_map(|kv| {
            let eq = kv.iter().position(|&b| b == b'=')?;
            let (k, v) = (std::ffi::OsStr::from_bytes(&kv[..eq]), std::ffi::OsStr::from_bytes(&kv[eq + 1..]));
            (!deny.is_match(&k.to_string_lossy())).then(|| (k.to_owned(), Some(v.to_owned())))
        })
        .collect())
}

/// The job's environment, in layers: the project's development environment
/// (`session.env_shell`) underneath, `session.env` over it, and the harness's
/// own variables (the account) over both.
fn layer_env(ctx: &Ctx, inv: &mut Invocation, cwd: &Path) -> Result<()> {
    let deny = regex::Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    let dev = dev_env(&ctx.cfg.session.env_shell, &deny, cwd)?;
    let session_env = ctx.cfg.session.env.iter().map(|(k, v)| (OsString::from(k), Some(OsString::from(v))));
    inv.env = dev.into_iter().chain(session_env).chain(std::mem::take(&mut inv.env)).collect();
    Ok(())
}

/// Where an attempt runs: the account, its harness, and the model the job uses on it.
struct Seat<'a> {
    key: String,
    home: &'a Home,
    harness: &'static dyn Harness,
    conf: HarnessConf,
    model: Option<String>,
}

/// The job's workspace: its clone (a build job), where the session stands, its scratch directory,
/// and its `/tmp` (on disk).
struct Workspace {
    clone: Option<PathBuf>,
    cwd: PathBuf,
    scratch: PathBuf,
    tmp: PathBuf,
}

/// A session ready to run.
struct Launch {
    cmd: Command,
    stdin: Option<String>,
    session_id: String,
    brief: String,
    reviewing: Option<Reviewing>,
}

/// Either the next step's input, or how the attempt ends here.
type Step<T> = ControlFlow<Ended, T>;

/// Unwraps a [`Step`], ending the attempt as it says.
macro_rules! go {
    ($step:expr) => {
        match $step {
            ControlFlow::Continue(v) => v,
            ControlFlow::Break(ended) => return Ok(ended),
        }
    };
}

/// One attempt: take the seat and the workspace, launch the session, drive it, and judge what it did.
fn attempt(ctx: &Ctx, job: &Job, spend: &mut (f64, u32)) -> Result<Ended> {
    let found = accounts::discover(&ctx.cfg);
    let seat = go!(seat(ctx, job, &found));
    let work = go!(workspace(ctx, job)?);
    if let Some(failed) = forkable(job, &found, seat.home, &work.cwd) {
        return Ok(failed);
    }
    let mut launch = go!(launch(ctx, job, &seat, &work, &found)?);
    // An earlier attempt's answer must not pass for this one's.
    let _ = std::fs::remove_file(ctx.job_file(&job.name, "result.md"));
    let run = Run {
        session_id: Some(&launch.session_id),
        budget_usd: job.budget_usd,
        log: "log.ndjson",
        stderr: "stderr.log",
        measure: work.clone.iter().chain([&work.tmp]).map(PathBuf::as_path).collect(),
    };
    let (report, ending, disk) =
        drive(ctx, job, &seat.key, &run, &mut launch.cmd, launch.stdin.as_deref(), seat.harness.reader())?;
    if !report.answer.is_empty() {
        std::fs::write(ctx.job_file(&job.name, "result.md"), format!("{}\n", report.answer))?;
    }
    *spend = (report.cost_usd, report.turns);
    if disk.is_some() {
        // Stopped, not failed: the clone and the session are kept for a resume after cleanup.
        return Ok((Outcome::Stopped, disk));
    }
    let ran = Ran {
        report: &report,
        dir: work.clone.as_deref(),
        scratch: &work.scratch,
        brief: &launch.brief,
        reviewing: launch.reviewing.as_ref(),
        model: seat.model.as_deref(),
    };
    let marker = &ctx.cfg.session.final_marker;
    match verdict(&report, ending, marker, job.readonly(), now_s()) {
        // A cap authorizes more spend; a run that has said `status: done` needs none. It ends as its
        // report says, the overrun recorded, and the acceptance check and the `ended` chain judge it as usual.
        Verdict::Budget if report.ended && final_status_done(&report.answer, marker) => {
            let (spent_usd, cap_usd) = (job.cost_usd + report.cost_usd, job.budget_usd);
            port(ctx, job).emit(SessionEvent::Overran { job: job.name.clone(), spent_usd, cap_usd })?;
            let (outcome, _) = conclude(ctx, job, &seat.key, Verdict::Done, now_s())?;
            let note = format!("finished ${spent_usd:.2} against a ${cap_usd:.2} budget");
            at_end(ctx, job, seat.home, &ran, (outcome, Some(note)), ending.silent)
        }
        Verdict::Budget => at_cap(ctx, job, &ran),
        v => {
            let ended = conclude(ctx, job, &seat.key, v, now_s())?;
            at_end(ctx, job, seat.home, &ran, ended, ending.silent)
        }
    }
}

/// The account the job was dispatched to, and the model it runs there.
fn seat<'a>(ctx: &Ctx, job: &Job, found: &'a [Home]) -> Step<Seat<'a>> {
    let Some(key) = job.account.clone() else {
        return ControlFlow::Break((Outcome::Failed, Some("dispatched without an account".into())));
    };
    let Some(home) = found.iter().find(|h| h.key() == key) else {
        return ControlFlow::Break((Outcome::Retry, Some(format!("account {key} is no longer available"))));
    };
    let harness = harness_of(home);
    let conf = ctx.cfg.harness(harness.id());
    let model = job.models.get(harness.id()).cloned().or_else(|| conf.model.clone());
    if let Some(reason) = model.as_deref().and_then(|m| ctx.cfg.session.banned(m)) {
        return ControlFlow::Break((Outcome::Failed, Some(format!("model banned: {reason}"))));
    }
    ControlFlow::Continue(Seat { key, home, harness, conf, model })
}

/// A build job's clone, made on its first run; a read-only job stands in the repository.
fn workspace(ctx: &Ctx, job: &Job) -> Result<Step<Workspace>> {
    let mut clone_dir = job.workdir.clone();
    if !job.readonly() && clone_dir.is_none() {
        let bases = bases(&ctx.world()?, job);
        let (dir, branch, base) = match clone(ctx, &job.name, &bases, job.session_id.is_some()) {
            Ok(made) => made,
            Err(e) => return Ok(ControlFlow::Break((Outcome::Failed, Some(format!("{e:#}"))))),
        };
        port(ctx, job).emit(SessionEvent::Cloned { job: job.name.clone(), workdir: dir.clone(), branch, base })?;
        clone_dir = Some(dir);
    }
    let cwd = if job.readonly() { ctx.cfg.repo.clone() } else { clone_dir.clone().unwrap_or_default() };
    let scratch = ctx.job_scratch(&job.name);
    std::fs::create_dir_all(&scratch)?;
    let tmp = ctx.job_tmp(&job.name);
    std::fs::create_dir_all(&tmp)?;
    Ok(ControlFlow::Continue(Workspace { clone: clone_dir, cwd, scratch, tmp }))
}

/// Chooses the session (new, resumed or forked), records that it started, and builds its command.
fn launch(ctx: &Ctx, job: &Job, seat: &Seat<'_>, work: &Workspace, found: &[Home]) -> Result<Step<Launch>> {
    let fork = job.fork.as_ref().filter(|f| job.session_id.as_deref().is_none_or(|s| s == f.session_id));
    let resumed = if fork.is_some() { None } else { resumable(job, found, seat.home, &work.scratch) };
    let session_id =
        fork.map(|f| f.session_id.clone()).or(resumed.clone()).unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let brief = std::fs::read_to_string(ctx.job_file(&job.name, "prompt.md")).context("the job's brief is gone")?;
    // A review reads a branch that lives in the builder's clone: bring it to the repository first.
    let reviewing = match review::setup(ctx, job, seat.model.as_deref()) {
        Ok(r) => r,
        Err(why) => return Ok(ControlFlow::Break((Outcome::Failed, Some(why)))),
    };
    let preamble = reviewing.as_ref().map(review::preamble);
    // A fork has its parent's context but not its own task: it is told its brief.
    let prompt = opening(&brief, job.message.as_deref(), resumed.is_some(), preamble.as_deref());
    // What a read-only job reads is the live repository, so its commit is taken now, not when queued.
    let commit = if job.readonly() { Ctx::clean_head(&ctx.cfg.repo) } else { None };
    port(ctx, job).emit(SessionEvent::Started {
        job: job.name.clone(),
        account: seat.key.clone(),
        session_id: session_id.clone(),
        commit,
        family: job.review_of().and(seat.model.as_deref()).map(crate::config::model_family),
    })?;
    let session = match (fork, &resumed) {
        (Some(f), _) => Session::Fork(&f.session_id),
        (None, Some(_)) => Session::Resume(&session_id),
        (None, None) => Session::New(&session_id),
    };
    let mut inv = match invocation(ctx, job, seat, work, &prompt, session) {
        Ok(i) => i,
        Err(refusal) => return Ok(ControlFlow::Break((Outcome::Failed, Some(refusal)))),
    };
    layer_env(ctx, &mut inv, &work.cwd)?;
    inv.write_files().context("writing the prompt file")?;
    let mounts = Mounts {
        home: seat.home,
        workdir: work.clone.as_deref(),
        scratch: &work.scratch,
        tmp: &work.tmp,
        cwd: &work.cwd,
        account: true,
    };
    let cmd = command(ctx, &inv, &mounts);
    Ok(ControlFlow::Continue(Launch { cmd, stdin: inv.stdin, session_id, brief, reviewing }))
}

/// The harness's command line for the session, or why it refuses.
fn invocation(
    ctx: &Ctx,
    job: &Job,
    seat: &Seat<'_>,
    work: &Workspace,
    prompt: &str,
    session: Session<'_>,
) -> Result<Invocation, String> {
    let briefing = ctx
        .cfg
        .session
        .briefing
        .replace("{workspace}", &workspace_note(ctx, job))
        .replace("{branch}", job.branch.as_deref().unwrap_or(""));
    let name = format!("relay-{}", job.name);
    let task = Task {
        prompt,
        model: seat.model.as_deref(),
        effort: job.effort.as_deref().or(seat.conf.effort.as_deref()),
        session,
        permissions: permissions(ctx.cfg.sandbox.enabled, job.readonly()),
        briefing: Some(&briefing),
        budget_usd: Some(job.budget_usd),
        name: Some(&name),
        events: true,
        lean: true,
        scratch: &work.scratch,
    };
    let mut inv = seat.conf.locate(seat.harness.invocation(seat.home, &task).map_err(|r| r.0)?);
    inv.args.extend(seat.conf.extra_args.iter().map(OsString::from));
    Ok(inv)
}

/// Whether the session this fork asks for can be carried to `home`: the failure otherwise.
fn forkable(job: &Job, found: &[Home], home: &Home, cwd: &Path) -> Option<Ended> {
    // A fork that never named a session of its own forks again.
    let f = job.fork.as_ref().filter(|f| job.session_id.as_deref().is_none_or(|s| s == f.session_id))?;
    let from = found.iter().find(|h| h.key() == f.account);
    let carried =
        from.is_some_and(|h| h.harness == home.harness) && harness_of(home).carry_fork(&f.session_id, from, home, cwd);
    (!carried).then(|| (Outcome::Failed, Some(format!("{}'s session {} cannot be forked here", f.job, f.session_id))))
}

/// What a finished run left, for the decision that follows it.
struct Ran<'a> {
    report: &'a Report,
    dir: Option<&'a Path>,
    scratch: &'a Path,
    brief: &'a str,
    /// For a review: the branch it read.
    reviewing: Option<&'a Reviewing>,
    /// The model the run used.
    model: Option<&'a str>,
}

/// The request at `point`, with the brief, the earlier decisions and what was spent.
fn request(ctx: &Ctx, job: &Job, run: &Ran<'_>, point: Point) -> Result<(Request, World)> {
    let world = ctx.world()?;
    let mut req = Request::new(job, point);
    run.brief.clone_into(&mut req.brief);
    req.report.clone_from(&run.report.answer);
    req.spent_usd = job.cost_usd + run.report.cost_usd;
    req.cap_usd = job.budget_usd;
    req.next_cap_usd = job.budget_usd;
    req.ceiling_usd = ctx.cfg.limits.job_ceiling_usd;
    req.earlier = decide::earlier(&world, job);
    Ok((req, world))
}

/// A run at its cap: the owner decides, or the `cap` chain does, and the job
/// goes on with a larger cap. No change since the last checkpoint is a stop
/// that asks no model, by the `rule` handler.
fn at_cap(ctx: &Ctx, job: &Job, run: &Ran<'_>) -> Result<Ended> {
    let hooked = ctx.cfg.hooks.cap.is_some();
    let spent = job.cost_usd + run.report.cost_usd;
    let cap = match checkpoint::next(&ctx.cfg.limits, job.readonly(), hooked, job.budget_usd, spent) {
        Next::Manual => {
            return Ok((Outcome::Failed, Some("reached its spend cap; resume with --budget to go on".into())));
        }
        Next::Ceiling { cap, ceiling } => {
            return Ok((
                Outcome::Failed,
                Some(format!(
                    "reached its spend cap; another ${cap:.2} would pass the job's ceiling of ${ceiling:.2}; \
                     resume with --budget to go on"
                )),
            ));
        }
        Next::Check { cap } => cap,
    };
    let dir = run.dir.context("a build job without its clone")?;
    let port = port(ctx, job);
    let n = u32::try_from(job.checkpoints.len()).unwrap_or(u32::MAX).saturating_add(1);
    let from = checkpoint::base(dir, &job.checkpoints);
    let now = checkpoint::point(dir, &run.scratch.join("checkpoint.index"));
    let progressed = checkpoint::progressed(&from, &now);
    let (mut req, _) = request(ctx, job, run, Point::Cap)?;
    req.next_cap_usd = cap;
    req.situation = format!("The run reached its spend cap of ${:.2} (checkpoint {n}).", job.budget_usd);
    req.earlier = job.checkpoints.iter().map(|c| format!("{}: {}", c.summary(), c.why)).collect();
    if progressed {
        port.emit(SessionEvent::Checking { job: job.name.clone(), checkpoint: n })?;
        req.evidence = checkpoint::evidence(dir, &from, &now);
    }
    let rule = || {
        (!progressed).then(|| {
            (Action::Stop { why: String::new() }, "no new commit and no changed file since the last checkpoint".into())
        })
    };
    let handlers = decide::chain(&ctx.cfg, Point::Cap).unwrap_or_default();
    let d = decide::decide(ctx, job, &req, &handlers, &rule, Action::Stop { why: String::new() })?;
    let (verdict, next, message) = match d.action.clone() {
        Action::Continue { budget } => (CheckVerdict::Continue, Some(budget.unwrap_or(cap)), String::new()),
        Action::Redirect { message, budget } => (CheckVerdict::Redirect, Some(budget.unwrap_or(cap)), message),
        _ => (CheckVerdict::Stop, None, String::new()),
    };
    let checker = match d.handler.as_deref() {
        Some("rule") | None => d.account.clone(),
        Some(h) => d.account.clone().or_else(|| Some(h.to_owned())),
    };
    let check = Check {
        checkpoint: n,
        verdict,
        why: match (&d.action, d.why.is_empty()) {
            (Action::Stop { why }, true) if !why.is_empty() => why.clone(),
            _ => d.why.clone(),
        },
        message: next.map(|cap| checkpoint::resume_message(n, cap, verdict, &message)).unwrap_or_default(),
        next_cap_usd: next,
        head: now.head.clone(),
        tree: now.tree.clone(),
        checker,
        cost_usd: d.cost_usd,
        decision: Some(d.id),
    };
    let stopped = if check.why.is_empty() { String::new() } else { format!(" ({})", check.why) };
    port.emit(SessionEvent::Checked { job: job.name.clone(), check })?;
    Ok(if next.is_none() {
        (Outcome::Failed, Some(format!("waits for a human at its checkpoint{stopped}; resume with --budget to go on")))
    } else {
        (Outcome::Retry, None)
    })
}

/// A read-only session for a decision on `agent`'s harness: the account it
/// ran on, what it cost, and its answer or why there is none.
/// With `patient` it first waits for a seat, up to `hooks.seat_wait_minutes`.
pub fn consult(
    ctx: &Ctx,
    job: &Job,
    agent: &decide::Agent,
    prompt: &str,
    patient: bool,
) -> (Option<String>, f64, Result<String, String>) {
    let wait = if patient { ctx.cfg.hooks.seat_wait_minutes } else { 0.0 };
    let home = match await_seat(ctx, job, agent, wait) {
        Ok(home) => home,
        Err(why) => return (None, 0.0, Err(why)),
    };
    let run = || -> Result<(Report, Ending)> {
        let harness = harness_of(&home);
        let conf = ctx.cfg.harness(harness.id());
        let scratch = ctx.job_scratch(&job.name).join("check");
        std::fs::create_dir_all(&scratch)?;
        let id = uuid::Uuid::now_v7().to_string();
        let name = format!("relay-{}-check", job.name);
        let task = Task {
            prompt,
            model: agent.model.as_deref().or(conf.model.as_deref()),
            effort: None,
            session: Session::New(&id),
            permissions: permissions(ctx.cfg.sandbox.enabled, true),
            briefing: Some("The repository is mounted read-only. Report; do not try to change files."),
            budget_usd: Some(agent.budget_usd),
            name: Some(&name),
            events: true,
            lean: true,
            scratch: &scratch,
        };
        let mut inv = conf.locate(harness.invocation(&home, &task).map_err(|r| anyhow::anyhow!(r.0))?);
        inv.args.extend(conf.extra_args.iter().map(OsString::from));
        inv.write_files().context("writing the prompt file")?;
        let tmp = ctx.job_tmp(&job.name).join("check");
        std::fs::create_dir_all(&tmp)?;
        let mounts =
            Mounts { home: &home, workdir: None, scratch: &scratch, tmp: &tmp, cwd: &ctx.cfg.repo, account: true };
        let mut cmd = command(ctx, &inv, &mounts);
        let run = Run {
            session_id: None,
            budget_usd: agent.budget_usd,
            log: "check.ndjson",
            stderr: "check-stderr.log",
            measure: Vec::new(),
        };
        drive(ctx, job, &home.key(), &run, &mut cmd, inv.stdin.as_deref(), harness.reader())
            .map(|(report, ending, _)| (report, ending))
    };
    let (report, ending) = match run() {
        Ok(r) => r,
        Err(e) => return (Some(home.key()), 0.0, Err(format!("agent:{} did not run: {e:#}", agent.label))),
    };
    let answer = match verdict(&report, ending, "", true, now_s()) {
        Verdict::Done => Ok(report.answer.clone()),
        v => Err(format!("agent:{} did not finish: {v:?}", agent.label)),
    };
    (Some(home.key()), report.cost_usd, answer)
}

/// How often a wait for a seat looks again.
const SEAT_POLL: Duration = Duration::from_secs(2);
/// The least between two quota readings a wait for a seat asks for.
const SEAT_REREAD: Duration = Duration::from_secs(30);

/// A seat for `agent`'s session: waits up to `minutes` for one to free, asking for a fresh quota
/// reading of any account whose reading is old (dispatch only takes readings for queued jobs, and a
/// checkpoint is not one). Nothing else is held meanwhile but the worker's own seat.
fn await_seat(ctx: &Ctx, job: &Job, agent: &decide::Agent, minutes: f64) -> Result<Home, String> {
    let wait = Duration::from_secs_f64(minutes.max(0.0) * 60.0);
    let started = Instant::now();
    let mut asked: Option<Instant> = None;
    loop {
        let (home, stale) = checker_home(ctx, job, &agent.harness, agent.model.as_deref());
        if let Some(home) = home {
            return Ok(home);
        }
        if stale && asked.is_none_or(|t| t.elapsed() >= SEAT_REREAD) {
            asked = Some(Instant::now());
            // `quota refresh` holds its own role, so overlapping requests start one reading.
            let _ = ctx.spawn_self(&["quota", "refresh", "--stale"], &ctx.state.join("quota.log"));
        }
        let left = wait.saturating_sub(started.elapsed());
        if left.is_zero() {
            let (harness, label) = (&agent.harness, &agent.label);
            return Err(if wait.is_zero() {
                format!("no {harness} account is free for agent:{label}")
            } else {
                format!("no {harness} account became free for agent:{label} within hooks.seat_wait_minutes = {minutes}")
            });
        }
        std::thread::sleep(left.min(SEAT_POLL));
    }
}

/// An account of `harness` that may take a session for `model`: another
/// than the worker's and idle where there is one, so no worker grades itself.
/// Whether it is idle is the dispatcher's count: the live sessions of every
/// project against `limits.per_account`, homes of one sign-in sharing a seat budget.
/// Also whether some account of the harness has a quota reading that is old, so none can be trusted.
fn checker_home(ctx: &Ctx, job: &Job, harness: &str, model: Option<&str>) -> (Option<Home>, bool) {
    let model = model.map(str::to_owned).or_else(|| ctx.cfg.harness(harness).model);
    let Ok(world) = ctx.world() else { return (None, false) };
    let now = now_micros();
    let found = accounts::discover(&ctx.cfg);
    let stale = found.iter().filter(|h| h.harness == harness).any(|h| {
        let state = world.accounts.get(&h.key());
        !state.is_some_and(|s| s.unsigned) && accounts::due(harness_of(h), state, now)
    });
    // The same judgment the dispatcher makes: a check never spends past an
    // account's floor or on a reading it cannot trust.
    let usable: Vec<&Home> = found
        .iter()
        .filter(|h| h.harness == harness)
        .filter(|h| accounts::fit(h, world.accounts.get(&h.key()), model.as_deref(), &ctx.cfg.limits, now).is_ok())
        .collect();
    let twins = accounts::twins(&found);
    let mut load: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut worlds = ctx.neighbours();
    worlds.push(world);
    for (account, n) in dispatch::seats_taken(&worlds) {
        *load.entry(accounts::seat(&twins, &account).to_owned()).or_default() += n;
    }
    let worker = |h: &&&Home| job.account.as_deref() == Some(h.key().as_str());
    let busy =
        |h: &&&Home| load.get(accounts::seat(&twins, &h.key())).copied().unwrap_or(0) >= ctx.cfg.limits.per_account;
    // Another idle account, else the worker's own, whose seat its ended run
    // just freed; never a seat another job holds.
    let home = usable.iter().find(|h| !worker(h) && !busy(h)).or_else(|| usable.iter().find(worker));
    (home.map(|h| (*h).clone()), stale)
}

/// The session to resume, when the job has one its history can follow here:
/// only on the same harness, which can carry it between its own accounts.
fn resumable(job: &Job, found: &[Home], home: &Home, scratch: &Path) -> Option<String> {
    let id = job.session_id.as_ref()?;
    let from = found.iter().find(|h| Some(h.key()) == job.transcript_account)?;
    (from.harness == home.harness && harness_of(home).carry(id, Some(from), home, scratch)).then(|| id.clone())
}

/// What the session is told first: a resumed one already has the brief. A review is told which commit
/// to judge on every start and resume, since the branch may have moved since it last looked.
fn opening(brief: &str, message: Option<&str>, resumed: bool, preamble: Option<&str>) -> String {
    let told = match (message, resumed) {
        (Some(m), true) => m.to_owned(),
        (None, true) => "Continue where you left off.".into(),
        (Some(m), false) => format!("{brief}\n\n{m}"),
        (None, false) => brief.to_owned(),
    };
    preamble.map_or(told.clone(), |p| format!("{p}\n\n{told}"))
}

/// Inside the sandbox the sandbox is the boundary; outside it, the harness's own approvals are.
fn permissions(sandboxed: bool, readonly: bool) -> Permissions {
    match (sandboxed, readonly) {
        (true, _) => Permissions::Unrestricted,
        (false, true) => Permissions::ReadOnly,
        (false, false) => Permissions::EditsOnly,
    }
}

fn now_s() -> i64 {
    secs(now_micros())
}

/// What a verdict means for the job, and what it tells the other systems.
fn conclude(ctx: &Ctx, job: &Job, key: &str, v: Verdict, now_s: i64) -> Result<Ended> {
    let port = port(ctx, job);
    let s = &ctx.cfg.session;
    Ok(match v {
        Verdict::Done => (Outcome::Done, None),
        Verdict::FollowUp(message) if job.follow_ups == 0 => {
            port.emit(SessionEvent::FollowUp { job: job.name.clone(), message, budget_usd: None })?;
            (Outcome::Retry, Some("ended without its final report; asked once more".into()))
        }
        Verdict::FollowUp(_) => (Outcome::Done, Some(format!("no {} section after a follow-up", s.final_marker))),
        Verdict::Limited { until } => {
            // Two minutes after a stated reset; fifteen when none is stated (an overload or bare 429).
            let until = until.map_or(now_s + 15 * 60, |u| u + 120);
            port.emit(SessionEvent::Limited { account: key.to_owned(), until })?;
            (Outcome::Retry, Some(format!("{key} hit its limit; moving on")))
        }
        Verdict::SignedOut => {
            port.emit(SessionEvent::SignedOut { account: key.to_owned() })?;
            (Outcome::Retry, Some(format!("{key} is signed out; moving on")))
        }
        Verdict::NoOutput if job.reruns < ctx.cfg.limits.max_retries => {
            (Outcome::Rerun, Some("no output; trying again".into()))
        }
        Verdict::NoOutput => (Outcome::Failed, Some("no output, again".into())),
        Verdict::Budget => (Outcome::Failed, Some("reached its spend cap; resume with --budget to go on".into())),
        Verdict::Failed(why) => (Outcome::Failed, Some(why)),
    })
}

/// The invocation, niced, inside the sandbox when it is on.
fn command(ctx: &Ctx, inv: &Invocation, mounts: &Mounts<'_>) -> Command {
    let mut argv: Vec<OsString> = vec!["nice".into(), "-n".into(), ctx.cfg.session.nice.to_string().into()];
    argv.push(inv.program.clone());
    argv.extend(inv.args.iter().cloned());
    let env = sandbox::session_env(ctx, inv);
    if ctx.cfg.sandbox.enabled {
        let mut c = Command::new("bwrap");
        c.args(sandbox::args(ctx, &accounts::home(), mounts, env)).args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).env_clear().envs(env).current_dir(mounts.cwd);
        c
    }
}

/// One run's own limits and files.
struct Run<'a> {
    /// The session whose renaming the harness reports, for the worker; none for a check.
    session_id: Option<&'a str>,
    budget_usd: f64,
    /// The job's files its stream and its stderr go to.
    log: &'a str,
    stderr: &'a str,
    /// What to measure while it runs, for a job's worker: its clone (a build job) and its `/tmp`.
    measure: Vec<&'a Path>,
}

/// What the runner hears while it drives a session: a line of output, the
/// end of the output, or the clone's measured size.
enum Heard {
    Line(std::io::Result<String>),
    End,
    Disk(u64),
}

/// The longest one measurement of a clone may take.
const DISK_WALK: Duration = Duration::from_secs(120);
/// After a `disk` decision says `wait`, the least time before it is asked again.
const DISK_REASK: Duration = Duration::from_mins(60);

/// Why a clone of `bytes` should stop its job, when it should: it is over
/// `limits.job_disk_gb`, or the disk has `free_gb` left, under `limits.min_free_disk_gb`
/// while this is the largest clone running (`largest`), since stopping a small one
/// would not help and stopping every one would be worse than the full disk.
pub fn disk_reason(limits: &crate::config::Limits, bytes: u64, free_gb: Option<f64>, largest: bool) -> Option<String> {
    let gb = |g: f64| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        os::size_str((g * 1e9) as u64)
    };
    #[allow(clippy::cast_precision_loss)]
    let held = bytes as f64 / 1e9;
    if limits.job_disk_gb > 0.0 && held > limits.job_disk_gb {
        return Some(format!("disk: clone {} over {}", os::size_str(bytes), gb(limits.job_disk_gb)));
    }
    let free = free_gb.filter(|&f| largest && limits.min_free_disk_gb > 0.0 && f < limits.min_free_disk_gb)?;
    Some(format!("disk: {} free, under {}", gb(free), gb(limits.min_free_disk_gb)))
}

/// A measured clone: records its size, and when it is a reason to stop,
/// asks the `disk` chain (none set: stop). `Some(reason)` stops the job; `None` lets it go on.
fn on_disk(ctx: &Ctx, job: &Job, bytes: u64, quiet_until: &mut Option<Instant>) -> Result<Option<String>> {
    port(ctx, job).emit(SessionEvent::Disk { job: job.name.clone(), bytes })?;
    if quiet_until.is_some_and(|t| Instant::now() < t) {
        return Ok(None);
    }
    let world = ctx.world()?;
    let largest = world
        .jobs
        .values()
        .filter(|o| o.name != job.name && o.status.has_runner())
        .all(|o| o.disk_bytes.unwrap_or(0) <= bytes);
    let free = os::free_disk_gb(&ctx.clones);
    let Some(why) = disk_reason(&ctx.cfg.limits, bytes, free, largest) else { return Ok(None) };
    let Some(handlers) = decide::chain(&ctx.cfg, Point::Disk) else { return Ok(Some(why)) };
    let mut req = Request::new(job, Point::Disk);
    req.brief = std::fs::read_to_string(ctx.job_file(&job.name, "prompt.md")).unwrap_or_default();
    req.situation = format!(
        "{why}. The job is running. Stopping it keeps its clone and session, so it can be resumed after cleanup."
    );
    req.spent_usd = job.cost_usd;
    req.earlier = decide::earlier(&world, job);
    let d = decide::decide(ctx, job, &req, &handlers, &|| None, Action::Stop { why: why.clone() })?;
    if d.action == Action::Wait {
        *quiet_until = Some(Instant::now() + DISK_REASK);
        return Ok(None);
    }
    Ok(Some(why))
}

/// Runs the agent, feeding its stdin and teeing its stream to the job's log.
/// Ends it if it goes silent, runs past its time limit or spends past its
/// cap: the same limits for every harness, whatever the CLI enforces itself.
fn drive(
    ctx: &Ctx,
    job: &Job,
    account: &str,
    run: &Run<'_>,
    cmd: &mut Command,
    stdin: Option<&str>,
    mut reader: Box<dyn Reader>,
) -> Result<(Report, Ending, Option<String>)> {
    let port = port(ctx, job);
    let mut log = Ctx::append_private(&ctx.job_file(&job.name, run.log))?;
    let mut child = cmd
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Ctx::append_private(&ctx.job_file(&job.name, run.stderr))?)
        .spawn()
        .context("starting the session")?;
    if let (Some(text), Some(mut pipe)) = (stdin.map(str::to_owned), child.stdin.take()) {
        // A writer thread, so a full pipe cannot block reading the output.
        std::thread::spawn(move || pipe.write_all(text.as_bytes()));
    }
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = mpsc::channel();
    let lines = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if lines.send(Heard::Line(line)).is_err() {
                return;
            }
        }
        let _ = lines.send(Heard::End);
    });
    // Measures the clone now and every `limits.disk_sample_minutes`, until `_stop` is dropped.
    let (_stop, stopped) = mpsc::channel::<()>();
    if !run.measure.is_empty() {
        let dirs: Vec<PathBuf> = run.measure.iter().map(|p| (*p).to_path_buf()).collect();
        let every = Duration::from_secs_f64(ctx.cfg.limits.disk_sample_minutes * 60.0);
        std::thread::spawn(move || {
            loop {
                if tx.send(Heard::Disk(os::dirs_size(&dirs, DISK_WALK))).is_err() {
                    return;
                }
                if !matches!(stopped.recv_timeout(every), Err(mpsc::RecvTimeoutError::Timeout)) {
                    return;
                }
            }
        });
    }

    let silent_after = Duration::from_secs(ctx.cfg.limits.silent_minutes * 60);
    let deadline = Instant::now() + Duration::from_secs_f64(ctx.cfg.session.max_hours * 3600.0);
    let mut ending = Ending::default();
    let mut disk = None;
    let mut quiet_until = None;
    let mut heard_at = Instant::now();
    let mut named = run.session_id.map(str::to_owned);
    loop {
        let wait =
            silent_after.saturating_sub(heard_at.elapsed()).min(deadline.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Heard::Line(Ok(line))) => {
                heard_at = Instant::now();
                if reader.keep(&line) {
                    writeln!(log, "{line}")?;
                }
                if let Some(meters) = reader.line(&line) {
                    port.emit(SessionEvent::Rate { account: account.to_owned(), meters })?;
                }
                let r = reader.report();
                if let (Some(named), Some(id)) = (named.as_mut(), r.session_id.as_deref())
                    && id != named
                {
                    port.emit(SessionEvent::Identified { job: job.name.clone(), session_id: id.to_owned() })?;
                    id.clone_into(named);
                }
                if r.cost_usd > run.budget_usd {
                    ending.over_budget = true;
                    let _ = child.kill();
                    break;
                }
            }
            Ok(Heard::Disk(bytes)) => {
                disk = on_disk(ctx, job, bytes, &mut quiet_until)?;
                if disk.is_some() {
                    // bwrap's --die-with-parent and pid namespace take everything inside with it.
                    let _ = child.kill();
                    break;
                }
            }
            Ok(Heard::Line(Err(_)) | Heard::End) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                (ending.silent, ending.timed_out) =
                    if Instant::now() >= deadline { (false, true) } else { (true, false) };
                // bwrap's --die-with-parent and pid namespace take everything inside with it.
                let _ = child.kill();
                break;
            }
        }
    }
    ending.exit_ok = child.wait()?.success();
    Ok((reader.report().clone(), ending, disk))
}

/// The build jobs whose branches a build job starts from: those it waits
/// for, through any read-only job between (build, review, build), and the one
/// it forks, each with its clone and branch.
pub fn bases(world: &World, job: &Job) -> Vec<(String, PathBuf, String)> {
    let mut todo: Vec<&String> = job.after.iter().collect();
    todo.extend(job.fork.as_ref().map(|f| &f.job));
    let mut seen: Vec<&String> = Vec::new();
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
    while let Some(n) = todo.pop() {
        if seen.contains(&n) {
            continue;
        }
        seen.push(n);
        let Some(j) = world.jobs.get(n) else { continue };
        if j.readonly() {
            todo.extend(&j.after);
        } else if let (Some(dir), Some(branch)) = (&j.workdir, &j.branch) {
            out.push((n.clone(), dir.clone(), branch.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Of the base branches' tips, the one every other is an ancestor of, or why
/// there is none. `None` with no bases: the repository's `HEAD`.
pub fn pick_base(
    tips: &[(String, String)],
    is_ancestor: impl Fn(&str, &str) -> bool,
) -> Result<Option<String>, String> {
    let Some(top) = tips.iter().find(|(_, t)| tips.iter().all(|(_, o)| o == t || is_ancestor(o, t))) else {
        let names: Vec<&str> = tips.iter().map(|(n, _)| n.as_str()).collect();
        return if tips.is_empty() {
            Ok(None)
        } else {
            Err(format!(
                "the branches of {} diverge, so no one of them holds all their work; land one into another, \
                 or queue this job after one of them",
                names.join(" and ")
            ))
        };
    };
    Ok(Some(top.1.clone()))
}

/// A private clone that borrows the repository's objects (`--shared`), so it is
/// small and works across filesystems, on its own branch, unable to push.
/// A build job with `bases` starts from the newest of their branch tips (`pick_base`).
/// A job that has run (`ran`) and lost its clone to gc starts from its own work in
/// the repository instead: the rescue ref gc made, else its branch as fetched.
fn clone(
    ctx: &Ctx,
    name: &str,
    bases: &[(String, PathBuf, String)],
    ran: bool,
) -> Result<(PathBuf, String, Option<String>)> {
    let dir = ctx.clones.join(name);
    let branch = format!("{}{name}", ctx.cfg.session.branch_prefix);
    std::fs::create_dir_all(&ctx.clones)?;
    let ok = |c: &mut Command, what: &str| -> Result<()> {
        let out = c.output().with_context(|| what.to_owned())?;
        if !out.status.success() {
            bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    };
    // The job has no clone on record, so anything here is relay's own leftover from a failed attempt.
    let _ = std::fs::remove_dir_all(&dir);
    let made = (|| -> Result<Option<String>> {
        ok(Command::new("git").args(["clone", "--quiet", "--shared"]).arg(&ctx.cfg.repo).arg(&dir), "git clone")?;
        let mut tips = Vec::new();
        for (job, from, b) in bases {
            let what = format!("fetching {job}'s branch {b}");
            ok(Ctx::git(&dir).arg("fetch").arg("--quiet").arg(from).arg(format!("{b}:refs/relay/base/{job}")), &what)?;
            let tip = checkpoint_rev(&dir, &format!("refs/relay/base/{job}")).with_context(|| what.clone())?;
            tips.push((job.clone(), tip));
        }
        let ancestor = |a: &str, b: &str| {
            Ctx::git(&dir).args(["merge-base", "--is-ancestor", a, b]).status().is_ok_and(|s| s.success())
        };
        let own = [format!("refs/relay/rescue/{name}"), format!("refs/heads/{branch}")]
            .into_iter()
            .find(|r| ran && checkpoint_rev(&ctx.cfg.repo, r).is_some());
        let start = match own {
            Some(r) => {
                let what = format!("fetching {name}'s own work from {r}");
                ok(
                    Ctx::git(&dir).arg("fetch").arg("--quiet").arg(&ctx.cfg.repo).arg(format!("+{r}:refs/relay/own")),
                    &what,
                )?;
                checkpoint_rev(&dir, "refs/relay/own")
            }
            None => pick_base(&tips, ancestor).map_err(|e| anyhow::anyhow!(e))?,
        };
        let mut switch = Ctx::git(&dir);
        switch.args(["switch", "--quiet", "-c", &branch]).args(start.as_deref());
        ok(&mut switch, "git switch")?;
        ok(Ctx::git(&dir).args(["remote", "set-url", "--push", "origin", "relay-jobs-do-not-push"]), "git remote")?;
        // Where the branch started, for a job that starts fresh: the revert control runs the check here.
        Ok(if ran { None } else { checkpoint_rev(&dir, "HEAD") })
    })();
    if made.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    made.map(|base| (dir, branch, base))
}

/// The commit `rev` names in `dir`.
fn checkpoint_rev(dir: &Path, rev: &str) -> Option<String> {
    let out = Ctx::git(dir).args(["rev-parse", "--verify", rev]).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod disk_tests {
    use super::disk_reason;
    use crate::config::Limits;

    #[test]
    fn a_clone_over_the_ceiling_or_a_full_disk_names_its_reason() {
        let l = Limits { job_disk_gb: 40.0, min_free_disk_gb: 20.0, ..Limits::default() };
        let gb = |g: u64| g * 1_000_000_000;
        assert_eq!(disk_reason(&l, gb(52), Some(500.0), true).as_deref(), Some("disk: clone 52G over 40G"));
        assert_eq!(disk_reason(&l, gb(40), Some(500.0), true), None, "at the ceiling is not over it");
        assert_eq!(disk_reason(&l, gb(12), Some(8.0), true).as_deref(), Some("disk: 8G free, under 20G"));
        assert_eq!(disk_reason(&l, gb(12), Some(8.0), false), None, "only the largest clone answers for a full disk");
        assert_eq!(disk_reason(&l, gb(12), None, true), None, "unmeasured free space is not a reason");
        assert_eq!(
            disk_reason(&l, gb(52), Some(8.0), true).as_deref(),
            Some("disk: clone 52G over 40G"),
            "the job's own size comes first"
        );
        let off = Limits { job_disk_gb: 0.0, min_free_disk_gb: 0.0, ..l };
        assert_eq!(disk_reason(&off, gb(900), Some(0.1), true), None, "zero turns both off");
    }
}

#[cfg(test)]
mod base_tests {
    use std::path::PathBuf;

    use super::{bases, pick_base};
    use crate::world::{Job, World};

    #[test]
    fn a_build_finds_its_bases_through_a_review_between() {
        let build = |n: &str, after: &[&str]| Job {
            name: n.into(),
            after: after.iter().map(|a| (*a).to_owned()).collect(),
            workdir: Some(PathBuf::from(format!("/c/{n}"))),
            branch: Some(format!("job/{n}")),
            ..Job::default()
        };
        let review = |n: &str, after: &[&str]| Job {
            kind: crate::bus::Kind::Read,
            workdir: None,
            branch: None,
            ..build(n, after)
        };
        let mut w = World::default();
        for j in [build("one", &[]), review("look", &["one"]), review("loop", &["loop"]), build("other", &[])] {
            w.jobs.insert(j.name.clone(), j);
        }
        let names = |j: &Job| bases(&w, j).into_iter().map(|b| b.0).collect::<Vec<_>>();
        assert_eq!(names(&build("two", &["look"])), ["one"], "build, review, build");
        assert_eq!(names(&build("two", &["look", "other", "loop"])), ["one", "other"]);
        assert!(names(&build("two", &["loop"])).is_empty(), "a cycle ends");
    }

    #[test]
    fn a_build_starts_from_the_newest_of_its_bases_or_fails_when_they_diverge() {
        let tips = |ts: &[&str]| ts.iter().map(|t| ((*t).to_uppercase(), (*t).to_owned())).collect::<Vec<_>>();
        // a ← b on one line; c off on its own.
        let anc = |x: &str, y: &str| matches!((x, y), ("a", "b"));
        assert_eq!(pick_base(&[], anc), Ok(None), "no build dependency: HEAD");
        assert_eq!(pick_base(&tips(&["a"]), anc), Ok(Some("a".into())));
        assert_eq!(pick_base(&tips(&["a", "b"]), anc), Ok(Some("b".into())));
        assert_eq!(pick_base(&tips(&["b", "a"]), anc), Ok(Some("b".into())));
        assert_eq!(pick_base(&tips(&["a", "a"]), anc), Ok(Some("a".into())), "the same tip twice");
        let e = pick_base(&tips(&["b", "c"]), anc).unwrap_err();
        assert!(e.contains("B and C diverge"), "{e}");
    }
}

#[cfg(test)]
mod dev_env_tests {
    #[test]
    fn the_environment_is_read_after_the_marker_and_filtered() {
        // A "shell" that prints a banner, sets variables, then runs the command it is given.
        let shell: Vec<String> =
            ["sh", "-c", "echo welcome; export DEV_TOOL=/nix/store/x API_TOKEN=s3cret; exec \"$@\"", "shell"]
                .map(String::from)
                .to_vec();
        let deny = regex::Regex::new("TOKEN").unwrap();
        let env = super::dev_env(&shell, &deny, std::path::Path::new("/")).unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.clone());
        assert_eq!(get("DEV_TOOL").as_deref(), Some(std::ffi::OsStr::new("/nix/store/x")));
        assert_eq!(get("API_TOKEN"), None, "secret-named variables stay out");
        assert!(env.iter().all(|(k, _)| !k.to_string_lossy().contains("welcome")), "the banner is not a variable");
        assert!(super::dev_env(&["false".to_owned()], &deny, std::path::Path::new("/")).is_err());
        assert_eq!(super::dev_env(&[], &deny, std::path::Path::new("/")).unwrap(), Vec::new());
    }
}
