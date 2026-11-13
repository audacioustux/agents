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
use std::io::{BufRead, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use super::{Phase, clear, command, layer_env, port};
use crate::bus::{BarResult, SessionEvent};
use crate::ctx::{Ctx, SessionRepo};
use crate::harness::{Home, Invocation};
use crate::hostdir::{self, HostDir};
use crate::sandbox::Mounts;
use crate::systems::checkpoint;
use crate::systems::decide::CheckRun;
use crate::world::{Bar, Job};

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
fn said(code: Option<i32>, declared: &[i32]) -> Said {
    match code {
        Some(0) => Said::Passed,
        Some(124..=127 | 137 | 143) | None => Said::Broke,
        Some(c) if declared.contains(&c) => Said::Broke,
        Some(_) => Said::Failed,
    }
}

/// Why a command that said nothing about the code could not run, from what relay saw: its exit status
/// and the timeout it applied. Never from the command's output.
fn why(code: Option<i32>, declared: &[i32], timeout_secs: u64) -> String {
    match code {
        Some(124) => format!("it timed out (exit 124, after {timeout_secs} s: hooks.command_timeout_seconds)"),
        Some(125) => "the shell or timeout could not start it (exit 125)".to_owned(),
        Some(126) => "the command was found but is not runnable (exit 126)".to_owned(),
        Some(127) => "the command was not found (exit 127)".to_owned(),
        Some(c @ (137 | 143)) => format!("it was killed (exit {c})"),
        Some(c) if declared.contains(&c) => {
            format!("it exited {c}, which hooks.check_could_not_run_exit_codes declares as \"could not run\"")
        }
        Some(c) => format!("exit {c}"),
        None => "it was ended by a signal".to_owned(),
    }
}

/// The longest explanation a tail keeps whole.
const NOTE: usize = TAIL / 4;

/// The recorded tail: the `note` (why a check could not run, and where) whole, then the end of the
/// `output` in what is left of [`TAIL`]. Cutting one string from its start would drop the note first.
fn tail_for(note: &str, output: &str) -> String {
    let output = output.trim_end();
    if note.is_empty() {
        return checkpoint::bounded(&tail_of(output, TAIL), TAIL + 64);
    }
    let note = checkpoint::bounded(note, NOTE);
    if output.is_empty() {
        return note;
    }
    // Room for the note, the newline and the "… (N earlier bytes)" tail_of adds.
    let room = (TAIL + 64).saturating_sub(note.len() + 1 + 40).min(TAIL);
    format!("{note}\n{}", tail_of(output, room))
}

/// A command run at one commit.
struct Ran {
    said: Said,
    /// Its exit status, -1 when it had none.
    code: i32,
    /// The end of its output.
    text: String,
    /// Why it could not run, when it could not (from its exit status, or what relay could not do):
    /// kept whole in the record, never cut with the output.
    note: String,
    /// The failing tests its whole output names, read before the output is cut to its end; `None` when
    /// they could not all be read.
    failing: Option<Vec<String>>,
}

/// Runs the job's check on its branch tip (and, for a revert control, at its base), and records
/// `bar-checked` for the commit it read, which is read once and checked as it was.
pub(super) fn run(
    ctx: &Ctx,
    job: &Job,
    home: &Home,
    dir: &SessionRepo,
    base: Option<&str>,
    inbox: &HostDir,
    dev: &[(OsString, Option<OsString>)],
) -> Result<CheckRun> {
    let command = job.check().context("a check was asked of a job that declares none")?.to_owned();
    wait_for_memory(ctx, job);
    let tip = dir.rev("HEAD");
    let place = Place { job, inbox, work: &ctx.job_scratch(&format!("{}.bar", job.name)), dev };
    let head = match &tip {
        Some(tip) => at(ctx, home, &command, dir, tip, Phase::Check, &place),
        None => broke("the clone has no commit to check"),
    };
    let red_first = matches!(job.acceptance, crate::bus::Acceptance::CheckRedFirst(_));
    // The check before this one, as the record holds it: this one is compared with it.
    let before = ctx.world()?.jobs().get(&job.name).and_then(|j| j.bars.last().cloned());
    let (mut base_exit, mut text, mut note, mut failing) = (None, head.text.clone(), head.note.clone(), Vec::new());
    let result = match head.said {
        Said::Broke => BarResult::CheckCouldNotRun,
        // A revert control expects the base to fail: its failure there is the point, not an excuse.
        Said::Failed if red_first => BarResult::Fail,
        Said::Failed => {
            let at = at_base(ctx, base, home, &command, dir, &place);
            base_exit = Some(at.code);
            match at.said {
                Said::Passed => BarResult::Fail,
                Said::Failed => match base_red(head.failing.as_deref(), at.failing.as_deref()) {
                    Some(tests) => {
                        text = format!("{text}\n(the same check also failed at the base commit)");
                        failing = tests;
                        BarResult::BaseRed
                    }
                    None => BarResult::Fail,
                },
                Said::Broke => {
                    (text, note) = (at.text, format!("at the base commit, {}", at.note));
                    BarResult::CheckCouldNotRun
                }
            }
        }
        Said::Passed if !red_first => BarResult::Pass,
        Said::Passed => {
            let at = at_base(ctx, base, home, &command, dir, &place);
            base_exit = Some(at.code);
            match at.said {
                Said::Failed => BarResult::Pass,
                Said::Passed => {
                    text = format!("{text}\n(the same check also passed at the base commit)");
                    BarResult::CheckProvesNothing
                }
                Said::Broke => {
                    (text, note) = (at.text, format!("at the base commit, {}", at.note));
                    BarResult::CheckCouldNotRun
                }
            }
        }
    };
    if result == BarResult::Fail {
        failing = head.failing.clone().unwrap_or_default();
    }
    let now = Failure { commit: tip.as_deref(), result, exit: head.code, base_exit, failing: &failing };
    let no_progress = before.as_ref().is_some_and(|b| now.repeats(b));
    let tail = tail_for(&note, &text);
    let event = SessionEvent::BarChecked {
        job: job.name.clone(),
        commit: tip.clone(),
        exit: head.code,
        tail: tail.clone(),
        base_exit,
        result,
        failing: failing.clone(),
        no_progress,
    };
    port(ctx, job).emit(event)?;
    Ok(CheckRun { command, commit: tip, result, exit: head.code, base_exit, failing, no_progress, tail })
}

/// How often a check that the memory gate holds looks at the machine's memory again.
const MEMORY_POLL: std::time::Duration = std::time::Duration::from_secs(15);

/// The machine's memory gate, for a check as for a session: a check starts no sooner than the
/// machine's available memory is at the owner's floor (`[machine] min_free_mem_gb`,
/// [`crate::systems::dispatch::memory_hold`]). It waits here, in its runner, which keeps its seat
/// and its compiling unit meanwhile, and says why in the runner's log. Nothing running is touched;
/// a stop ends the wait with the runner. With no floor in the policy it does not look.
fn wait_for_memory(ctx: &Ctx, job: &Job) {
    if ctx.cfg.machine.min_free_mem_gb.is_none() {
        return;
    }
    let mut said = false;
    while let Some(why) = crate::systems::dispatch::memory_hold(&ctx.cfg, crate::os::mem_available_gb()) {
        if !said {
            eprintln!("{} {}: its acceptance check waits: {why}", crate::os::stamp(), job.name);
            said = true;
        }
        std::thread::sleep(MEMORY_POLL);
    }
}

/// A check's failure, as far as two runs of it are compared.
struct Failure<'a> {
    commit: Option<&'a str>,
    result: BarResult,
    exit: i32,
    base_exit: Option<i32>,
    failing: &'a [String],
}

