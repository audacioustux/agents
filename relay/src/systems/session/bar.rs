//! session: a job's acceptance check, run on the committed tip of its branch.
//!
//! The check judges what would land, so it runs on a clean checkout of the branch's `HEAD`, never on
//! the working tree a session left behind: uncommitted and untracked files are not part of the work.
//! The checkout is a fresh clone with a configuration of its own. The check runs in it inside the
//! session's sandbox plan, with the clone and the session's account out of sight, and a development
//! environment entered from the project's repository, not from files the session could have changed.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::{checkpoint_rev, command, layer_env, port};
use crate::bus::{BarResult, SessionEvent};
use crate::ctx::Ctx;
use crate::harness::{Home, Invocation};
use crate::sandbox::Mounts;
use crate::systems::checkpoint;
use crate::systems::decide::CheckRun;
use crate::world::Job;

/// How long the tail of a check's output may be.
const TAIL: usize = 4096;

/// What a command run said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    Passed,
    /// It ran and failed: the code it was run on is not good enough.
    Failed,
    /// It said nothing about the code: it timed out, was not found or runnable, or was killed.
    Broke,
}

/// What an exit status says. The shell's own codes (125-127), the timeout's (124) and a kill (137,
/// 143, or no code at all) mean the command did not get to judge anything.
fn said(code: Option<i32>) -> Said {
    match code {
        Some(0) => Said::Passed,
        Some(124..=127 | 137 | 143) | None => Said::Broke,
        Some(_) => Said::Failed,
    }
}

/// A command run at one commit.
struct Ran {
    said: Said,
    /// Its exit status, -1 when it had none.
    code: i32,
    /// The end of its output, or why it could not run.
    text: String,
}

/// Runs the job's check on its branch tip (and, for a revert control, at its base), and records
/// `bar-checked` for the commit it read, which is read once and checked as it was.
pub(super) fn run(ctx: &Ctx, job: &Job, home: &Home, dir: &Path, scratch: &Path) -> Result<CheckRun> {
    let command = job.check().context("a check was asked of a job that declares none")?.to_owned();
    let tip = checkpoint_rev(dir, "HEAD");
    let head = match &tip {
        Some(tip) => at(ctx, home, &command, dir, tip, scratch),
        None => broke("the clone has no commit to check"),
    };
    let red_first = matches!(job.acceptance, crate::bus::Acceptance::CheckRedFirst(_));
    let (mut base_exit, mut text) = (None, head.text.clone());
    let result = match head.said {
        Said::Broke => BarResult::CheckCouldNotRun,
        Said::Failed => BarResult::Fail,
        Said::Passed if !red_first => BarResult::Pass,
        Said::Passed => {
            let base = at_base(ctx, job, home, &command, dir, scratch);
            base_exit = Some(base.code);
            match base.said {
                Said::Failed => BarResult::Pass,
                Said::Passed => {
                    text = format!("{text}\n(the same check also passed at the base commit)");
                    BarResult::CheckProvesNothing
                }
                Said::Broke => {
                    text = format!("the check could not run at the base commit: {}", base.text);
                    BarResult::CheckCouldNotRun
                }
            }
        }
    };
    let tail = checkpoint::bounded(&tail_of(text.trim_end(), TAIL), TAIL + 64);
    let event = SessionEvent::BarChecked {
        job: job.name.clone(),
        commit: tip.clone(),
        exit: head.code,
        tail: tail.clone(),
        base_exit,
        result,
    };
    port(ctx, job).emit(event)?;
    Ok(CheckRun { command, commit: tip, result, exit: head.code, base_exit, tail })
}

/// The check at the commit the branch started from.
fn at_base(ctx: &Ctx, job: &Job, home: &Home, command: &str, dir: &Path, scratch: &Path) -> Ran {
    match job.base.clone().or_else(|| checkpoint::base(dir, &[]).head) {
        Some(base) => at(ctx, home, command, dir, &base, scratch),
        None => broke("no base commit is known for the revert control"),
    }
}

