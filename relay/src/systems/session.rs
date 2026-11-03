//! session: runs one dispatched job on its account's harness and reports
//! what happened. Emits: cloned, started, identified, rate, limited,
//! signed-out, follow-up, requeued, checking, checked, bar-checked, reviewed, ended. One process per running job, started by
//! dispatch. Nothing here names a harness: the account's harness builds the
//! command and reads the output, and the limits are enforced here for all.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::Verdict as Judgement;
use crate::bus::{Action, BarResult, Check, CheckVerdict, Outcome, Point, Reason, SessionEvent, now_micros, secs};
use crate::ctx::Ctx;
use crate::harness::report::{Ending, Report, Verdict, verdict};
use crate::harness::{Home, Invocation, Permissions, Reader, Session, Task};
use crate::os;
use crate::sandbox::{self, Mounts};
use crate::systems::checkpoint::{self, Next};
use crate::systems::decide::{self, CheckRun, Request, Reviewing};
use crate::world::{Job, Status, World};

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
    port.emit(SessionEvent::Ended { job: name, outcome, cost_usd: spend.0, turns: spend.1, reason })?;
    ctx.wake_dispatch()
}

type Ended = (Outcome, Option<String>);

fn workspace_note(ctx: &Ctx, job: &Job) -> String {
    let place = if job.readonly {
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

// One attempt, start to verdict, in the order things happen; splitting it would only scatter that order.
#[allow(clippy::too_many_lines)]
fn attempt(ctx: &Ctx, job: &Job, spend: &mut (f64, u32)) -> Result<Ended> {
    let port = port(ctx, job);
    let key = job.account.clone().context("dispatched without an account")?;
    let found = accounts::discover(&ctx.cfg);
    let Some(home) = found.iter().find(|h| h.key() == key) else {
        return Ok((Outcome::Retry, Some(format!("account {key} is no longer available"))));
    };
    let harness = harness_of(home);
    let conf = ctx.cfg.harness(harness.id());
    let model = job.models.get(harness.id()).cloned().or_else(|| conf.model.clone());
    if let Some(reason) = model.as_deref().and_then(|m| ctx.cfg.session.banned(m)) {
        return Ok((Outcome::Failed, Some(format!("model banned: {reason}"))));
    }

    let mut workdir = job.workdir.clone();
    if !job.readonly && workdir.is_none() {
        let bases = bases(&ctx.world()?, job);
        let (dir, branch, base) = match clone(ctx, &job.name, &bases, job.session_id.is_some()) {
            Ok(made) => made,
            Err(e) => return Ok((Outcome::Failed, Some(format!("{e:#}")))),
        };
        port.emit(SessionEvent::Cloned { job: job.name.clone(), workdir: dir.clone(), branch, base })?;
        workdir = Some(dir);
    }
    let cwd = if job.readonly { ctx.cfg.repo.clone() } else { workdir.clone().unwrap_or_default() };
    let scratch = ctx.job_scratch(&job.name);
    std::fs::create_dir_all(&scratch)?;

    if let Some(failed) = forkable(job, &found, home, &cwd) {
        return Ok(failed);
    }
    let fork = job.fork.as_ref().filter(|f| job.session_id.as_deref().is_none_or(|s| s == f.session_id));
    let resumed = if fork.is_some() { None } else { resumable(job, &found, home, &scratch) };
    let session_id =
        fork.map(|f| f.session_id.clone()).or(resumed.clone()).unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let brief = std::fs::read_to_string(ctx.job_file(&job.name, "prompt.md")).context("the job's brief is gone")?;
    // A review reads a branch that lives in the builder's clone: bring it to the repository first.
    let reviewing = match review_setup(ctx, job, model.as_deref()) {
        Ok(r) => r,
        Err(why) => return Ok((Outcome::Failed, Some(why))),
    };
    let preamble = reviewing.as_ref().map(review_preamble);
    // A fork has its parent's context but not its own task: it is told its brief.
    let prompt = opening(&brief, job.message.as_deref(), resumed.is_some(), preamble.as_deref());
    // What a read-only job reads is the live repository, so its commit is taken now, not when queued.
    let commit = if job.readonly { Ctx::clean_head(&ctx.cfg.repo) } else { None };
    port.emit(SessionEvent::Started {
        job: job.name.clone(),
        account: key.clone(),
        session_id: session_id.clone(),
        commit,
        family: job.review_of.as_ref().and(model.as_deref()).map(crate::config::model_family),
    })?;

    let s = &ctx.cfg.session;
    let briefing = s
        .briefing
        .replace("{workspace}", &workspace_note(ctx, job))
        .replace("{branch}", job.branch.as_deref().unwrap_or(""));
    let name = format!("relay-{}", job.name);
    let task = Task {
        prompt: &prompt,
        model: model.as_deref(),
        effort: job.effort.as_deref().or(conf.effort.as_deref()),
        session: match (fork, &resumed) {
            (Some(f), _) => Session::Fork(&f.session_id),
            (None, Some(_)) => Session::Resume(&session_id),
            (None, None) => Session::New(&session_id),
        },
        permissions: permissions(ctx.cfg.sandbox.enabled, job.readonly),
        briefing: Some(&briefing),
        budget_usd: Some(job.budget_usd),
        name: Some(&name),
        events: true,
        lean: true,
        scratch: &scratch,
    };
    let mut inv = match harness.invocation(home, &task) {
        Ok(i) => conf.locate(i),
        Err(refusal) => return Ok((Outcome::Failed, Some(refusal.0))),
    };
    inv.args.extend(conf.extra_args.iter().map(OsString::from));
    layer_env(ctx, &mut inv, &cwd)?;
    inv.write_files().context("writing the prompt file")?;
    let mut cmd = command(ctx, home, &inv, workdir.as_deref(), &scratch, &cwd);

    // An earlier attempt's answer must not pass for this one's.
    let _ = std::fs::remove_file(ctx.job_file(&job.name, "result.md"));
    let run = Run {
        session_id: Some(&session_id),
        budget_usd: job.budget_usd,
        log: "log.ndjson",
        stderr: "stderr.log",
        clone: workdir.as_deref(),
    };
    let (report, ending, disk) = drive(ctx, job, &key, &run, &mut cmd, inv.stdin.as_deref(), harness.reader())?;
    if !report.answer.is_empty() {
        std::fs::write(ctx.job_file(&job.name, "result.md"), format!("{}\n", report.answer))?;
    }
    *spend = (report.cost_usd, report.turns);
    if disk.is_some() {
        // Stopped, not failed: the clone and the session are kept for a resume after cleanup.
        return Ok((Outcome::Stopped, disk));
    }
    let run = Ran {
        report: &report,
        dir: workdir.as_deref(),
        scratch: &scratch,
        brief: &brief,
        reviewing: reviewing.as_ref(),
        model: model.as_deref(),
    };
    match verdict(&report, ending, &s.final_marker, job.readonly, now_s()) {
        Verdict::Budget => at_cap(ctx, job, &run),
        v => {
            let ended = conclude(ctx, job, &key, v, now_s())?;
            at_end(ctx, job, home, &run, ended, ending.silent)
        }
    }
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
    let cap = match checkpoint::next(&ctx.cfg.limits, job.readonly, hooked, job.budget_usd, spent) {
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
    port.emit(SessionEvent::Checked { job: job.name.clone(), check })?;
    Ok(if next.is_none() {
        (Outcome::Failed, Some("waits for a human at its checkpoint; resume with --budget to go on".into()))
    } else {
        (Outcome::Retry, None)
    })
}

/// The chain that judges a run when it ends: `hooks.ended`, else just `rule` for a job that declares
/// something for it to judge (an acceptance check, a review, a profile that escalates).
fn ended_chain(ctx: &Ctx, job: &Job) -> Option<Vec<decide::Handler>> {
    decide::chain(&ctx.cfg, Point::Ended)
        .or_else(|| decide::declares(&ctx.cfg, job).then(|| vec![decide::Handler::Rule]))
}

/// A run that ended done or failed, when `hooks.ended` is set (or the job declares a check, a review
/// or an escalation): the chain may accept it, resume it, start it afresh under another profile,
/// queue a follow-up, give a review's verdict, or stop it. Limits, sign-outs and the first missing
/// report stay relay's own.
fn at_end(ctx: &Ctx, job: &Job, home: &Home, run: &Ran<'_>, ended: Ended, silent: bool) -> Result<Ended> {
    let Some(handlers) = ended_chain(ctx, job) else { return Ok(ended) };
    if !matches!(ended.0, Outcome::Done | Outcome::Failed) {
        return Ok(ended);
    }
    let (mut req, _) = request(ctx, job, run, Point::Ended)?;
    let how = if ended.0 == Outcome::Done { "done" } else { "failed" };
    req.situation = format!("The run ended {how}{}.", ended.1.as_deref().map(|r| format!(": {r}")).unwrap_or_default());
    if let Some(dir) = run.dir {
        let now = checkpoint::point(dir, &run.scratch.join("checkpoint.index"));
        req.evidence = checkpoint::evidence(dir, &checkpoint::base(dir, &[]), &now);
    }
    // The bar, before anyone judges: every handler hears of it, and none can accept a red run.
    if let (Outcome::Done, Some(cmd), Some(dir)) = (ended.0, job.check.as_deref(), run.dir) {
        req.check = Some(check_run(ctx, job, home, cmd, dir, run.scratch)?);
    }
    req.reviewing = run.reviewing.cloned();
    if let Some(r) = run.reviewing {
        let _ = write!(
            req.situation,
            " The job is a review of {}'s branch ({} at {}); the report's `verdict:` line is its verdict, given as the `review` action.",
            r.job, r.reference, r.commit
        );
    }
    let red = req.check.as_ref().filter(|c| c.result != BarResult::Pass).cloned();
    let final_done = final_status_done(&run.report.answer, &ctx.cfg.session.final_marker);
    let escalate = decide::escalation(&ctx.cfg, job).map(|(to, after)| (to, after, job.failed_ends + 1));
    let rule = || {
        ended_rule(&EndRule {
            outcome: ended.0,
            final_done,
            silent,
            follow_ups: job.follow_ups,
            check: req.check.as_ref(),
            answer: run.report.answer.as_str(),
            marker: ctx.cfg.session.final_marker.as_str(),
            reviewing: run.reviewing.is_some(),
            escalate: escalate.as_ref().map(|(to, after, failed)| (to.as_str(), *after, *failed)),
        })
    };
    // A red run is never accepted, not even when every handler defers or is refused.
    let fallback = if red.is_some() { Action::Stop { why: String::new() } } else { Action::Accept };
    let d = decide::decide(ctx, job, &req, &handlers, &rule, fallback)?;
    let port = port(ctx, job);
    Ok(match d.action {
        Action::Stop { why } => {
            let why = if !why.is_empty() {
                why
            } else if let Some(c) = &red {
                format!("acceptance check {}: {}", c.result.as_str(), d.why)
            } else {
                d.why
            };
            (Outcome::Failed, Some(why))
        }
        Action::Resume { message, budget } => {
            let message = if message.trim().is_empty() { "Continue where you left off.".into() } else { message };
            port.emit(SessionEvent::FollowUp { job: job.name.clone(), message, budget_usd: budget })?;
            (Outcome::Retry, Some(format!("resumed by decision {}", d.id)))
        }
        Action::Requeue { profile } => {
            let p = ctx.cfg.profile(&profile)?;
            let harnesses: Vec<String> = p.harness.iter().cloned().collect();
            let models = match (&p.harness, &p.model) {
                (Some(h), Some(m)) => [(h.clone(), m.clone())].into(),
                _ => std::collections::BTreeMap::new(),
            };
            port.emit(SessionEvent::Requeued {
                job: job.name.clone(),
                profile: profile.clone(),
                harnesses,
                models,
                effort: p.effort.clone(),
                budget_usd: p.budget.unwrap_or(job.budget_usd),
            })?;
            (Outcome::Retry, Some(format!("requeued under profile {profile}")))
        }
        Action::FollowUp { brief, profile, after, fork, name } => {
            let spec = crate::systems::queue::FollowUp { name, brief, profile, after, fork };
            let queued = crate::systems::queue::follow_up(ctx, job, spec)?;
            // A follow-up is not an acceptance: a red run stays failed.
            let outcome = if red.is_some() { Outcome::Failed } else { ended.0 };
            (
                outcome,
                Some(ended.1.map_or_else(
                    || format!("follow-up {queued} queued"),
                    |r| format!("{r}; follow-up {queued} queued"),
                )),
            )
        }
        Action::Review { verdict, reason, message } => {
            let note = carry_out_review(ctx, job, run, verdict, reason, &message)?;
            (ended.0, Some(note))
        }
        _ => ended,
    })
}

/// Records a review's verdict on the reviewed job, and for a reject resumes the builder's session
/// with the findings, within `hooks.max_decisions`. Returns the line for the review's own record.
fn carry_out_review(
    ctx: &Ctx,
    job: &Job,
    run: &Ran<'_>,
    verdict: Judgement,
    reason: Option<Reason>,
    findings: &str,
) -> Result<String> {
    let (Some(of), Some(seen)) = (job.review_of.as_deref(), run.reviewing) else {
        return Ok(format!("verdict {}, but this job reviews no branch", verdict.as_str()));
    };
    let family = run.model.map(crate::config::model_family);
    port(ctx, job).emit(SessionEvent::Reviewed {
        job: job.name.clone(),
        of: of.to_owned(),
        commit: seen.commit.clone(),
        verdict,
        reason,
        family,
    })?;
    let said = match reason {
        Some(r) => format!("review of {of}: {} ({})", verdict.as_str(), r.as_str()),
        None => format!("review of {of}: {}", verdict.as_str()),
    };
    if verdict == Judgement::Accept {
        return Ok(said);
    }
    let message = format!(
        "A review of your branch by {} at {} rejected it ({}). Its findings follow; address them, commit your \
         work, and end with your FINAL section again.\n\n{}",
        job.name,
        seen.commit,
        reason.map_or("other", Reason::as_str),
        checkpoint::bounded(findings.trim(), 4096)
    );
    Ok(match crate::systems::queue::resume_reviewed(ctx, of, message)? {
        None => format!("{said}; {of} resumed with the findings"),
        Some(why) => format!("{said}; {of} not resumed: {why}"),
    })
}

/// What the `rule` handler judges a run ended by.
struct EndRule<'a> {
    outcome: Outcome,
    final_done: bool,
    silent: bool,
    follow_ups: u32,
    /// The acceptance check's latest run, for a run that ended done.
    check: Option<&'a CheckRun>,
    answer: &'a str,
    marker: &'a str,
    /// The job is a review of a branch.
    reviewing: bool,
    /// Where the profile escalates to, after how many failed ends, and how many there are with this one.
    escalate: Option<(&'a str, u32, u32)>,
}

/// The `rule` handler when a run ends: a red acceptance check resumes the session with its output
/// (or, when it proves nothing, stops the job); a review's verdict is taken from its report; a clean
/// done is accepted; a first silence resumes the same session; enough failed ends requeue the job under
/// the profile it escalates to. Anything else is deferred.
fn ended_rule(r: &EndRule<'_>) -> Option<(Action, String)> {
    match r.outcome {
        Outcome::Done => {
            if let Some(c) = r.check {
                match c.result {
                    BarResult::Pass => {}
                    BarResult::Fail => {
                        let message = check_message(c);
                        let why = format!("the acceptance check failed (exit {}); resumed with its output", c.exit);
                        return Some((Action::Resume { message, budget: None }, why));
                    }
                    BarResult::CheckProvesNothing => {
                        let why = format!(
                            "check-proves-nothing: `{}` also passes at the base commit, so it shows nothing about this change",
                            c.command
                        );
                        return Some((Action::Stop { why: why.clone() }, why));
                    }
                }
            }
            if r.reviewing
                && let Some((verdict, reason)) = review_verdict(r.answer, r.marker)
            {
                let tail =
                    r.answer.rfind(r.marker).filter(|_| !r.marker.is_empty()).map_or(r.answer, |i| &r.answer[i..]);
                let message = checkpoint::bounded(tail.trim(), 4096);
                return Some((
                    Action::Review { verdict, reason, message },
                    "the review's report gives its verdict".into(),
                ));
            }
            r.final_done.then(|| (Action::Accept, "done, and its report says status: done".into()))
        }
        Outcome::Failed if r.silent && r.follow_ups == 0 => {
            let message = "Continue where you left off.".to_owned();
            Some((Action::Resume { message, budget: None }, "went silent once; resumed in the same session".into()))
        }
        Outcome::Failed => {
            let (to, after, failed) = r.escalate?;
            (failed >= after).then(|| {
                (
                    Action::Requeue { profile: to.to_owned() },
                    format!("{failed} failed end(s), the profile escalates to {to} after {after}"),
                )
            })
        }
        _ => None,
    }
}

/// What the worker is told when its acceptance check failed.
fn check_message(c: &CheckRun) -> String {
    format!(
        "Your run ended, and then the acceptance check `{}` was run on your branch{} and failed (exit {}). \
         Fix what it reports, commit your work, and end with your FINAL section again. The end of its output:\n\n{}",
        c.command,
        c.commit.as_deref().map_or(String::new(), |c| format!(" at {}", c.get(..12).unwrap_or(c))),
        c.exit,
        c.tail.trim_end()
    )
}

/// A review's verdict from its report's final section: a `verdict: accept|reject` line, and for a
/// reject a `reason:` line naming a code (`other` when it names none that exists).
fn review_verdict(answer: &str, marker: &str) -> Option<(Judgement, Option<Reason>)> {
    let tail = if marker.is_empty() { Some(answer) } else { answer.rfind(marker).map(|i| &answer[i..]) }?;
    let field = |name: &str| {
        tail.lines().rev().find_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            l.get(..name.len() + 1)
                .filter(|h| h[..name.len()].eq_ignore_ascii_case(name) && h.ends_with(':'))
                .map(|_| l[name.len() + 1..].trim().to_ascii_lowercase())
        })
    };
    let verdict = match field("verdict")?.split_whitespace().next()? {
        "accept" | "accepted" => Judgement::Accept,
        "reject" | "rejected" => Judgement::Reject,
        _ => return None,
    };
    let reason = (verdict == Judgement::Reject).then(|| {
        field("reason").and_then(|r| Reason::parse(r.split_whitespace().next().unwrap_or(""))).unwrap_or(Reason::Other)
    });
    Some((verdict, reason))
}

/// How long the tail of a check's output may be.
const CHECK_TAIL: usize = 4096;

/// The end of `text`, at most `max` bytes, saying how much came before.
fn tail_of(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let cut = (text.len() - max..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
    format!("… ({cut} earlier bytes)\n{}", &text[cut..])
}

/// Runs the job's acceptance check on its clone, as the `rule` handler's bar for a run that ended
/// done, and records `bar-checked`. The check runs inside the same sandbox plan as the session, so
/// it sees what the session saw and can change only what it could, under `command_timeout_seconds`.
/// With a revert control it also runs at the job's base commit, where it must fail.
fn check_run(ctx: &Ctx, job: &Job, home: &Home, cmd: &str, dir: &Path, scratch: &Path) -> Result<CheckRun> {
    let commit = checkpoint_rev(dir, "HEAD");
    let dirty = Ctx::git(dir)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty());
    let (mut exit, mut text) = match &dirty {
        // The branch is what lands: tracked changes left uncommitted are not part of it.
        Some(status) => (1, format!("the clone has uncommitted changes; commit them first:\n{status}")),
        None => run_in_plan(ctx, home, cmd, dir, dir, scratch),
    };
    let mut base_exit = None;
    if exit == 0 && job.check_red_first {
        let base = job.base.clone().or_else(|| checkpoint::base(dir, &[]).head);
        let (code, note) = match base {
            Some(base) => {
                let at = scratch.join("check-base");
                let _ = std::fs::remove_dir_all(&at);
                let made = std::fs::create_dir_all(&at).is_ok() && export_commit(dir, &base, &at);
                let ran = if made {
                    run_in_plan(ctx, home, cmd, dir, &at, scratch)
                } else {
                    (-1, format!("could not check out the base commit {base} for the revert control"))
                };
                let _ = std::fs::remove_dir_all(&at);
                ran
            }
            None => (-1, "no base commit is known for the revert control".to_owned()),
        };
        base_exit = Some(code);
        if code == 0 {
            text = format!("{text}\n(the same check also passed at the base commit)");
        } else if code == -1 {
            exit = -1;
            text = note;
        }
    }
    let result = match (exit, base_exit) {
        (0, Some(0)) => BarResult::CheckProvesNothing,
        (0, _) => BarResult::Pass,
        _ => BarResult::Fail,
    };
    let tail = checkpoint::bounded(&tail_of(text.trim_end(), CHECK_TAIL), CHECK_TAIL + 64);
    port(ctx, job).emit(SessionEvent::BarChecked {
        job: job.name.clone(),
        commit: commit.clone(),
        exit,
        tail: tail.clone(),
        base_exit,
        result,
    })?;
    Ok(CheckRun { command: cmd.to_owned(), commit, result, exit, base_exit, tail })
}

/// Writes the tree of `commit` in the clone `dir` into the empty directory `into`.
fn export_commit(dir: &Path, commit: &str, into: &Path) -> bool {
    let Ok(mut archive) = Ctx::git(dir).args(["archive", "--format=tar", commit]).stdout(Stdio::piped()).spawn() else {
        return false;
    };
    let Some(tar) = archive.stdout.take() else { return false };
    let unpacked = Command::new("tar").arg("-x").arg("-C").arg(into).stdin(tar).status().is_ok_and(|s| s.success());
    unpacked & archive.wait().is_ok_and(|s| s.success())
}

/// Runs `cmd` with `sh -c` in `cwd`, inside the sandbox plan of a session on `home` with the job's
/// clone `dir`, bounded by `hooks.command_timeout_seconds` (killed 5 s later if it ignores the TERM).
/// Its exit status (-1 if it could not run) and the end of its combined output.
fn run_in_plan(ctx: &Ctx, home: &Home, cmd: &str, dir: &Path, cwd: &Path, scratch: &Path) -> (i32, String) {
    let go = || -> Result<(i32, String)> {
        let mut inv = Invocation::new("timeout");
        inv.args = ["-k", "5", &ctx.cfg.hooks.command_timeout_seconds.max(1).to_string(), "sh", "-c", cmd]
            .map(OsString::from)
            .to_vec();
        layer_env(ctx, &mut inv, cwd)?;
        let out = scratch.join("check.out");
        let file = std::fs::File::create(&out)?;
        let mut c = command(ctx, home, &inv, Some(dir), scratch, cwd);
        c.stdin(Stdio::null()).stdout(file.try_clone()?).stderr(file);
        let status = c.status().context("running the acceptance check")?;
        let text = tail_text(&out, 2 * CHECK_TAIL as u64);
        let _ = std::fs::remove_file(&out);
        Ok((status.code().unwrap_or(-1), text))
    };
    go().unwrap_or_else(|e| (-1, format!("the acceptance check could not run: {e:#}")))
}

/// The last `max` bytes of a file, as text.
fn tail_text(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Fetches the branch a review reads into the repository, as `land fetch` does, and says what the
/// reviewer is to read. Fails the review when the branch cannot be had, or when the reviewed job's
/// profile asks for an independent family and this review runs on the same one.
fn review_setup(ctx: &Ctx, job: &Job, model: Option<&str>) -> Result<Option<Reviewing>, String> {
    let Some(of) = job.review_of.as_deref() else { return Ok(None) };
    let world = ctx.world().map_err(|e| format!("{e:#}"))?;
    let Some(builder) = world.jobs.get(of) else { return Err(format!("the job under review, {of}, is gone")) };
    let asks = builder.profile.as_deref().and_then(|p| ctx.cfg.profiles.get(p)).and_then(|p| p.independent.as_deref());
    if let (Some(_), Some(model), Some(built)) = (asks, model, ctx.cfg.model_of(builder)) {
        let (a, b) = (crate::config::model_family(model), crate::config::model_family(&built));
        if a == b {
            return Err(format!(
                "independent review refused: {of} was built on {built} and this review runs on {model}, both the {a} family"
            ));
        }
    }
    let reference = format!("refs/relay/review/{}", job.name);
    let repo = &ctx.cfg.repo;
    let live = builder.workdir.as_ref().zip(builder.branch.as_ref()).filter(|(d, _)| d.exists());
    let fetched = match live {
        Some((dir, branch)) => Ctx::git(repo)
            .args(["fetch", "--quiet"])
            .arg(dir)
            .arg(format!("+{branch}:{reference}"))
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| {
                if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()) }
            }),
        // The clone is gone: its work is in the repository, rescued by gc or fetched by hand.
        None => [
            format!("refs/relay/rescue/{of}"),
            builder.branch.as_ref().map(|b| format!("refs/heads/{b}")).unwrap_or_default(),
        ]
        .into_iter()
        .filter(|r| !r.is_empty())
        .find_map(|r| checkpoint_rev(repo, &r))
        .ok_or_else(|| format!("{of}'s branch is in no clone or repository ref"))
        .and_then(|rev| {
            let ok = Ctx::git(repo).args(["update-ref", &reference, &rev]).status().is_ok_and(|s| s.success());
            if ok { Ok(()) } else { Err("git update-ref failed".to_owned()) }
        }),
    };
    fetched.map_err(|e| format!("fetching {of}'s branch for review: {e}"))?;
    let commit = checkpoint_rev(repo, &reference).ok_or_else(|| format!("{reference} was not created"))?;
    let base =
        builder.base.clone().filter(|b| checkpoint_rev(repo, &format!("{b}^{{commit}}")).is_some()).or_else(|| {
            let out = Ctx::git(repo).args(["merge-base", "HEAD", &commit]).output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .filter(|s| !s.is_empty() && out.status.success())
        });
    Ok(Some(Reviewing { job: of.to_owned(), reference, commit, base }))
}