impl Failure<'_> {
    /// Whether this check failed at the same commit as `before` did, and the same way: the same exit
    /// statuses (at the tip, and at the base where it ran there) and the same failing tests, whatever
    /// order they were named in. The output itself is not compared: it carries timings. The check runs
    /// on the committed tip, so at the same commit the session changed nothing it judges, and a fail
    /// that repeats says nothing new. A different fail at the same commit (another test, another exit)
    /// is a check that does not always say the same of the same code: that is new, and is told.
    ///
    /// A run that names no failing test (its output could not be read, or the failure is not a test's,
    /// such as clippy's) says nothing about tests either way, so where either run names none the exit
    /// statuses alone decide: a wrong "same" costs one hand-over, a wrong "different" another cold build.
    fn repeats(&self, before: &Bar) -> bool {
        let same = |a: &[String], b: &[String]| {
            a.is_empty()
                || b.is_empty()
                || (a.len() == b.len() && a.iter().all(|t| b.contains(t)) && b.iter().all(|t| a.contains(t)))
        };
        self.result == BarResult::Fail
            && before.result == BarResult::Fail
            && self.commit.is_some()
            && self.commit == before.commit.as_deref()
            && self.exit == before.exit
            && self.base_exit == before.base_exit
            && same(self.failing, &before.failing)
    }
}

