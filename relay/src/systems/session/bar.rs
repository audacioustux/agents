//! session: a job's acceptance check, run on the committed tip of its branch.
//!
//! The check judges what would land, so it runs on a clean checkout of the branch's `HEAD`, never on
//! the working tree a session left behind: uncommitted and untracked files are not part of the work.
//! The checkout is a fresh clone with a configuration of its own. The check runs in it inside the
//! session's sandbox plan, with the clone and the session's account out of sight, and a development
//! environment entered from the project's repository, not from files the session could have changed.
//!
//! Its working directory (`<job>.bar.d`: the checkout and its `/tmp`) is made fresh for each run, outside
//! anything the session could write, and its output goes to the job's inbox, which the check cannot write.
//! A session that planted a symlink in its own directories has nothing relay follows.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::{command, layer_env, port};
use crate::bus::{BarResult, SessionEvent};
use crate::ctx::{Ctx, SessionRepo};
use crate::harness::{Home, Invocation};
use crate::hostdir::{self, HostDir};
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
    /// The failing tests its whole output names, read before the output is cut to its end.
    failing: Vec<String>,
}

/// Runs the job's check on its branch tip (and, for a revert control, at its base), and records
/// `bar-checked` for the commit it read, which is read once and checked as it was.
pub(super) fn run(ctx: &Ctx, job: &Job, home: &Home, dir: &SessionRepo, inbox: &HostDir) -> Result<CheckRun> {
    let command = job.check().context("a check was asked of a job that declares none")?.to_owned();
    let tip = dir.rev("HEAD");
    let place = Place { inbox, work: &ctx.job_scratch(&format!("{}.bar", job.name)) };
    let head = match &tip {
        Some(tip) => at(ctx, home, &command, dir, tip, &place),
        None => broke("the clone has no commit to check"),
    };
    let red_first = matches!(job.acceptance, crate::bus::Acceptance::CheckRedFirst(_));
    let (mut base_exit, mut text, mut failing) = (None, head.text.clone(), Vec::new());
    let result = match head.said {
        Said::Broke => BarResult::CheckCouldNotRun,
        // A revert control expects the base to fail: its failure there is the point, not an excuse.
        Said::Failed if red_first => BarResult::Fail,
        Said::Failed => {
            let base = at_base(ctx, job, home, &command, dir, &place);
            base_exit = Some(base.code);
            match base.said {
                Said::Passed => BarResult::Fail,
                Said::Failed => match base_red(&head.failing, &base.failing) {
                    Some(tests) => {
                        text = format!("{text}\n(the same check also failed at the base commit)");
                        failing = tests;
                        BarResult::BaseRed
                    }
                    None => BarResult::Fail,
                },
                Said::Broke => {
                    text = format!("the check could not run at the base commit: {}", base.text);
                    BarResult::CheckCouldNotRun
                }
            }
        }
        Said::Passed if !red_first => BarResult::Pass,
        Said::Passed => {
            let base = at_base(ctx, job, home, &command, dir, &place);
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
        failing: failing.clone(),
    };
    port(ctx, job).emit(event)?;
    Ok(CheckRun { command, commit: tip, result, exit: head.code, base_exit, failing, tail })
}

/// Whether a check that failed at the tip and at the base, naming the failing tests `tip` and `base`,
/// failed for the base's reason, and if so the tip's failing tests. Both naming tests: every test
/// failing at the tip fails at the base too. Neither naming any: the failure cannot be compared, so
/// failing at the base is enough. Only one naming tests: they failed differently, so it is the job's.
fn base_red(tip: &[String], base: &[String]) -> Option<Vec<String>> {
    match (tip.is_empty(), base.is_empty()) {
        (true, true) => Some(Vec::new()),
        (false, false) => tip.iter().all(|t| base.contains(t)).then(|| tip.to_vec()),
        _ => None,
    }
}

/// The failing tests a whole `cargo test` output names, in the order first named: its
/// `test NAME ... FAILED` lines and the names indented under a `failures:` line. Names are kept whole,
/// spaces and all (a doctest's is `src/lib.rs - f (line 3)`). The `---- NAME stdout ----` headers are
/// not read: `--show-output` prints them for passing tests too, under `successes:`.
fn failing_tests(output: impl std::io::BufRead) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut listing = false;
    for line in output.split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches('\r');
        let name = if line == "failures:" {
            listing = true;
            None
        } else if let Some(n) = line.strip_prefix("test ").and_then(|l| l.strip_suffix(" ... FAILED")) {
            Some(n)
        } else if listing && line.starts_with("    ") {
            Some(line.trim())
        } else {
            listing &= line.trim().is_empty();
            None
        };
        if let Some(n) = name.filter(|n| !n.is_empty())
            && !names.iter().any(|m| m == n)
        {
            names.push(n.to_owned());
        }
    }
    names
}

/// The check at the commit the branch started from.
fn at_base(ctx: &Ctx, job: &Job, home: &Home, command: &str, dir: &SessionRepo, place: &Place<'_>) -> Ran {
    match job.base.clone().or_else(|| checkpoint::base(dir, &[]).head) {
        Some(base) => at(ctx, home, command, dir, &base, place),
        None => broke("no base commit is known for the revert control"),
    }
}

fn broke(why: &str) -> Ran {
    Ran { said: Said::Broke, code: -1, text: why.to_owned(), failing: Vec::new() }
}

/// Where a check runs: its working directory, made fresh for each run in a place only relay writes,
/// and the job's inbox, where its output goes.
struct Place<'a> {
    inbox: &'a HostDir,
    work: &'a Path,
}

