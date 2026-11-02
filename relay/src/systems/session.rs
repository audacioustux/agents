//! session: runs one dispatched job as a headless Claude session and reports
//! what happened. Emits: cloned, started, rate, limited, signed-out,
//! follow-up, ended. One process per running job, started by dispatch.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::accounts;
use crate::bus::{Outcome, SessionEvent, now_micros};
use crate::ctx::Ctx;
use crate::sandbox::{self, Mounts};
use crate::stream::{Ending, Observed, Verdict, verdict};
use crate::world::Job;
use harness_bridge::headless::{Headless, Permissions, Start};

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run a dispatched job. Dispatch starts this; it is not for direct use.
    Run { name: String },
}

pub fn run(ctx: &Ctx, Cmd::Run { name }: Cmd) -> Result<()> {
    let port = ctx.port::<SessionEvent>();
    let job = ctx.world()?.jobs.remove(&name).with_context(|| format!("no job named {name}"))?;
    let mut spend = (0.0, 0);
    let (outcome, reason) =
        attempt(ctx, &job, &mut spend).unwrap_or_else(|e| (Outcome::Failed, Some(format!("{e:#}"))));
    port.emit(SessionEvent::Ended { job: name, outcome, cost_usd: spend.0, turns: spend.1, reason })?;
    ctx.wake_dispatch()
}

type Ended = (Outcome, Option<String>);

fn attempt(ctx: &Ctx, job: &Job, spend: &mut (f64, u32)) -> Result<Ended> {
    let port = ctx.port::<SessionEvent>();
    let label = job.account.clone().context("dispatched without an account")?;
    let found = accounts::discover(&ctx.cfg);
    let Some(account) = found.iter().find(|a| a.label == label) else {
        return Ok((Outcome::Retry, Some(format!("account {label} is no longer available"))));
    };
    if let Some(reason) = job.model.as_deref().and_then(|m| ctx.cfg.session.banned(m)) {
        return Ok((Outcome::Failed, Some(format!("model banned: {reason}"))));
    }

    let mut workdir = job.workdir.clone();
    if !job.readonly && workdir.is_none() {
        let (dir, branch) = clone(ctx, &job.name)?;
        port.emit(SessionEvent::Cloned { job: job.name.clone(), workdir: dir.clone(), branch })?;
        workdir = Some(dir);
    }
    let cwd = if job.readonly { ctx.cfg.repo.clone() } else { workdir.clone().unwrap_or_default() };

    let from = job.transcript_account.as_ref().and_then(|l| found.iter().find(|a| &a.label == l));
    let resumable = job.transcript_account.is_some()
        && carry_transcript(&job.session_id, from.map(|a| a.dir.as_path()), &account.dir)?;
    let brief = std::fs::read_to_string(ctx.job_file(&job.name, "prompt.md")).context("the job's brief is gone")?;
    let prompt = match (&job.message, resumable) {
        (Some(m), true) => m.clone(),
        (None, true) => "Continue where you left off.".into(),
        (Some(m), false) => format!("{brief}\n\n{m}"),
        (None, false) => brief,
    };
    port.emit(SessionEvent::Started { job: job.name.clone(), account: label.clone() })?;

    let s = &ctx.cfg.session;
    let workspace = if job.readonly {
        "The repository is mounted read-only. Report; do not try to change files.".to_owned()
    } else {
        format!(
            "You are on branch {} in a private clone. Commit your work there with explicit pathspecs. Never push.",
            job.branch.as_deref().map_or_else(|| format!("{}{}", s.branch_prefix, job.name), str::to_owned)
        )
    };
    let briefing =
        s.briefing.replace("{workspace}", &workspace).replace("{branch}", job.branch.as_deref().unwrap_or(""));
    let permissions = match (ctx.cfg.sandbox.enabled, job.readonly) {
        (true, _) => Permissions::Unrestricted,
        (false, true) => Permissions::ReadOnly,
        (false, false) => Permissions::EditsOnly,
    };
    let headless = Headless {
        command: &s.command,
        start: if resumable { Start::Resume(&job.session_id) } else { Start::Fresh(&job.session_id) },
        model: job.model.as_deref(),
        effort: job.effort.as_deref(),
        budget_usd: job.budget_usd,
        briefing: &briefing,
        permissions,
        extra_args: &s.extra_args,
    };
    let mut argv: Vec<OsString> = vec!["nice".into(), "-n".into(), s.nice.to_string().into()];
    argv.extend(headless.argv());
    let mut cmd = if ctx.cfg.sandbox.enabled {
        let mounts = Mounts { account_dir: &account.dir, workdir: workdir.as_deref(), cwd: &cwd };
        let mut c = Command::new("bwrap");
        c.args(sandbox::args(ctx, &accounts::home(), &mounts, std::env::vars_os())).args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).env_clear().envs(sandbox::plain_env(ctx, &account.dir)).current_dir(&cwd);
        c
    };

    // An earlier attempt's answer must not pass for this one's.
    let _ = std::fs::remove_file(ctx.job_file(&job.name, "result.md"));
    let (obs, ending) = drive(ctx, &job.name, &mut cmd, &prompt)?;
    let answer = obs.answer();
    if !answer.is_empty() {
        std::fs::write(ctx.job_file(&job.name, "result.md"), format!("{answer}\n"))?;
    }
    *spend = (obs.cost_usd(), obs.turns());
    let now_s = i64::try_from(now_micros() / 1_000_000).unwrap_or(i64::MAX);
    Ok(match verdict(&obs, ending, &s.final_marker, now_s) {
        Verdict::Done => (Outcome::Done, None),
        Verdict::FollowUp(message) if job.follow_ups == 0 => {
            port.emit(SessionEvent::FollowUp { job: job.name.clone(), message })?;
            (Outcome::Retry, Some("ended without its final report; asked once more".into()))
        }
        Verdict::FollowUp(_) => (Outcome::Done, Some(format!("no {} section after a follow-up", s.final_marker))),
        Verdict::Limited { until } => {
            // Two minutes after a stated reset; fifteen when none is stated (an overload or bare 429).
            let until = until.map_or(now_s + 15 * 60, |u| u + 120);
            port.emit(SessionEvent::Limited { account: label.clone(), until })?;
            (Outcome::Retry, Some(format!("{label} hit its limit; moving on")))
        }
        Verdict::SignedOut => {
            port.emit(SessionEvent::SignedOut { account: label.clone() })?;
            (Outcome::Retry, Some(format!("{label} is signed out; moving on")))
        }
        Verdict::NoOutput if job.reruns < ctx.cfg.limits.max_retries => {
            (Outcome::Rerun, Some("no output; trying again".into()))
        }
        Verdict::NoOutput => (Outcome::Failed, Some("no output, again".into())),
        Verdict::Budget => (Outcome::Failed, Some("reached its spend cap; resume with --budget to go on".into())),
        Verdict::Failed(why) => (Outcome::Failed, Some(why)),
    })
}