/// Whether a check that failed at the tip and at the base, naming the failing tests `tip` and `base`,
/// failed for the base's reason, and if so the tip's failing tests. Either list unknown: it cannot be
/// compared, so it is the job's. Both naming tests: every test failing at the tip fails at the base
/// too. Neither naming any: failing at the base is enough. Only one naming tests: they failed
/// differently, so it is the job's.
fn base_red(tip: Option<&[String]>, base: Option<&[String]>) -> Option<Vec<String>> {
    let (tip, base) = (tip?, base?);
    match (tip.is_empty(), base.is_empty()) {
        (true, true) => Some(Vec::new()),
        (false, false) => tip.iter().all(|t| base.contains(t)).then(|| tip.to_vec()),
        _ => None,
    }
}

/// The most failing tests compared, and the longest output line read: the output is the worker's,
/// so past either it is not compared at all.
const MAX_FAILING: usize = 1000;
const MAX_LINE: usize = 64 * 1024;

/// The failing tests a whole `cargo test` output names, in the order first named: its
/// `test NAME ... FAILED` lines and the names indented under a `failures:` line. A name is kept whole,
/// spaces and all (a doctest's is `src/lib.rs - f (line 3)`), after the test target it ran in, since a
/// name is only unique within one. The `---- NAME stdout ----` headers are not read: `--show-output`
/// prints them for passing tests too, under `successes:`. `None` when the output cannot be read
/// whole: a read error, a line longer than [`MAX_LINE`] or more than [`MAX_FAILING`] failing tests.
fn failing_tests(mut output: impl BufRead) -> Option<Vec<String>> {
    let (mut names, mut target, mut listing, mut buf) = (Vec::<String>::new(), String::new(), false, Vec::new());
    loop {
        buf.clear();
        if output.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).ok()? == 0 {
            return Some(names);
        }
        if buf.len() > MAX_LINE {
            return None;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        let qualified = |name: &str| if target.is_empty() { name.to_owned() } else { format!("{target}: {name}") };
        // (whether it failed, the test): a cargo test name is qualified with its target; nextest's already is.
        let outcome = if let Some(t) = test_target(line) {
            (target, listing) = (t, false);
            None
        } else if line == "failures:" {
            listing = true;
            None
        } else if let Some(n) = line.strip_prefix("test ").and_then(|l| l.strip_suffix(" ... FAILED")).map(str::trim)
            && !n.is_empty()
        {
            Some((true, qualified(n)))
        } else if let Some((failed, n)) = nextest_outcome(line) {
            Some((failed, n.to_owned()))
        } else if listing && line.starts_with("    ") && !line.trim().is_empty() {
            Some((true, qualified(line.trim())))
        } else {
            listing &= line.trim().is_empty();
            None
        };
        match outcome {
            // A retry that passes (`TRY 2 PASS`, `FLAKY 2/3`) takes back the attempts that failed.
            Some((false, id)) => names.retain(|m| *m != id),
            Some((true, id)) if !names.contains(&id) => {
                if names.len() == MAX_FAILING {
                    return None;
                }
                names.push(id);
            }
            _ => {}
        }
    }
}

