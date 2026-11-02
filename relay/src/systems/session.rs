//! session: runs one dispatched job on its account's harness and reports
//! what happened. Emits: cloned, started, identified, rate, limited,
//! signed-out, follow-up, checking, checked, ended. One process per running job, started by
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
use crate::bus::{Check, CheckVerdict, Outcome, SessionEvent, now_micros, secs};
use crate::ctx::Ctx;
use crate::harness::report::{Ending, Report, Verdict, verdict};
use crate::harness::{Home, Invocation, Permissions, Reader, Session, Task};
use crate::sandbox::{self, Mounts};
use crate::systems::checkpoint::{self, Next};
use crate::world::{Job, Status};

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
    let prompt = opening(brief.clone(), job.message.as_deref(), resumed.is_some());
    // What a read-only job reads is the live repository, so its commit is taken now, not when queued.
    let commit = if job.readonly { Ctx::clean_head(&ctx.cfg.repo) } else { None };
    port.emit(SessionEvent::Started {
        job: job.name.clone(),
        account: key.clone(),
        session_id: session_id.clone(),
        commit,
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
    // The development environment underneath; the harness's own variables (the account) win.
    let deny = regex::Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    let dev = dev_env(&ctx.cfg.session.env_shell, &deny, &cwd)?;
    inv.env = dev.into_iter().chain(std::mem::take(&mut inv.env)).collect();
    inv.write_files().context("writing the prompt file")?;
    let mut cmd = command(ctx, home, &inv, workdir.as_deref(), &scratch, &cwd);

    // An earlier attempt's answer must not pass for this one's.
    let _ = std::fs::remove_file(ctx.job_file(&job.name, "result.md"));
    let run =
        Run { session_id: Some(&session_id), budget_usd: job.budget_usd, log: "log.ndjson", stderr: "stderr.log" };
    let (report, ending) = drive(ctx, job, &key, &run, &mut cmd, inv.stdin.as_deref(), harness.reader())?;
    if !report.answer.is_empty() {
        std::fs::write(ctx.job_file(&job.name, "result.md"), format!("{}\n", report.answer))?;
    }
    *spend = (report.cost_usd, report.turns);
    match verdict(&report, ending, &s.final_marker, job.readonly, now_s()) {
        Verdict::Budget => at_cap(ctx, job, &report, workdir.as_deref(), &scratch, &brief),
        v => conclude(ctx, job, &key, v, now_s()),
    }
}

/// A run at its cap: the owner decides, or its progress is checked and it
/// goes on with a larger cap. No change since the last checkpoint is a stop
/// that asks no model.
fn at_cap(ctx: &Ctx, job: &Job, report: &Report, dir: Option<&Path>, scratch: &Path, brief: &str) -> Result<Ended> {
    let cap = match checkpoint::next(&ctx.cfg.limits, job.readonly, job.budget_usd, job.cost_usd + report.cost_usd) {
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
    let dir = dir.context("a build job without its clone")?;
    let port = port(ctx, job);
    let n = u32::try_from(job.checkpoints.len()).unwrap_or(u32::MAX).saturating_add(1);
    let from = checkpoint::base(dir, &job.checkpoints);
    let now = checkpoint::point(dir, &scratch.join("checkpoint.index"));
    let mut check = Check {
        checkpoint: n,
        verdict: CheckVerdict::Stop,
        why: "no new commit and no changed file since the last checkpoint".into(),
        message: String::new(),
        next_cap_usd: None,
        head: now.head.clone(),
        tree: now.tree.clone(),
        checker: None,
        cost_usd: 0.0,
    };
    if checkpoint::progressed(&from, &now) {
        port.emit(SessionEvent::Checking { job: job.name.clone(), checkpoint: n })?;
        let prompt =
            checkpoint::prompt(brief, &job.checkpoints, &checkpoint::evidence(dir, &from, &now), &report.answer);
        let (checker, cost, answer) = judge(ctx, job, &prompt);
        (check.checker, check.cost_usd) = (checker, cost);
        match answer {
            Ok(a) => {
                if a.verdict != CheckVerdict::Stop {
                    check.message = checkpoint::resume_message(n, cap, &a);
                    check.next_cap_usd = Some(cap);
                }
                (check.verdict, check.why) = (a.verdict, a.why);
            }
            Err(why) => check.why = why,
        }
    }
    let waits = check.next_cap_usd.is_none();
    port.emit(SessionEvent::Checked { job: job.name.clone(), check })?;
    Ok(if waits {
        (Outcome::Failed, Some("waits for a human at its checkpoint; resume with --budget to go on".into()))
    } else {
        (Outcome::Retry, None)
    })
}

/// The progress check on the checker's harness, read-only: the account it
/// ran on, what it cost, and its answer or why there is none.
fn judge(ctx: &Ctx, job: &Job, prompt: &str) -> (Option<String>, f64, Result<checkpoint::Answer, String>) {
    let c = &ctx.cfg.limits.checker;
    let Some(home) = checker_home(ctx, job) else {
        return (None, 0.0, Err(format!("no {} account is free for the progress check", c.harness)));
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
            model: c.model.as_deref().or(conf.model.as_deref()),
            effort: None,
            session: Session::New(&id),
            permissions: permissions(ctx.cfg.sandbox.enabled, true),
            briefing: Some("The repository is mounted read-only. Report; do not try to change files."),
            budget_usd: Some(c.budget_usd),
            name: Some(&name),
            events: true,
            lean: true,
            scratch: &scratch,
        };
        let mut inv = conf.locate(harness.invocation(&home, &task).map_err(|r| anyhow::anyhow!(r.0))?);
        inv.args.extend(conf.extra_args.iter().map(OsString::from));
        inv.write_files().context("writing the prompt file")?;
        let mut cmd = command(ctx, &home, &inv, None, &scratch, &ctx.cfg.repo);
        let run = Run { session_id: None, budget_usd: c.budget_usd, log: "check.ndjson", stderr: "check-stderr.log" };
        drive(ctx, job, &home.key(), &run, &mut cmd, inv.stdin.as_deref(), harness.reader())
    };
    let (report, ending) = match run() {
        Ok(r) => r,
        Err(e) => return (Some(home.key()), 0.0, Err(format!("the progress check did not run: {e:#}"))),
    };
    let answer = match verdict(&report, ending, "", true, now_s()) {
        Verdict::Done => checkpoint::parse(&report.answer).ok_or_else(|| "the progress check gave no verdict".into()),
        v => Err(format!("the progress check did not finish: {v:?}")),
    };
    (Some(home.key()), report.cost_usd, answer)
}

/// An account of the checker's harness that may take a session: another
/// than the worker's and idle where there is one, so no worker grades itself.
fn checker_home(ctx: &Ctx, job: &Job) -> Option<Home> {
    let c = &ctx.cfg.limits.checker;
    let model = c.model.clone().or_else(|| ctx.cfg.harness(&c.harness).model);
    let world = ctx.world().ok()?;
    let now = now_micros();
    let found = accounts::discover(&ctx.cfg);
    // The same judgment the dispatcher makes: a check never spends past an
    // account's floor or on a reading it cannot trust.
    let usable: Vec<&Home> = found
        .iter()
        .filter(|h| h.harness == c.harness.as_str())
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
    let port = port(ctx, job);
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

/// One run's own limits and files.
struct Run<'a> {
    /// The session whose renaming the harness reports, for the worker; none for a check.
    session_id: Option<&'a str>,
    budget_usd: f64,
    /// The job's files its stream and its stderr go to.
    log: &'a str,
    stderr: &'a str,
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
) -> Result<(Report, Ending)> {
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
    let mut named = run.session_id.map(str::to_owned);
    loop {
        let wait = silent_after.min(deadline.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Ok(line)) => {
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
