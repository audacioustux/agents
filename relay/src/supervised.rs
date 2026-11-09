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

/// How often the supervisor looks at its child and its runner.
const SUPERVISOR_LOOK: Duration = Duration::from_millis(25);

/// A command that runs `program` under `relay supervise`, tied to this process as its runner: add
/// its arguments, environment, directory and pipes as for `program` itself, which inherits them all.
pub fn command(program: impl AsRef<OsStr>) -> io::Result<Command> {
    let mut c = Command::new(supervisor()?);
    c.args(["supervise", "--parent", &std::process::id().to_string(), "--"]).arg(program);
    Ok(c)
}

/// The relay binary that supervises. A unit test runs in a test binary under `target/<profile>/deps`;
/// cargo builds relay itself beside it for the integration tests.
fn supervisor() -> io::Result<std::path::PathBuf> {
    #[cfg(not(test))]
    return crate::os::own_binary();
    #[cfg(test)]
    {
        let exe = std::env::current_exe()?;
        let relay = exe.parent().and_then(std::path::Path::parent).map(|d| d.join("relay"));
        relay.filter(|p| p.exists()).ok_or_else(|| io::Error::other("relay is not built beside the tests"))
    }
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
        debug_assert_eq!(cmd.get_args().next(), Some(OsStr::new("supervise")), "made by supervised::command");
        let child = cmd.process_group(0).spawn()?;
        Ok(Self { child, exited: None })
    }

    /// The child, for its pipes.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kills its whole group and reaps it. The supervisor is not reaped until it has been, so the
    /// group id is still its own when it is signalled.
    pub fn kill(&mut self) {
        if self.exited.is_none() {
            let _ = crate::os::kill_group(self.child.id());
            let _ = self.child.kill();
            #[allow(clippy::disallowed_methods, reason = "the supervisor reaps a child it has just killed")]
            let status = self.child.wait();
            self.exited = status.ok();
        }
    }

    /// Waits until the child has exited and `closed` says its output has closed, asking `stop` every
    /// [`LOOK`]. `Ok(Err(why))`: `stop` answered first, and the child's group was killed.
    /// `Ok(Ok(status))`: it ended on its own (and its supervisor ended what it left in its group).
    pub fn finish<S>(
        &mut self,
        closed: impl Fn() -> bool,
        mut stop: impl FnMut() -> Option<S>,
    ) -> io::Result<Result<ExitStatus, S>> {
        loop {
            if self.exited.is_none() {
                #[allow(clippy::disallowed_methods, reason = "the supervisor's own look")]
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

/// `relay supervise --parent <runner> -- <argv>`: runs `argv` in this process's own group, which it
/// leads, and ends the group with it:
/// - `argv` exits: everything it left in the group is killed, and this exits as it did;
/// - the runner is gone (this process's parent is no longer `runner`, looked at before `argv` starts
///   and every [`SUPERVISOR_LOOK`], so a runner that dies at any moment is seen), or this is sent
///   TERM, INT or HUP: the whole group is killed, this process with it.
///
/// It polls its parent rather than relying on `PR_SET_PDEATHSIG`, which fires when the *thread* that
/// started it ends, and would need a handler to clean up after.
pub fn supervise(runner: u32, argv: &[OsString]) -> ExitCode {
    let me = rustix::process::getpid();
    if rustix::process::getpgrp() != me && rustix::process::setpgid(None, None).is_err() {
        // Never signal a group it does not lead: it could be the runner's, or a shell's.
        eprintln!("relay supervise: cannot lead a process group of its own");
        return ExitCode::from(127);
    }
    let told = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP] {
        if signal_hook::flag::register(sig, Arc::clone(&told)).is_err() {
            end_group();
        }
    }
    let runner_gone = || rustix::process::getppid().map(|p| p.as_raw_nonzero().get().cast_unsigned()) != Some(runner);
    if runner_gone() {
        end_group();
    }
    let Some((program, args)) = argv.split_first() else {
        eprintln!("relay supervise: nothing to run");
        return ExitCode::from(127);
    };
    let mut child = match Command::new(program).args(args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("relay supervise: starting {}: {e}", program.to_string_lossy());
            return ExitCode::from(127);
        }
    };
    loop {
        if runner_gone() || told.load(Ordering::Relaxed) {
            end_group();
        }
        #[allow(clippy::disallowed_methods, reason = "the supervisor itself")]
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_rest_of_group(me);
                return exit_code(status);
            }
            Ok(None) => std::thread::sleep(SUPERVISOR_LOOK),
            Err(_) => end_group(),
        }
    }
}

/// SIGKILL to this process's own group, which it leads: everything in it, this process too.
fn end_group() -> ! {
    let _ = rustix::process::kill_current_process_group(Signal::KILL);
    // Unreachable: the signal reaches this process as well.
    std::process::exit(137)
}