/// Whether a `cargo nextest` status line says a test failed or passed, and the test, already named
/// with its target: `        FAIL [   0.004s] relay::flow a_test` failed `relay::flow a_test`. A
/// timeout, a signal, an abort and a failed attempt (`TRY 2 FAIL`) fail; a pass, a passing retry
/// (`TRY 2 PASS`) and a flaky test (`FLAKY 2/3`) pass. A skip and the summary line are neither.
fn nextest_outcome(line: &str) -> Option<(bool, &str)> {
    let (status, rest) = line.trim_start().split_once(" [")?;
    let failed = status.ends_with("FAIL") || status.starts_with("SIG") || matches!(status, "TIMEOUT" | "ABORT");
    let passed = status.ends_with("PASS") || status.starts_with("FLAKY");
    let id = rest.split_once("] ")?.1.trim();
    // Newer versions put the test's place in the run first: `(3/10) relay::flow a_test`.
    let id = id.strip_prefix('(').and_then(|i| i.split_once(") ")).map_or(id, |(_, i)| i.trim_start());
    (!id.is_empty() && (failed || passed)).then_some((failed, id))
}

/// The test target a `cargo test` status line starts, without its binary's hash:
/// `     Running tests/flow.rs (target/debug/deps/flow-0a1b2c3d)` is `tests/flow.rs (flow)`, and
/// `   Doc-tests relay` is `doctests relay`.
fn test_target(line: &str) -> Option<String> {
    if let Some(krate) = line.strip_prefix("   Doc-tests ") {
        return Some(format!("doctests {krate}"));
    }
    let what = line.strip_prefix("     Running ")?;
    let Some((src, bin)) = what.strip_suffix(')').and_then(|w| w.split_once(" (")) else {
        return Some(what.to_owned());
    };
    let file = bin.rsplit(['/', '\\']).next().unwrap_or(bin);
    let stem = file.rsplit_once('-').filter(|(_, h)| h.bytes().all(|b| b.is_ascii_hexdigit())).map_or(file, |(s, _)| s);
    Some(format!("{src} ({stem})"))
}

/// The check at the commit the branch started from.
fn at_base(ctx: &Ctx, base: Option<&str>, home: &Home, command: &str, dir: &SessionRepo, place: &Place<'_>) -> Ran {
    match base.map(str::to_owned).or_else(|| checkpoint::base(dir, &[]).head) {
        Some(base) => at(ctx, home, command, dir, &base, Phase::CheckBase, place),
        None => broke("no base commit is known for the revert control"),
    }
}

fn broke(why: &str) -> Ran {
    Ran { said: Said::Broke, code: -1, text: String::new(), note: why.to_owned(), failing: Some(Vec::new()) }
}

/// Where a check runs: its working directory, made fresh for each run in a place only relay writes,
/// and the job's inbox, where its output goes.
struct Place<'a> {
    job: &'a Job,
    inbox: &'a HostDir,
    work: &'a Path,
    /// The attempt's development environment, read when it started: the same at the tip and the base.
    dev: &'a [(OsString, Option<OsString>)],
}