/// What a review is told before its brief.
fn review_preamble(r: &Reviewing) -> String {
    let base = r.base.as_deref().unwrap_or("HEAD");
    let range = format!("{base}..{}", r.reference);
    let reasons: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
    format!(
        "This is a review of job {job}'s branch. relay fetched it into the repository as `{reference}`, at commit {commit}. \
         It started from {base}, so `git log {range}` lists its commits and `git diff {range}` is its change. \
         Review that, and judge the work as the brief asks.\n\n\
         End your FINAL section with a line `verdict: accept` or `verdict: reject`. For a reject, add a line \
         `reason: <code>` ({codes}) and put the findings the builder must act on in `notes`.",
        job = r.job,
        reference = r.reference,
        commit = r.commit,
        codes = reasons.join(", "),
    )
}

/// Whether the report's final section says `status: done`.
fn final_status_done(answer: &str, marker: &str) -> bool {
    let tail = if marker.is_empty() { Some(answer) } else { answer.rfind(marker).map(|i| &answer[i..]) };
    tail.is_some_and(|t| {
        t.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.strip_prefix("status:").is_some_and(|v| v.trim().starts_with("done"))
        })
    })
}

/// A read-only session for a decision on `agent`'s harness: the account it
/// ran on, what it cost, and its answer or why there is none.
pub fn consult(
    ctx: &Ctx,
    job: &Job,
    agent: &decide::Agent,
    prompt: &str,
) -> (Option<String>, f64, Result<String, String>) {
    let Some(home) = checker_home(ctx, job, &agent.harness, agent.model.as_deref()) else {
        return (None, 0.0, Err(format!("no {} account is free for agent:{}", agent.harness, agent.label)));
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
        let mut cmd = command(ctx, &home, &inv, None, &scratch, &ctx.cfg.repo);
        let run = Run {
            session_id: None,
            budget_usd: agent.budget_usd,
            log: "check.ndjson",
            stderr: "check-stderr.log",
            clone: None,
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

/// An account of `harness` that may take a session for `model`: another
/// than the worker's and idle where there is one, so no worker grades itself.
fn checker_home(ctx: &Ctx, job: &Job, harness: &str, model: Option<&str>) -> Option<Home> {
    let model = model.map(str::to_owned).or_else(|| ctx.cfg.harness(harness).model);
    let world = ctx.world().ok()?;
    let now = now_micros();
    let found = accounts::discover(&ctx.cfg);
    // The same judgment the dispatcher makes: a check never spends past an
    // account's floor or on a reading it cannot trust.
    let usable: Vec<&Home> = found
        .iter()
        .filter(|h| h.harness == harness)
        .filter(|h| accounts::fit(h, world.accounts.get(&h.key()), model.as_deref(), ctx.cfg.limits.floor, now).is_ok())
        .collect();
    let worker = |h: &&&Home| job.account.as_deref() == Some(h.key().as_str());
    let busy = |h: &&&Home| {
        world.jobs.values().any(|j| j.status == Status::Running && j.account.as_deref() == Some(h.key().as_str()))
    };
    // Another idle account, else the worker's own, whose seat its ended run
    // just freed; never a seat another job holds.
    usable.iter().find(|h| !worker(h) && !busy(h)).or_else(|| usable.iter().find(worker)).map(|h| (*h).clone())
}

/// The session to resume, when the job has one its history can follow here:
/// only on the same harness, which can carry it between its own accounts.
fn resumable(job: &Job, found: &[Home], home: &Home, scratch: &Path) -> Option<String> {
    let id = job.session_id.as_ref()?;
    let from = found.iter().find(|h| Some(h.key()) == job.transcript_account)?;
    (from.harness == home.harness && harness_of(home).carry(id, Some(from), home, scratch)).then(|| id.clone())
}

/// What the session is told first: a resumed one already has the brief.
fn opening(brief: &str, message: Option<&str>, resumed: bool, preamble: Option<&str>) -> String {
    let brief = preamble.map_or_else(|| brief.to_owned(), |p| format!("{p}\n\n{brief}"));
    match (message, resumed) {
        (Some(m), true) => m.to_owned(),
        (None, true) => "Continue where you left off.".into(),
        (Some(m), false) => format!("{brief}\n\n{m}"),
        (None, false) => brief,
    }
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
fn command(ctx: &Ctx, home: &Home, inv: &Invocation, workdir: Option<&Path>, scratch: &Path, cwd: &Path) -> Command {
    let mut argv: Vec<OsString> = vec!["nice".into(), "-n".into(), ctx.cfg.session.nice.to_string().into()];
    argv.push(inv.program.clone());
    argv.extend(inv.args.iter().cloned());
    let env = sandbox::session_env(ctx, inv);
    if ctx.cfg.sandbox.enabled {
        let mounts = Mounts { home, workdir, scratch, cwd };
        let mut c = Command::new("bwrap");
        c.args(sandbox::args(ctx, &accounts::home(), &mounts, env)).args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).env_clear().envs(env).current_dir(cwd);
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
    /// The clone to measure while it runs, for a build job's worker.
    clone: Option<&'a Path>,
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
    if let Some(dir) = run.clone.map(Path::to_path_buf) {
        let every = Duration::from_secs_f64(ctx.cfg.limits.disk_sample_minutes * 60.0);
        std::thread::spawn(move || {
            loop {
                if tx.send(Heard::Disk(os::dir_size(&dir, DISK_WALK).0)).is_err() {
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
        if j.readonly {
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
        let review = |n: &str, after: &[&str]| Job { readonly: true, workdir: None, branch: None, ..build(n, after) };
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

    fn rule<'a>(outcome: super::Outcome, f: impl FnOnce(&mut super::EndRule<'a>)) -> Option<super::Action> {
        let mut r = super::EndRule {
            outcome,
            final_done: false,
            silent: false,
            follow_ups: 0,
            check: None,
            answer: "",
            marker: "FINAL",
            reviewing: false,
            escalate: None,
        };
        f(&mut r);
        super::ended_rule(&r).map(|r| r.0)
    }

    #[test]
    fn the_end_rule_accepts_a_clean_done_and_resumes_one_silence() {
        use super::{Action, Outcome};
        assert_eq!(rule(Outcome::Done, |r| r.final_done = true), Some(Action::Accept));
        assert_eq!(rule(Outcome::Done, |_| {}), None, "no status: done, so another handler judges");
        let resumed = rule(Outcome::Failed, |r| r.silent = true);
        assert!(matches!(resumed, Some(Action::Resume { .. })), "{resumed:?}");
        assert_eq!(
            rule(Outcome::Failed, |r| {
                r.silent = true;
                r.follow_ups = 1;
            }),
            None,
            "silent twice"
        );
        assert_eq!(rule(Outcome::Failed, |_| {}), None, "an error is judged elsewhere");
    }

    fn ran(result: super::BarResult) -> super::CheckRun {
        super::CheckRun {
            command: "cargo test".into(),
            commit: Some("0123456789abcdef".into()),
            result,
            exit: 1,
            base_exit: None,
            tail: "test x ... FAILED".into(),
        }
    }

    #[test]
    fn a_red_bar_resumes_with_its_output_and_a_bar_that_proves_nothing_stops() {
        use super::{Action, BarResult, Outcome};
        let red = ran(BarResult::Fail);
        let a = rule(Outcome::Done, |r| {
            r.final_done = true;
            r.check = Some(&red);
        });
        let Some(Action::Resume { message, .. }) = a else { panic!("{a:?}") };
        assert!(
            message.contains("cargo test") && message.contains("FAILED") && message.contains("0123456789ab"),
            "{message}"
        );
        let nothing = ran(BarResult::CheckProvesNothing);
        let a = rule(Outcome::Done, |r| {
            r.final_done = true;
            r.check = Some(&nothing);
        });
        let Some(Action::Stop { why }) = a else { panic!("{a:?}") };
        assert!(why.starts_with("check-proves-nothing"), "{why}");
        let green = ran(BarResult::Pass);
        let a = rule(Outcome::Done, |r| {
            r.final_done = true;
            r.check = Some(&green);
        });
        assert_eq!(a, Some(Action::Accept), "green and reported done");
    }

    #[test]
    fn failed_ends_escalate_the_profile_when_enough_have_failed() {
        use super::{Action, Outcome};
        let at = |failed| rule(Outcome::Failed, |r| r.escalate = Some(("strong", 2, failed)));
        assert_eq!(at(1), None);
        assert_eq!(at(2), Some(Action::Requeue { profile: "strong".into() }));
        assert_eq!(at(3), Some(Action::Requeue { profile: "strong".into() }));
        let first_silence = rule(Outcome::Failed, |r| {
            r.silent = true;
            r.escalate = Some(("strong", 1, 1));
        });
        assert!(matches!(first_silence, Some(Action::Resume { .. })), "a silence is resumed before it escalates");
    }

    #[test]
    fn a_review_gives_its_verdict_from_its_final_section() {
        use super::{Action, Judgement, Outcome, Reason};
        let reject = "looked\nFINAL\nstatus: done\nverdict: reject\nreason: test-failure\nnotes: parse() panics";
        let a = rule(Outcome::Done, |r| {
            r.reviewing = true;
            r.answer = reject;
        });
        let Some(Action::Review { verdict, reason, message }) = a else { panic!("{a:?}") };
        assert_eq!((verdict, reason), (Judgement::Reject, Some(Reason::TestFailure)));
        assert!(message.starts_with("FINAL") && message.contains("parse() panics"), "{message}");
        assert_eq!(super::review_verdict("FINAL\nverdict: Accept", "FINAL"), Some((Judgement::Accept, None)));
        assert_eq!(
            super::review_verdict("FINAL\n- verdict: reject\n- reason: vibes", "FINAL"),
            Some((Judgement::Reject, Some(Reason::Other))),
            "a reject whose reason is no code is `other`"
        );
        assert_eq!(
            super::review_verdict("verdict: accept\nFINAL\nstatus: done", "FINAL"),
            None,
            "only the final section"
        );
        assert_eq!(super::review_verdict("FINAL\nverdict: maybe", "FINAL"), None);
        let plain = rule(Outcome::Done, |r| {
            r.reviewing = true;
            r.final_done = true;
            r.answer = "FINAL\nstatus: done";
        });
        assert_eq!(plain, Some(Action::Accept), "a review that gives no verdict ends as any done run");
    }

    #[test]
    fn a_report_saying_done_is_recognised() {
        assert!(super::final_status_done("work\nFINAL\nstatus: done\ncommits: x", "FINAL"));
        assert!(!super::final_status_done("FINAL\nstatus: partial", "FINAL"));
        assert!(!super::final_status_done("status: done\nno marker", "FINAL"));
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
