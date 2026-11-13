//! The one way relay starts and waits for a child it runs for a session or a decision handler.
//!
//! Every such child runs under `relay supervise` ([`supervise`]), which leads a process group of its
//! own with the child and everything the child starts in it, and ends that group with itself: when
//! the child exits, when the runner that started it is gone (killed outright, crashed, stopped), or
//! when it is told to. Nothing is recorded and no stored number is ever signalled: the supervisor
//! signals its own group while it leads it, and the runner signals the group only while the
//! supervisor is its unreaped child, and in both cases the id cannot belong to anyone else.
//!
//! A child is done only when it has exited *and* its output has closed: a process can close its
//! output and run on. Until both, the caller's `stop` is asked every look (its limits, whether the
//! child is still wanted), and an answer kills the child's whole group. Dropped unreaped, it is
//! killed, so no error path leaves one running unwatched. `Child::wait` and `Child::try_wait` are
//! disallowed elsewhere (`clippy.toml`); the few children no session or handler path waits on say why.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustix::process::{Pid, PidfdFlags, Signal};

/// How often a supervised child is looked at.
pub const LOOK: Duration = Duration::from_millis(100);

/// How long a supervisor asked to stop has to end and reap its tree before its group is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often the supervisor looks at its child and its runner.
const SUPERVISOR_LOOK: Duration = Duration::from_millis(25);

/// A command that runs `program` under `relay supervise`, tied to this process as its runner: add
/// its arguments, environment, directory and pipes as for `program` itself, which inherits them all.
///
/// In unit tests it is `program` alone, leading its group as the supervisor would: they exercise
/// the runner's side, which needs no relay executable. The supervisor itself is the real binary's,
/// and tested as that (`tests/supervise.rs`, and end to end in `tests/flow.rs`).
pub fn command(program: impl AsRef<OsStr>) -> io::Result<Command> {
    holding(program, None)
}

/// As [`command`], with the supervisor holding the seat lock `hold` (`--hold`), shared, in a way its
/// child and everything it starts inherit: the lock is held until that whole tree has ended.
#[cfg_attr(test, allow(clippy::unnecessary_wraps, reason = "only the real supervisor can fail to be found"))]
#[cfg_attr(test, allow(unused_variables, reason = "no supervisor in unit tests"))]
pub fn holding(program: impl AsRef<OsStr>, hold: Option<&std::path::Path>) -> io::Result<Command> {
    #[cfg(not(test))]
    {
        let mut c = Command::new(crate::os::own_binary()?);
        c.args(["supervise", "--parent", &std::process::id().to_string()]);
        if let Some(path) = hold {
            c.arg("--hold").arg(path);
        }
        c.arg("--").arg(program);
        Ok(c)
    }
    #[cfg(test)]
    Ok(Command::new(program))
}

#[derive(Debug)]
pub struct Supervised {
    child: Child,
    exited: Option<ExitStatus>,
}