/// Runs the agent, feeding the prompt on stdin and teeing its stream to the
/// job's log. Ends it if it goes silent or runs past its time limit.
fn drive(ctx: &Ctx, name: &str, cmd: &mut Command, prompt: &str) -> Result<(Observed, Ending)> {
    let port = ctx.port::<SessionEvent>();
    let mut log = Ctx::append_private(&ctx.job_file(name, "log.ndjson"))?;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Ctx::append_private(&ctx.job_file(name, "stderr.log"))?)
        .spawn()
        .context("starting the session")?;
    let mut stdin = child.stdin.take().context("stdin")?;
    let prompt = prompt.to_owned();
    // A writer thread, so a full pipe cannot block reading the output.
    std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let limits = &ctx.cfg.limits;
    let silent_after = Duration::from_secs(limits.silent_minutes * 60);
    let deadline = Instant::now() + Duration::from_secs_f64(ctx.cfg.session.max_hours * 3600.0);
    let account = ctx.world()?.jobs.get(name).and_then(|j| j.account.clone()).unwrap_or_default();
    let mut obs = Observed::default();
    let (mut silent, mut timed_out) = (false, false);
    loop {
        let wait = silent_after.min(deadline.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Ok(line)) => {
                writeln!(log, "{line}")?;
                if let Some(w) = obs.line(&line) {
                    port.emit(SessionEvent::Rate { account: account.clone(), windows: w.clone() })?;
                }
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                (silent, timed_out) = if Instant::now() >= deadline { (false, true) } else { (true, false) };
                // bwrap's --die-with-parent and pid namespace take everything inside with it.
                let _ = child.kill();
                break;
            }
        }
    }
    let status = child.wait()?;
    Ok((obs, Ending { exit_ok: status.success(), timed_out, silent }))
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
    ok(Command::new("git").args(["clone", "--quiet", "--shared"]).arg(&ctx.cfg.repo).arg(&dir), "git clone")?;
    ok(Ctx::git(&dir).args(["switch", "--quiet", "-c", &branch]), "git switch")?;
    ok(Ctx::git(&dir).args(["remote", "set-url", "--push", "origin", "relay-jobs-do-not-push"]), "git remote")?;
    Ok((dir, branch))
}

/// A transcript is `<account>/projects/<cwd key>/<session>.jsonl`, plus a
/// `<session>/` sidecar dir when present. The cwd is the same path in every
/// sandbox, so the key is too, and copying the files moves the session.
/// Returns whether there is a transcript to resume.
fn carry_transcript(session: &str, from: Option<&Path>, to: &Path) -> Result<bool> {
    let find = |dir: &Path| -> Option<(PathBuf, PathBuf)> {
        let projects = dir.join("projects");
        std::fs::read_dir(&projects).ok()?.flatten().find_map(|e| {
            let file = e.path().join(format!("{session}.jsonl"));
            file.is_file().then(|| (e.path(), file))
        })
    };
    // The account that wrote last holds the newest copy.
    let Some((key_dir, file)) = from.filter(|f| *f != to).and_then(find) else { return Ok(find(to).is_some()) };
    let dest = to.join("projects").join(key_dir.file_name().context("project key")?);
    std::fs::create_dir_all(&dest)?;
    std::fs::copy(&file, dest.join(format!("{session}.jsonl")))?;
    let sidecar = key_dir.join(session);
    if sidecar.is_dir() {
        copy_dir(&sidecar, &dest.join(session))?;
    }
    Ok(true)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    #[test]
    fn transcript_moves_with_its_sidecar() {
        let d = Dir::new();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        let key = a.join("projects/-repo");
        std::fs::create_dir_all(key.join("s1/sub")).unwrap();
        std::fs::write(key.join("s1.jsonl"), "x").unwrap();
        std::fs::write(key.join("s1/sub/f"), "y").unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert!(!carry_transcript("s2", Some(&a), &b).unwrap());
        assert!(carry_transcript("s1", Some(&a), &b).unwrap());
        assert_eq!(std::fs::read_to_string(b.join("projects/-repo/s1.jsonl")).unwrap(), "x");
        assert_eq!(std::fs::read_to_string(b.join("projects/-repo/s1/sub/f")).unwrap(), "y");
        assert!(carry_transcript("s1", Some(&b), &b).unwrap(), "already here");
    }
}