fn broke(why: &str) -> Ran {
    Ran { said: Said::Broke, code: -1, text: why.to_owned() }
}

/// Runs `command` on a clean checkout of `rev` from the clone `source`.
fn at(ctx: &Ctx, home: &Home, command: &str, source: &Path, rev: &str, scratch: &Path) -> Ran {
    let tree = scratch.join("check-tree");
    let _ = std::fs::remove_dir_all(&tree);
    let ran = match checkout(source, rev, &tree) {
        Ok(()) => exec(ctx, home, command, &tree, scratch),
        Err(e) => broke(&format!("could not check out {rev} for the check: {e:#}")),
    };
    let _ = std::fs::remove_dir_all(&tree);
    ran
}

/// A fresh clone of `source` at `rev` in `into`: its history, its objects, none of its configuration,
/// hooks or working tree.
fn checkout(source: &Path, rev: &str, into: &Path) -> Result<()> {
    let git = |mut c: Command, what: &str| -> Result<()> {
        let out = c.output().with_context(|| what.to_owned())?;
        anyhow::ensure!(out.status.success(), "{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
        Ok(())
    };
    let mut clone = Ctx::git_untrusted(source);
    clone.args(["clone", "--quiet", "--local", "--no-checkout"]).arg(source).arg(into);
    git(clone, "git clone")?;
    let mut switch = Ctx::git_untrusted(into);
    switch.args(["checkout", "--quiet", "--detach", rev]);
    git(switch, "git checkout")
}

/// Runs `sh -c command` in `tree`, bounded by `hooks.command_timeout_seconds` (killed 5 s later if it
/// ignores the TERM), in the session's sandbox plan without the session's account or clone.
fn exec(ctx: &Ctx, home: &Home, command: &str, tree: &Path, scratch: &Path) -> Ran {
    let go = || -> Result<Ran> {
        let mut inv = Invocation::new("timeout");
        let secs = ctx.cfg.hooks.command_timeout_seconds.max(1).to_string();
        inv.args = ["-k", "5", &secs, "sh", "-c", command].map(OsString::from).to_vec();
        // The project's own development environment, entered where the session cannot have edited it.
        layer_env(ctx, &mut inv, &ctx.cfg.repo)?;
        let out = scratch.join("check.out");
        let file = std::fs::File::create(&out)?;
        let tmp = scratch.join("check-tmp");
        std::fs::create_dir_all(&tmp)?;
        let mounts = Mounts { home, workdir: None, scratch, tmp: &tmp, cwd: tree, account: false };
        let mut c = self::command(ctx, &inv, &mounts);
        c.stdin(Stdio::null()).stdout(file.try_clone()?).stderr(file);
        let status = c.status().context("running the acceptance check")?;
        let text = tail_text(&out, 2 * TAIL as u64);
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&tmp);
        let code = status.code();
        Ok(Ran { said: said(code), code: code.unwrap_or(-1), text })
    };
    go().unwrap_or_else(|e| broke(&format!("the acceptance check could not run: {e:#}")))
}

/// The end of `text`, at most `max` bytes, saying how much came before.
fn tail_of(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let cut = (text.len() - max..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
    format!("… ({cut} earlier bytes)\n{}", &text[cut..])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_command_that_ran_and_failed_has_failed() {
        assert_eq!(said(Some(0)), Said::Passed);
        assert_eq!(said(Some(1)), Said::Failed);
        assert_eq!(said(Some(101)), Said::Failed, "a test runner's own failure code");
        for broken in [Some(124), Some(125), Some(126), Some(127), Some(137), Some(143), None] {
            assert_eq!(said(broken), Said::Broke, "{broken:?}");
        }
    }

    #[test]
    fn the_tail_keeps_the_end() {
        assert_eq!(tail_of("short", 10), "short");
        let t = tail_of("0123456789", 4);
        assert!(t.ends_with("6789") && t.contains("6 earlier bytes"), "{t}");
        assert!(tail_of("ééééé", 3).ends_with('é'), "never mid-character");
    }
}