impl Supervised {
    /// Starts `cmd`, made by [`command`], as the leader of a process group of its own.
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        #[cfg(not(test))]
        debug_assert_eq!(cmd.get_args().next(), Some(OsStr::new("supervise")), "made by supervised::command");
        let child = cmd.process_group(0).spawn()?;
        Ok(Self { child, exited: None })
    }

    /// The child, for its pipes.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Stops it and reaps it: asks the supervisor (TERM) to end and reap its whole tree, which is how
    /// its seat outlives nothing of it ([`supervise`]), waits [`STOP_GRACE`] at most for that, and then
    /// kills whatever is left in its group, the supervisor too. The supervisor is not reaped until
    /// then, so the group id is still its own when it is signalled.
    pub fn kill(&mut self) {
        if self.exited.is_none() {
            if let Some(pid) = i32::try_from(self.child.id()).ok().and_then(Pid::from_raw)
                && rustix::process::kill_process(pid, Signal::TERM).is_ok()
            {
                let until = std::time::Instant::now() + STOP_GRACE;
                while std::time::Instant::now() < until && !self.leader_ended().unwrap_or(true) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let _ = crate::os::kill_group(self.child.id());
            let _ = self.child.kill();
            #[allow(clippy::disallowed_methods, reason = "the supervisor reaps a child it has just killed")]
            let status = self.child.wait();
            self.exited = status.ok();
        }
    }

    /// Whether the child (the supervisor, its group's leader) has ended, leaving it unreaped.
    fn leader_ended(&self) -> io::Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        let pid = i32::try_from(self.child.id()).ok().and_then(Pid::from_raw).ok_or_else(|| io::Error::other("pid"))?;
        let ended = waitid(WaitId::Pid(pid), WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT)?;
        Ok(ended.is_some())
    }

    /// Whether everything the child started was seen to end: the supervisor exited (with a code) only
    /// after it had reaped its whole tree, sandboxed processes included. Killed instead (its
    /// [`STOP_GRACE`] ran out, or it gave up on its tree), it ended by a signal, and a process of the tree
    /// may outlive it for a moment: its sandbox's processes are not in its group, and end only as their
    /// namespace is torn down. False until it has been reaped.
    pub fn tree_ended(&self) -> bool {
        self.exited.is_some_and(|status| status.code().is_some())
    }

    /// Waits until the child has exited and `closed` says its output has closed, asking `stop` every
    /// [`LOOK`]. `Ok(Err(why))`: `stop` answered first, and the child's group was killed.
    /// `Ok(Ok(status))`: it ended, however it ended, and whatever it left in its group was killed.
    pub fn finish<S>(
        &mut self,
        closed: impl Fn() -> bool,
        mut stop: impl FnMut() -> Option<S>,
    ) -> io::Result<Result<ExitStatus, S>> {
        loop {
            if self.exited.is_none() && self.leader_ended()? {
                // Whatever is left in its group (all of it, if the supervisor was killed) goes before it
                // is reaped: until then its pid, and so the group id, can be no one else's.
                let _ = crate::os::kill_group(self.child.id());
                #[allow(clippy::disallowed_methods, reason = "the supervisor reaps a child that has ended")]
                let status = self.child.try_wait()?;
                self.exited = status;
            }
            if let Some(status) = self.exited.filter(|_| closed()) {
                return Ok(Ok(status));
            }
            if let Some(why) = stop() {
                self.kill();
                return Ok(Err(why));
            }
            std::thread::sleep(LOOK);
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `relay supervise --parent <runner> [--hold <lock>] -- <argv>`: runs `argv` in this process's own
/// group, which it leads, and ends everything `argv` started with it:
/// - `argv` exits: everything it left is ended, and this exits as it did;
/// - the runner is gone (this process's parent is no longer `runner`, looked at before `argv` starts
///   and every [`SUPERVISOR_LOOK`], so a runner that dies at any moment is seen), or this is sent
///   TERM, INT or HUP: everything is ended, and this exits 137.
///
/// It is a subreaper (`PR_SET_CHILD_SUBREAPER`): whatever its tree orphans comes back to it, a child
/// that left its group with `setsid` or a sandbox's processes whose bwrap died, so it can end and reap
/// every descendant ([`end_tree`]) before it exits, and nothing it started outlives it. With `hold`
/// it holds that seat lock (shared, never inherited) for its whole life: a session's seat is taken
/// until the last process of its tree has ended. Only a tree that will not empty (forking as fast as
/// it is killed), or a signal that cannot be caught, takes its group and it at once.
///
/// It polls its parent rather than relying on `PR_SET_PDEATHSIG`, which fires when the *thread* that
/// started it ends, and would need a handler to clean up after.
pub fn supervise(runner: u32, hold: Option<&std::path::Path>, argv: &[OsString]) -> ExitCode {
    let me = rustix::process::getpid();
    if rustix::process::getpgrp() != me && rustix::process::setpgid(None, None).is_err() {
        // Never signal a group it does not lead: it could be the runner's, or a shell's.
        eprintln!("relay supervise: cannot lead a process group of its own");
        return ExitCode::from(127);
    }
    if rustix::process::set_child_subreaper(Some(me)).is_err() {
        eprintln!("relay supervise: cannot be a subreaper");
        return ExitCode::from(127);
    }
    let told = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP] {
        if signal_hook::flag::register(sig, Arc::clone(&told)).is_err() {
            return ExitCode::from(127);
        }
    }
    let _hold = match hold.map(crate::os::SeatHold::take).transpose() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("relay supervise: holding the seat: {e:#}");
            return ExitCode::from(127);
        }
    };
    let runner_gone = || rustix::process::getppid().map(|p| p.as_raw_nonzero().get().cast_unsigned()) != Some(runner);
    if runner_gone() {
        return ExitCode::from(137);
    }
    let Some((program, args)) = argv.split_first() else {
        eprintln!("relay supervise: nothing to run");
        return ExitCode::from(127);
    };
    let main = match Command::new(program).args(args).spawn() {
        // Reaped below with every other child, never through `Child`.
        Ok(c) => c.id(),
        Err(e) => {
            eprintln!("relay supervise: starting {}: {e}", program.to_string_lossy());
            return ExitCode::from(127);
        }
    };
    let mut status = None;
    loop {
        if runner_gone() || told.load(Ordering::Relaxed) {
            end_tree(me, main, &mut status);
            return ExitCode::from(137);
        }
        reap(main, &mut status);
        if let Some(st) = status {
            end_tree(me, main, &mut status);
            return exit_code(st);
        }
        std::thread::sleep(SUPERVISOR_LOOK);
    }
}