/// Runs `command` on a clean checkout of `rev` from the clone `source`.
fn at(ctx: &Ctx, home: &Home, command: &str, source: &SessionRepo, rev: &str, place: &Place<'_>) -> Ran {
    let tree = place.work.join("tree");
    let ran =
        match hostdir::fresh_dir(place.work).map_err(anyhow::Error::from).and_then(|()| checkout(source, rev, &tree)) {
            Ok(()) => exec(ctx, home, command, &tree, place),
            Err(e) => broke(&format!("could not check out {rev} for the check: {e:#}")),
        };
    let _ = std::fs::remove_dir_all(place.work);
    ran
}

/// A fresh clone of `source` at `rev` in `into`: its history, its objects, none of its configuration,
/// hooks or working tree. The tree checked out is the session's work, attributes and all, so it is a
/// [`SessionRepo`] too.
fn checkout(source: &SessionRepo, rev: &str, into: &Path) -> Result<()> {
    let git = |mut c: Command, what: &str| -> Result<()> {
        let out = c.output().with_context(|| what.to_owned())?;
        anyhow::ensure!(out.status.success(), "{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
        Ok(())
    };
    let mut clone = source.git();
    clone.args(["clone", "--quiet", "--local", "--no-checkout"]).arg(source.path()).arg(into);
    git(clone, "git clone")?;
    let mut switch = SessionRepo::new(into).git();
    switch.args(["checkout", "--quiet", "--detach", rev]);
    git(switch, "git checkout")
}

/// Runs `sh -c command` in `tree`, bounded by `hooks.command_timeout_seconds` (killed 5 s later if it
/// ignores the TERM), in the session's sandbox plan without the session's account or clone.
fn exec(ctx: &Ctx, home: &Home, command: &str, tree: &Path, place: &Place<'_>) -> Ran {
    let go = || -> Result<Ran> {
        let mut inv = Invocation::new("timeout");
        let secs = ctx.cfg.hooks.command_timeout_seconds.max(1).to_string();
        inv.args = ["-k", "5", &secs, "sh", "-c", command].map(OsString::from).to_vec();
        // The project's own development environment, entered where the session cannot have edited it.
        layer_env(ctx, &mut inv, &ctx.cfg.repo)?;
        let file = place.inbox.create("check.out")?;
        let tmp = place.work.join("tmp");
        std::fs::create_dir(&tmp)?;
        let mounts =
            Mounts { home, workdir: None, scratch: place.work, inbox: None, tmp: &tmp, cwd: tree, account: false };
        let mut c = self::command(ctx, &inv, &mounts);
        c.stdin(Stdio::null()).stdout(file.try_clone()?).stderr(file);
        let status = c.status().context("running the acceptance check")?;
        let text = place.inbox.read("check.out").map(|f| tail_text(f, 2 * TAIL as u64)).unwrap_or_default();
        let code = status.code();
        let failing = match said(code) {
            Said::Failed => place.inbox.read("check.out").map(|f| failing_tests(std::io::BufReader::new(f))),
            _ => Ok(Vec::new()),
        }
        .unwrap_or_default();
        let _ = place.inbox.remove("check.out");
        Ok(Ran { said: said(code), code: code.unwrap_or(-1), text, failing })
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
fn tail_text(mut f: std::fs::File, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
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

    const RED: &str = "running 3 tests\ntest a::one ... ok\ntest a::two ... FAILED\n\nfailures:\n\n\
        ---- a::two stdout ----\nthread 'a::two' panicked at src/a.rs:9:5:\n  left: 17\n\n\
        failures:\n    a::two\n\ntest result: FAILED. 1 passed; 1 failed\n";

    fn names(output: &str) -> Vec<String> {
        failing_tests(output.as_bytes())
    }

    #[test]
    fn a_cargo_test_output_names_its_failing_tests() {
        assert_eq!(names(RED), ["a::two"]);
        assert_eq!(names(&RED.replace('\n', "\r\n")), ["a::two"], "CRLF line ends");
        assert_eq!(names("Diff in src/a.rs at line 3:\nerror: could not compile"), Vec::<String>::new());
    }

    #[test]
    fn a_passing_test_s_output_is_not_a_failure() {
        let shown =
            format!("successes:\n\n---- a::one stdout ----\nhi\n    indented\n\nsuccesses:\n    a::one\n\n{RED}");
        assert_eq!(names(&shown), ["a::two"], "--show-output prints passing tests' headers and list");
    }

    #[test]
    fn a_doctest_keeps_its_whole_name() {
        let doc = "test src/lib.rs - f (line 3) ... FAILED\n\nfailures:\n    src/lib.rs - f (line 3)\n";
        assert_eq!(names(doc), ["src/lib.rs - f (line 3)"]);
        let other = doc.replace("line 3", "line 9");
        assert_eq!(base_red(&names(&other), &names(doc)), None, "a different doctest failing is the job's");
    }

    #[test]
    fn a_failure_is_the_base_s_when_every_failing_test_fails_there_too() {
        let (red, both) = (names(RED), names(&RED.replace("    a::two\n", "    a::two\n    a::new\n")));
        assert_eq!(base_red(&red, &both), Some(vec!["a::two".to_owned()]), "fewer fail at the tip");
        assert_eq!(base_red(&both, &red), None, "a new failing test is the job's");
        assert_eq!(base_red(&[], &[]), Some(vec![]), "nothing to compare: the base fails too");
        assert_eq!(base_red(&[], &red), None, "the tip fails before the tests the base fails");
    }

    #[test]
    fn the_tail_keeps_the_end() {
        assert_eq!(tail_of("short", 10), "short");
        let t = tail_of("0123456789", 4);
        assert!(t.ends_with("6789") && t.contains("6 earlier bytes"), "{t}");
        assert!(tail_of("ééééé", 3).ends_with('é'), "never mid-character");
    }
}
