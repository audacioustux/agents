//! session: runs one dispatched job on its account's harness and reports
//! what happened. Emits: cloned, started, identified, rate, limited,
//! signed-out, follow-up, ended. One process per running job, started by
//! dispatch. Nothing here names a harness: the account's harness builds the
//! command and reads the output, and the limits are enforced here for all.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::accounts::{self, harness_of};
use crate::bus::{Outcome, SessionEvent, now_micros, secs};
use crate::ctx::Ctx;
use crate::harness::report::{Ending, Report, Verdict, verdict};
use crate::harness::{Home, Invocation, Permissions, Reader, Session, Task};
use crate::sandbox::{self, Mounts};
use crate::world::Job;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run a dispatched job. Dispatch starts this; it is not for direct use.
    Run { name: String },
}

pub fn run(ctx: &Ctx, Cmd::Run { name }: Cmd) -> Result<()> {
    Ctx::wait_for_gate();
    let port = ctx.port::<SessionEvent>();
    let job = ctx.world()?.jobs.remove(&name).with_context(|| format!("no job named {name}"))?;
    let mut spend = (0.0, 0);
    let (outcome, reason) =
        attempt(ctx, &job, &mut spend).unwrap_or_else(|e| (Outcome::Failed, Some(format!("{e:#}"))));
    port.emit(SessionEvent::Ended { job: name, outcome, cost_usd: spend.0, turns: spend.1, reason })?;
    ctx.wake_dispatch()
}

type Ended = (Outcome, Option<String>);

fn workspace_note(ctx: &Ctx, job: &Job) -> String {
    if job.readonly {
        "The repository is mounted read-only. Report; do not try to change files.".to_owned()
    } else {
        let branch = job.branch.clone().unwrap_or_else(|| format!("{}{}", ctx.cfg.session.branch_prefix, job.name));
        format!(
            "You are on branch {branch} in a private clone. Commit your work there with explicit pathspecs. Never push."
        )
    }
}

fn attempt(ctx: &Ctx, job: &Job, spend: &mut (f64, u32)) -> Result<Ended> {
    let port = ctx.port::<SessionEvent>();
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
        let (dir, branch) = clone(ctx, &job.name)?;
        port.emit(SessionEvent::Cloned { job: job.name.clone(), workdir: dir.clone(), branch })?;
        workdir = Some(dir);
    }
    let cwd = if job.readonly { ctx.cfg.repo.clone() } else { workdir.clone().unwrap_or_default() };
    let scratch = ctx.job_scratch(&job.name);
    std::fs::create_dir_all(&scratch)?;

    let resumed = resumable(job, &found, home, &scratch);
    let session_id = resumed.clone().unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let brief = std::fs::read_to_string(ctx.job_file(&job.name, "prompt.md")).context("the job's brief is gone")?;
    let prompt = opening(brief, job.message.as_deref(), resumed.is_some());
    port.emit(SessionEvent::Started { job: job.name.clone(), account: key.clone(), session_id: session_id.clone() })?;

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
        session: if resumed.is_some() { Session::Resume(&session_id) } else { Session::New(&session_id) },
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
    inv.write_files().context("writing the prompt file")?;
    let mut cmd = command(ctx, home, &inv, workdir.as_deref(), &scratch, &cwd);

    // An earlier attempt's answer must not pass for this one's.
    let _ = std::fs::remove_file(ctx.job_file(&job.name, "result.md"));
    let (report, ending) = drive(ctx, job, &key, &session_id, &mut cmd, inv.stdin.as_deref(), harness.reader())?;
    if !report.answer.is_empty() {
        std::fs::write(ctx.job_file(&job.name, "result.md"), format!("{}\n", report.answer))?;
    }
    *spend = (report.cost_usd, report.turns);
    conclude(ctx, job, &key, verdict(&report, ending, &s.final_marker, now_s()), now_s())
}

/// The session to resume, when the job has one its history can follow here:
/// only on the same harness, which can carry it between its own accounts.
fn resumable(job: &Job, found: &[Home], home: &Home, scratch: &Path) -> Option<String> {
    let id = job.session_id.as_ref()?;
    let from = found.iter().find(|h| Some(h.key()) == job.transcript_account)?;
    (from.harness == home.harness && harness_of(home).carry(id, Some(from), home, scratch)).then(|| id.clone())
}

/// What the session is told first: a resumed one already has the brief.
fn opening(brief: String, message: Option<&str>, resumed: bool) -> String {
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
    let port = ctx.port::<SessionEvent>();
    let s = &ctx.cfg.session;
    Ok(match v {
        Verdict::Done => (Outcome::Done, None),
        Verdict::FollowUp(message) if job.follow_ups == 0 => {
            port.emit(SessionEvent::FollowUp { job: job.name.clone(), message })?;
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

/// Runs the agent, feeding its stdin and teeing its stream to the job's log.
/// Ends it if it goes silent, runs past its time limit or spends past its
/// cap: the same limits for every harness, whatever the CLI enforces itself.
fn drive(
    ctx: &Ctx,
    job: &Job,
    account: &str,
    session_id: &str,
    cmd: &mut Command,
    stdin: Option<&str>,
    mut reader: Box<dyn Reader>,
) -> Result<(Report, Ending)> {
    let port = ctx.port::<SessionEvent>();
    let mut log = Ctx::append_private(&ctx.job_file(&job.name, "log.ndjson"))?;
    let mut child = cmd
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Ctx::append_private(&ctx.job_file(&job.name, "stderr.log"))?)
        .spawn()
        .context("starting the session")?;
    if let (Some(text), Some(mut pipe)) = (stdin.map(str::to_owned), child.stdin.take()) {
        // A writer thread, so a full pipe cannot block reading the output.
        std::thread::spawn(move || pipe.write_all(text.as_bytes()));
    }
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let silent_after = Duration::from_secs(ctx.cfg.limits.silent_minutes * 60);
    let deadline = Instant::now() + Duration::from_secs_f64(ctx.cfg.session.max_hours * 3600.0);
    let mut ending = Ending::default();
    let mut named = session_id.to_owned();
    loop {
        let wait = silent_after.min(deadline.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Ok(line)) => {
                writeln!(log, "{line}")?;
                if let Some(meters) = reader.line(&line) {
                    port.emit(SessionEvent::Rate { account: account.to_owned(), meters })?;
                }
                let r = reader.report();
                if let Some(id) = r.session_id.as_deref().filter(|id| *id != named) {
                    port.emit(SessionEvent::Identified { job: job.name.clone(), session_id: id.to_owned() })?;
                    id.clone_into(&mut named);
                }
                if r.cost_usd > job.budget_usd {
                    ending.over_budget = true;
                    let _ = child.kill();
                    break;
                }
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
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
    Ok((reader.report().clone(), ending))
}

/// A private clone that borrows the repository's objects (`--shared`), so it is
/// small and works across filesystems, on its own branch, unable to push.
fn clone(ctx: &Ctx, name: &str) -> Result<(PathBuf, String)> {
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
    let made = (|| {
        ok(Command::new("git").args(["clone", "--quiet", "--shared"]).arg(&ctx.cfg.repo).arg(&dir), "git clone")?;
        ok(Ctx::git(&dir).args(["switch", "--quiet", "-c", &branch]), "git switch")?;
        ok(Ctx::git(&dir).args(["remote", "set-url", "--push", "origin", "relay-jobs-do-not-push"]), "git remote")
    })();
    if made.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    made.map(|()| (dir, branch))
}