/// Reaps every child that has ended (an orphan of the tree too), noting `main`'s status in `status`.
/// Whether any child is left.
fn reap(main: u32, status: &mut Option<rustix::process::WaitStatus>) -> bool {
    use rustix::process::{WaitOptions, wait};
    loop {
        match wait(WaitOptions::NOHANG) {
            Ok(Some((pid, st))) => {
                if pid.as_raw_nonzero().get().cast_unsigned() == main {
                    *status = Some(st);
                }
            }
            Ok(None) => return true,
            // ECHILD: none is left, nor (a subreaper's) any descendant.
            Err(_) => return false,
        }
    }
}

/// Kills every descendant of `me` and reaps them all: when it returns, nothing `me` started runs.
/// Each is signalled through a pidfd opened before checking it is still one of the tree (its parent
/// is `me` or another of it), so a pid that was let go and reused is never signalled. A tree that
/// will not empty is ended with the group, this process too.
fn end_tree(me: Pid, main: u32, status: &mut Option<rustix::process::WaitStatus>) {
    let root = me.as_raw_nonzero().get().cast_unsigned();
    for _ in 0..1000 {
        let tree = crate::os::descendants(root);
        for &pid in &tree {
            let Some(p) = i32::try_from(pid).ok().and_then(Pid::from_raw) else { continue };
            match rustix::process::pidfd_open(p, PidfdFlags::empty()) {
                Ok(fd) if crate::os::parent_of(pid).is_some_and(|pp| pp == root || tree.contains(&pp)) => {
                    let _ = rustix::process::pidfd_send_signal(&fd, Signal::KILL);
                }
                Ok(_) | Err(rustix::io::Errno::SRCH) => {}
                Err(_) => end_group(),
            }
        }
        if !reap(main, status) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    end_group();
}

/// SIGKILL to this process's own group, which it leads: everything in it, this process too.
fn end_group() -> ! {
    let _ = rustix::process::kill_current_process_group(Signal::KILL);
    // Unreachable: the signal reaches this process as well.
    std::process::exit(137)
}

/// The exit code that says how `status` ended: its own, or 128 and the signal that ended it.
fn exit_code(status: rustix::process::WaitStatus) -> ExitCode {
    let code = status.exit_status().or_else(|| status.terminating_signal().map(|s| 128 + s)).unwrap_or(1);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    //! The runner's side, with `sh` standing in for the supervisor as the leader of its group
    //! ([`command`]); the supervisor itself is tested in `tests/supervise.rs`.

    /// A child that exits on its own was seen to end whole; one killed when stopped was not, so what it
    /// held (a session's copy of the linked caches) is not handed on.
    #[test]
    fn a_tree_is_seen_ended_only_when_its_leader_exits_on_its_own() {
        let mut done = Supervised::spawn(Command::new("sh").args(["-c", "exit 3"])).unwrap();
        let ended = done.finish(|| true, || None::<()>).unwrap();
        assert_eq!(ended.map(|s| s.code()), Ok(Some(3)));
        assert!(done.tree_ended());
        let mut stopped = Supervised::spawn(Command::new("sleep").arg("30")).unwrap();
        assert!(!stopped.tree_ended(), "not before it is reaped");
        assert_eq!(stopped.finish(|| true, || Some(())).unwrap(), Err(()));
        assert!(!stopped.tree_ended(), "killed: a process of its tree may outlive it");
    }

    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = command("sh").unwrap();
        c.args(["-c", script]).stdout(std::process::Stdio::piped());
        c
    }

    /// Whether `pid` has gone (or is a zombie no one has reaped yet).
    fn gone(pid: &str) -> bool {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        stat.is_empty() || stat.contains(") Z ")
    }

    fn eventually(f: impl Fn() -> bool) -> bool {
        (0..50).any(|_| {
            let ok = f();
            if !ok {
                std::thread::sleep(LOOK);
            }
            ok
        })
    }

    fn read_pid(path: &std::path::Path) -> String {
        assert!(eventually(|| std::fs::read_to_string(path).is_ok_and(|s| s.ends_with('\n'))), "{}", path.display());
        std::fs::read_to_string(path).unwrap().trim().to_owned()
    }

    /// PR #39 third review, issue 1: a child that closes its output and runs on is still supervised:
    /// stopped when asked, not waited for.
    #[test]
    fn a_child_that_closes_its_output_and_runs_on_is_stopped_when_asked() {
        let mut s = Supervised::spawn(&mut sh("exec >&-; sleep 30")).unwrap();
        let started = std::time::Instant::now();
        let mut looks = 0;
        let out = s.finish(
            || true,
            || {
                (looks > 3).then_some("withdrawn").or_else(|| {
                    looks += 1;
                    None
                })
            },
        );
        assert_eq!(out.unwrap().unwrap_err(), "withdrawn");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(eventually(|| gone(&s.child.id().to_string())));
    }

    /// A background grandchild goes with the group when the child is stopped.
    #[test]
    fn a_background_grandchild_goes_when_the_child_is_stopped() {
        let dir = crate::config::tempdir::Dir::new();
        let pid = dir.path().join("stopped.pid");
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 300 & echo $! >{}; sleep 300", pid.display()))).unwrap();
        let grandchild = read_pid(&pid);
        assert_eq!(s.finish(|| false, || Some("stop")).unwrap().unwrap_err(), "stop");
        assert!(eventually(|| gone(&grandchild)), "stopped, the grandchild went with the group");
    }

    /// PR #39 sixth review, issue 2: the supervisor (the group's leader) is killed while the rest of
    /// its group runs on, holding the output. The runner sees it end without reaping it, kills what
    /// is left while the unreaped leader still pins the group id, and only then reaps it: nothing
    /// survives to the timeout, and the output closes.
    #[test]
    fn a_killed_supervisor_takes_the_rest_of_its_group_with_it() {
        let dir = crate::config::tempdir::Dir::new();
        let pid = dir.path().join("left.pid");
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 300 & echo $! >{}; sleep 300", pid.display()))).unwrap();
        let left = read_pid(&pid);
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        let leader = rustix::process::Pid::from_raw(s.child.id().cast_signed()).unwrap();
        rustix::process::kill_process(leader, Signal::KILL).unwrap();
        let started = std::time::Instant::now();
        let r =
            s.finish(|| reader.is_finished(), || (started.elapsed() > Duration::from_secs(10)).then_some("timeout"));
        let status = r.unwrap().expect("ended once the rest of its group was killed, before the timeout");
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&status), Some(9));
        assert!(eventually(|| gone(&left)), "the rest of the group went with the leader");
    }

    /// Done on its own once it exited and its output closed, with its exit code; dropped unreaped, it is killed.
    #[test]
    fn a_child_ends_on_its_own_and_one_dropped_is_killed() {
        let mut s = Supervised::spawn(&mut sh("echo hi")).unwrap();
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        assert!(s.finish(|| reader.is_finished(), || None::<()>).unwrap().unwrap().success());
        let mut s = Supervised::spawn(&mut sh("exit 3")).unwrap();
        assert_eq!(s.finish(|| true, || None::<()>).unwrap().unwrap().code(), Some(3));
        let s = Supervised::spawn(&mut sh("sleep 30")).unwrap();
        let pid = s.child.id().to_string();
        drop(s);
        assert!(eventually(|| gone(&pid)), "dropped, it was killed and reaped");
    }
}