/// Runs `command` on a clean checkout of `rev` from the clone `source`.
fn at(ctx: &Ctx, home: &Home, command: &str, source: &SessionRepo, rev: &str, phase: Phase, place: &Place<'_>) -> Ran {
    let tree = place.work.join("tree");
    // What the last run left (a mode a test set, say) is cleared first; a directory that cannot be
    // cleared is said so, and is not mistaken for a checkout that failed.
    let ran = match hostdir::fresh_dir(place.work) {
        Err(e) => broke(&format!("could not clear the check directory {}: {e}", place.work.display())),
        Ok(()) => match checkout(source, rev, &tree) {
            Ok(()) => exec(ctx, home, command, &tree, phase, place),
            Err(e) => broke(&format!("could not check out {rev} for the check: {e:#}")),
        },
    };
    clear(ctx, place.job, place.work);
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

/// `sh -c command` under `timeout`, on the attempt's development environment `dev`: the one its
/// warm-up recorded from the repository, never from files the session could have edited, and read
/// when the attempt started, so a warm-up for a new key during the run does not change it.
pub(super) fn invocation(
    ctx: &Ctx,
    job: &Job,
    phase: Phase,
    command: &str,
    dev: &[(OsString, Option<OsString>)],
) -> Invocation {
    let mut inv = Invocation::new("timeout");
    let secs = ctx.cfg.hooks.command_timeout_seconds.max(1).to_string();
    inv.args = ["-k", "5", &secs, "sh", "-c", command].map(OsString::from).to_vec();
    layer_env(ctx, job, phase, &mut inv, dev);
    inv
}

/// Runs `sh -c command` in `tree`, bounded by `hooks.command_timeout_seconds` (killed 5 s later if it
/// ignores the TERM), in the session's sandbox plan without the session's account or clone.
fn exec(ctx: &Ctx, home: &Home, command: &str, tree: &Path, phase: Phase, place: &Place<'_>) -> Ran {
    let go = || -> Result<Ran> {
        let inv = invocation(ctx, place.job, phase, command, place.dev);
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
        let declared = &ctx.cfg.hooks.check_could_not_run_exit_codes;
        let said = said(code, declared);
        // A command that could not judge the code says why first, from what relay saw, before its output.
        let note = match said {
            Said::Broke => {
                format!("the check could not run: {}", why(code, declared, ctx.cfg.hooks.command_timeout_seconds))
            }
            _ => String::new(),
        };
        let failing = match said {
            Said::Failed => place.inbox.read("check.out").ok().and_then(|f| failing_tests(std::io::BufReader::new(f))),
            _ => Some(Vec::new()),
        };
        let _ = place.inbox.remove("check.out");
        Ok(Ran { said, code: code.unwrap_or(-1), text, note, failing })
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

    /// Review of #54: each check read the project's latest record, so a warm-up for a changed key during
    /// a run could give the session, the tip check and the base check different toolchains. The check
    /// takes the environment its attempt read when it started.
    #[test]
    fn a_warm_up_during_the_run_does_not_change_the_checks_environment() {
        use crate::config::{Config, tempdir::Dir};
        let d = Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let (shell, tool) = (d.path().join("shell"), d.path().join("tool"));
        std::fs::write(&shell, format!("#!/bin/sh\nexport DEV_TOOL=\"$(cat '{}')\"\nexec \"$@\"\n", tool.display()))
            .unwrap();
        std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let mut cfg = Config::parse("name = \"t\"\n", &repo).unwrap();
        cfg.session.env_shell = vec![shell.display().to_string()];
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        std::fs::create_dir_all(&ctx.bus).unwrap();
        let warm = |toolchain: &str| {
            std::fs::write(&tool, toolchain).unwrap();
            assert_eq!(crate::systems::devshell::tests::settled(&ctx), "entered");
        };
        let tool_in = |env: &[(OsString, Option<OsString>)]| {
            env.iter().rev().find(|(k, _)| k == "DEV_TOOL").and_then(|(_, v)| v.clone())
        };
        warm("rust-1.99");
        // The attempt starts: its environment is read once.
        let dev = crate::systems::devshell::snapshot(&ctx).unwrap();
        // Mid-run, the flake lock moves and the environment is entered again for the new key.
        std::fs::write(repo.join("flake.lock"), "{}").unwrap();
        warm("rust-2.0");
        let now = crate::systems::devshell::snapshot(&ctx).unwrap();
        assert_eq!(tool_in(&now).as_deref(), Some("rust-2.0".as_ref()), "the record moved on");
        // The tip check and the base check both run on what the session ran on.
        for _ in ["tip", "base"] {
            let inv = invocation(&ctx, &Job::default(), Phase::Check, "true", &dev);
            assert_eq!(tool_in(&inv.env).as_deref(), Some("rust-1.99".as_ref()));
        }
    }

    /// The check at the tip and the check at the base are each told their job and their phase, through the
    /// invocation the check is run with.
    #[test]
    fn the_check_at_the_tip_and_at_the_base_each_hold_their_own_phase() {
        use crate::config::{Config, tempdir::Dir};
        let d = Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = Config::parse("name = \"tickify\"\n", &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        let job = Job { name: "add-login".into(), attempts: 3, ..Job::default() };
        for (phase, want) in [(Phase::Check, "check"), (Phase::CheckBase, "check-base")] {
            let env = crate::sandbox::session_env(&ctx, &invocation(&ctx, &job, phase, "true", &[]));
            let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_string_lossy().into_owned());
            assert_eq!(get("RELAY_PROJECT").as_deref(), Some("tickify"));
            assert_eq!(get("RELAY_JOB").as_deref(), Some("add-login"));
            assert_eq!(get("RELAY_ATTEMPT").as_deref(), Some("3"));
            assert_eq!(get("RELAY_PHASE").as_deref(), Some(want));
        }
    }

    #[test]
    fn only_a_command_that_ran_and_failed_has_failed() {
        assert_eq!(said(Some(0), &[]), Said::Passed);
        assert_eq!(said(Some(1), &[]), Said::Failed);
        assert_eq!(said(Some(101), &[]), Said::Failed, "a test runner's own failure code");
        for broken in [Some(124), Some(125), Some(126), Some(127), Some(137), Some(143), None] {
            assert_eq!(said(broken, &[]), Said::Broke, "{broken:?}");
        }
    }

    /// NS-9 follow-up (Tickify): a check that is refused by its environment exits like any failure (mise
    /// exits 1), and relay cannot tell that from the text. The project declares the code its check uses
    /// for "could not run"; relay then ends it so, and says why from the code and the timeout it applied.
    #[test]
    fn a_declared_exit_code_is_a_check_that_could_not_run() {
        assert_eq!(said(Some(3), &[]), Said::Failed, "undeclared, 3 is a failure");
        assert_eq!(said(Some(3), &[3]), Said::Broke);
        assert_eq!(said(Some(1), &[3]), Said::Failed, "only what is declared");
        assert_eq!(said(Some(0), &[0]), Said::Passed, "a pass is never a refusal");
    }

    #[test]
    fn a_check_that_could_not_run_says_why_from_what_relay_saw() {
        assert!(why(Some(124), &[], 120).contains("120 s"), "{}", why(Some(124), &[], 120));
        assert!(why(Some(127), &[], 120).contains("not found"));
        assert!(why(Some(126), &[], 120).contains("not runnable"));
        assert!(why(Some(137), &[], 120).contains("killed"));
        assert!(why(None, &[], 120).contains("signal"));
        let declared = why(Some(3), &[3], 120);
        assert!(declared.contains("exited 3") && declared.contains("check_could_not_run_exit_codes"), "{declared}");
    }

    /// Copilot on #65: a check that could not run and printed 4 KiB or more lost the line saying why (and
    /// the base-commit prefix), because the tail keeps the end of one string. The explanation stays
    /// whole; only the output is cut to what is left.
    #[test]
    fn the_explanation_of_a_check_that_could_not_run_survives_a_long_output() {
        let note = "at the base commit, the check could not run: it timed out (exit 124, after 120 s)";
        let output = format!("{}the last line", "noise\n".repeat(2000));
        let tail = tail_for(note, &output);
        assert!(tail.starts_with(note), "{tail}");
        assert!(tail.ends_with("the last line"), "the end of the output is what stays");
        assert!(tail.len() <= TAIL + 64, "{}", tail.len());
        assert_eq!(tail_for("", "short"), "short");
        assert_eq!(tail_for(note, ""), note);
        let tail = tail_for(&"x".repeat(10_000), &output);
        assert!(tail.len() <= NOTE + 64 + TAIL + 64 && tail.ends_with("the last line"), "{}", tail.len());
    }

    const RED: &str = "running 3 tests\ntest a::one ... ok\ntest a::two ... FAILED\n\nfailures:\n\n\
        ---- a::two stdout ----\nthread 'a::two' panicked at src/a.rs:9:5:\n  left: 17\n\n\
        failures:\n    a::two\n\ntest result: FAILED. 1 passed; 1 failed\n";

    fn names(output: &str) -> Vec<String> {
        failing_tests(output.as_bytes()).expect("read whole")
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
        let doc =
            "   Doc-tests relay\ntest src/lib.rs - f (line 3) ... FAILED\n\nfailures:\n    src/lib.rs - f (line 3)\n";
        assert_eq!(names(doc), ["doctests relay: src/lib.rs - f (line 3)"]);
        let other = doc.replace("line 3", "line 9");
        assert_eq!(base_red(Some(&names(&other)), Some(&names(doc))), None, "another doctest");
    }

    #[test]
    fn a_test_is_named_with_the_target_it_ran_in() {
        let in_target =
            |bin: &str| format!("     Running tests/{bin}.rs (target/debug/deps/{bin}-0a1b2c3d4e5f6a7b)\n{RED}");
        assert_eq!(names(&in_target("flow")), ["tests/flow.rs (flow): a::two"]);
        let (at_base, at_tip) = (names(&in_target("one")), names(&in_target("two")));
        assert_eq!(base_red(Some(&at_tip), Some(&at_base)), None, "the same name in another target");
        let rebuilt = names(&in_target("flow").replace("0a1b2c3d4e5f6a7b", "ffffffffffffffff"));
        assert_eq!(rebuilt, names(&in_target("flow")), "a binary's hash is not part of its name");
    }

    #[test]
    fn a_nextest_output_names_its_failing_tests() {
        let run = "    Starting 3 tests across 2 binaries\n        PASS [   0.004s] relay a::one\n        \
            FAIL [   0.010s] relay::flow a_test\n     TIMEOUT [  60.001s] (3/3) relay b::slow\n------------\n     \
            Summary [  60.1s] 3 tests run: 1 passed, 2 failed\n        FAIL [   0.010s] relay::flow a_test\n";
        assert_eq!(names(run), ["relay::flow a_test", "relay b::slow"]);
        assert_eq!(nextest_outcome("   TRY 2 FAIL [   0.010s] relay a::flaky"), Some((true, "relay a::flaky")));
        assert_eq!(nextest_outcome("        SKIP [         ] relay a::skipped"), None);
    }

    #[test]
    fn a_nextest_retry_that_passes_is_not_a_failure() {
        let old = "        FAIL [   0.010s] relay a::old\n";
        let recovered = format!("{old}   TRY 1 FAIL [   0.010s] relay a::new\n   FLAKY 2/2 [   0.010s] relay a::new\n");
        let both = format!("{old}   TRY 1 FAIL [   0.010s] relay a::new\n   TRY 2 FAIL [   0.010s] relay a::new\n");
        assert_eq!(names(&recovered), ["relay a::old"]);
        assert_eq!(names(&recovered.replace("FLAKY 2/2", "TRY 2 PASS")), ["relay a::old"]);
        let (at_base, at_tip) = (names(&recovered), names(&both));
        assert_eq!(base_red(Some(&at_tip), Some(&at_base)), None, "it recovered at the base and fails at the tip");
        let after_cargo = format!("     Running tests/flow.rs (target/debug/deps/flow-0a1b)\n{recovered}");
        assert_eq!(names(&after_cargo), ["relay a::old"], "a cargo target before it does not rename nextest's tests");
        assert_eq!(names("test  ... FAILED\n"), Vec::<String>::new(), "no name");
    }

    #[test]
    fn an_output_too_big_to_compare_is_not_compared() {
        let long = format!("{}\n{RED}", "x".repeat(MAX_LINE + 1));
        assert_eq!(failing_tests(long.as_bytes()), None, "a line too long to read");
        let mut many = String::new();
        for i in 0..=MAX_FAILING {
            std::fmt::Write::write_fmt(&mut many, format_args!("test t{i} ... FAILED\n")).expect("to a String");
        }
        assert_eq!(failing_tests(many.as_bytes()), None, "more failing tests than are kept");
        assert_eq!(base_red(None, Some(&names(RED))), None, "unknown at the tip is the job's");
    }

    #[test]
    fn a_failure_is_the_base_s_when_every_failing_test_fails_there_too() {
        let (red1, both) = (names(RED), names(&RED.replace("    a::two\n", "    a::two\n    a::new\n")));
        assert_eq!(base_red(Some(&red1), Some(&both)), Some(vec!["a::two".to_owned()]), "fewer fail at the tip");
        assert_eq!(base_red(Some(&both), Some(&red1)), None, "a new failing test is the job's");
        assert_eq!(base_red(Some(&[]), Some(&[])), Some(vec![]), "nothing to compare: the base fails too");
        assert_eq!(base_red(Some(&[]), Some(&red1)), None, "the tip fails before the tests the base fails");
    }

    #[test]
    fn a_fail_repeats_the_one_before_only_at_the_same_commit_the_same_way() {
        let before = |commit: &str, failing: &[&str]| Bar {
            commit: Some(commit.into()),
            result: BarResult::Fail,
            exit: 101,
            base_exit: Some(0),
            failing: failing.iter().map(|&t| t.to_owned()).collect(),
            no_progress: false,
            tail: String::new(),
        };
        let two = ["t: a".to_owned(), "t: b".to_owned()];
        let now = Failure { commit: Some("c1"), result: BarResult::Fail, exit: 101, base_exit: Some(0), failing: &two };
        assert!(now.repeats(&before("c1", &["t: b", "t: a"])), "the same tests, in any order");
        assert!(!now.repeats(&before("c0", &["t: a", "t: b"])), "a new commit is progress");
        assert!(!now.repeats(&before("c1", &["t: a"])), "another failing test at the same commit is new");
        assert!(!Failure { exit: 1, ..now }.repeats(&before("c1", &["t: a", "t: b"])), "another exit status");
        assert!(!Failure { base_exit: Some(1), ..now }.repeats(&before("c1", &["t: a", "t: b"])), "another base");
        let none = Failure { failing: &[], ..now };
        assert!(none.repeats(&before("c1", &[])), "neither names a test: the exit statuses decide");
        assert!(none.repeats(&before("c1", &["t: a"])), "only one names tests: the exit statuses decide");
        assert!(now.repeats(&before("c1", &[])), "the earlier run named none: the exit statuses decide");
        assert!(
            !Failure { result: BarResult::BaseRed, ..now }.repeats(&before("c1", &["t: a", "t: b"])),
            "only a fail"
        );
        let passed = Bar { result: BarResult::Pass, ..before("c1", &["t: a", "t: b"]) };
        assert!(!now.repeats(&passed), "after a pass, a fail is news");
        assert!(!Failure { commit: None, ..now }.repeats(&Bar { commit: None, ..before("c1", &[]) }), "no commit");
    }

    #[test]
    fn the_tail_keeps_the_end() {
        assert_eq!(tail_of("short", 10), "short");
        let t = tail_of("0123456789", 4);
        assert!(t.ends_with("6789") && t.contains("6 earlier bytes"), "{t}");
        assert!(tail_of("ééééé", 3).ends_with('é'), "never mid-character");
    }
}