/// Kills every other process in the group `me` leads, until none is left, and lives on to report
/// how its child ended. Each is signalled through a pidfd opened before checking it is still in the
/// group, so a pid that was let go and reused is never signalled. A group that will not empty (a
/// process forking as fast as it is killed) is ended whole instead.
fn kill_rest_of_group(me: Pid) {
    let pgid = me.as_raw_nonzero().get().cast_unsigned();
    for _ in 0..1000 {
        let rest: Vec<u32> = crate::os::group_members(pgid).into_iter().filter(|&p| p != pgid).collect();
        if rest.is_empty() {
            return;
        }
        for pid in rest {
            let Some(p) = i32::try_from(pid).ok().and_then(Pid::from_raw) else { continue };
            match rustix::process::pidfd_open(p, PidfdFlags::empty()) {
                Ok(fd) if rustix::process::getpgid(Some(p)).is_ok_and(|g| g == me) => {
                    let _ = rustix::process::pidfd_send_signal(&fd, Signal::KILL);
                }
                Ok(_) | Err(rustix::io::Errno::SRCH) => {}
                Err(_) => end_group(),
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    end_group();
}

/// The exit code that says how `status` ended: its own, or 128 and the signal that ended it.
fn exit_code(status: ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt as _;
    let code = status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(1);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
mod tests {
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

    /// PR #39 fifth review: a handler's background grandchild is gone when the handler is stopped, and
    /// when the handler exits on its own, even holding the handler's output: its supervisor ends it.
    #[test]
    fn a_handlers_background_grandchild_goes_when_it_is_stopped_or_exits() {
        let dir = crate::config::tempdir::Dir::new();
        let pid = dir.path().join("stopped.pid");
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 300 & echo $! >{}; sleep 300", pid.display()))).unwrap();
        let grandchild = read_pid(&pid);
        assert_eq!(s.finish(|| false, || Some("stop")).unwrap().unwrap_err(), "stop");
        assert!(eventually(|| gone(&grandchild)), "stopped, the grandchild went with the group");

        let pid = dir.path().join("exited.pid");
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 300 & echo $! >{}", pid.display()))).unwrap();
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        let started = std::time::Instant::now();
        let r = s.finish(|| reader.is_finished(), || (started.elapsed() > Duration::from_secs(10)).then_some("stop"));
        assert!(r.unwrap().unwrap().success(), "it exited, and the grandchild holding its output was ended");
        assert!(gone(&read_pid(&pid)), "the grandchild went when the handler exited");
    }

    /// PR #39 fifth review, gaps 1 and 3: nothing is recorded, and a runner killed at once after it
    /// started a handler (before the supervisor has even looked) or while the handler works takes the
    /// handler's whole group with it. The "runner" here is a shell that starts the supervisor and is
    /// killed with SIGKILL.
    #[test]
    fn a_handler_goes_with_its_runner_however_soon_the_runner_is_killed() {
        let dir = crate::config::tempdir::Dir::new();
        let relay = supervisor().unwrap();
        for wait_for_handler in [false, true] {
            let tag = if wait_for_handler { "working" } else { "at-once" };
            let (sup, handler, grandchild) = (
                dir.path().join(format!("{tag}.sup")),
                dir.path().join(format!("{tag}.handler")),
                dir.path().join(format!("{tag}.gc")),
            );
            let inner =
                format!("echo $$ >{}; sleep 300 & echo $! >{}; sleep 300", handler.display(), grandchild.display());
            let wait = if wait_for_handler {
                format!("while [ ! -s {} ]; do sleep 0.05; done; ", grandchild.display())
            } else {
                String::new()
            };
            let runner = format!(
                "\"$0\" supervise --parent $$ -- sh -c '{inner}' & echo $! >{}; {wait}kill -9 $$",
                sup.display()
            );
            let mut sh = Command::new("sh");
            sh.arg("-c").arg(&runner).arg(&relay);
            #[allow(clippy::disallowed_methods, reason = "a test's stand-in runner, which kills itself")]
            let status = sh.status().unwrap();
            assert!(!status.success());
            let sup = read_pid(&sup);
            assert!(eventually(|| gone(&sup)), "{tag}: the supervisor ended");
            for f in [&handler, &grandchild] {
                if let Ok(pid) = std::fs::read_to_string(f) {
                    assert!(eventually(|| gone(pid.trim())), "{tag}: {} went with the runner", f.display());
                }
            }
            if wait_for_handler {
                assert!(grandchild.exists(), "the handler was at work when its runner was killed");
            }
            let pgid = sup.parse().unwrap();
            assert!(eventually(|| crate::os::group_members(pgid).is_empty()), "{tag}: nothing is left in its group");
        }
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
        let mut s = Supervised::spawn(&mut command("no-such-program-anywhere").unwrap()).unwrap();
        assert_eq!(s.finish(|| true, || None::<()>).unwrap().unwrap().code(), Some(127));
        let s = Supervised::spawn(&mut sh("sleep 30")).unwrap();
        let pid = s.child.id().to_string();
        drop(s);
        assert!(eventually(|| gone(&pid)), "dropped, it was killed and reaped");
    }
}
