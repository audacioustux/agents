//! The one way relay waits for a child it starts for a session or a decision handler.
//!
//! A child is done only when it has exited *and* its output has closed: a process can close its
//! output and run on, or exit and leave a helper holding the output open. Until both, the caller's
//! `stop` is asked every look (its limits, whether the child is still wanted), and an answer kills
//! the child, with its whole process group when it leads one. Dropped unreaped, it is killed, so no
//! error path leaves one running unwatched. `Child::wait` and `Child::try_wait` are disallowed
//! elsewhere (`clippy.toml`); the few children no session or handler path waits on say why.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

/// How often a supervised child is looked at.
pub const LOOK: Duration = Duration::from_millis(100);

/// Which process group a supervised child runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// A group of its own, killed whole: a decision handler and everything it starts.
    Own,
    /// The runner's group, so a `queue stop` (a TERM to that group) reaches it too: a session, whose
    /// sandbox (bubblewrap's pid namespace, `--die-with-parent`) goes with it when it is killed.
    Runner,
}

#[derive(Debug)]
pub struct Supervised {
    child: Child,
    group: Group,
    exited: Option<ExitStatus>,
}

impl Supervised {
    /// Starts `cmd` in `group`.
    pub fn spawn(cmd: &mut Command, group: Group) -> io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        if group == Group::Own {
            cmd.process_group(0);
        }
        Ok(Self { child: cmd.spawn()?, group, exited: None })
    }

    /// The child, for its pipes.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kills it (its whole group when it leads one) and reaps it.
    pub fn kill(&mut self) {
        if self.exited.is_some() {
            // Reaped: its id may name another process by now, and a group it led is gone with it
            // unless a member holds the id, in which case `finish` is still looking and kills that.
            return;
        }
        if self.group == Group::Own {
            let _ = crate::os::kill_group(self.child.id());
        }
        let _ = self.child.kill();
        #[allow(clippy::disallowed_methods, reason = "the supervisor reaps a child it has just killed")]
        let status = self.child.wait();
        self.exited = status.ok();
    }

    /// Waits until the child has exited and `closed` says its output has closed, asking `stop` every
    /// [`LOOK`]. `Ok(Err(why))`: `stop` answered first, and the child was killed (its group when it
    /// leads one, even after the leader exited). `Ok(Ok(status))`: it ended on its own.
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
                if self.exited.is_some() && self.group == Group::Own {
                    // The leader is gone and something in its group holds the output: the group lives on.
                    let _ = crate::os::kill_group(self.child.id());
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
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

    /// PR #39 third review, issue 1: a child that closes its output and runs on is still supervised:
    /// stopped when asked, not waited for.
    #[test]
    fn a_child_that_closes_its_output_and_runs_on_is_stopped_when_asked() {
        let mut s = Supervised::spawn(&mut sh("exec >&-; sleep 30"), Group::Own).unwrap();
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

    /// One that exits but leaves a helper holding its output: the helper's group is killed when asked.
    #[test]
    fn a_helper_left_holding_the_output_goes_with_the_group() {
        let dir = crate::config::tempdir::Dir::new();
        let pid = dir.path().join("helper.pid");
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 30 & echo $! >{}", pid.display())), Group::Own).unwrap();
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        let mut looks = 0;
        let r = s.finish(
            || reader.is_finished(),
            || {
                (looks > 5).then_some("stop").or_else(|| {
                    looks += 1;
                    None
                })
            },
        );
        assert_eq!(r.unwrap().unwrap_err(), "stop", "it exited, but its output was held open");
        let helper = std::fs::read_to_string(&pid).unwrap().trim().to_owned();
        assert!(eventually(|| gone(&helper)), "the helper went with the group");
    }

    /// Done on its own once it exited and its output closed; dropped unreaped, it is killed.
    #[test]
    fn a_child_ends_on_its_own_and_one_dropped_is_killed() {
        let mut s = Supervised::spawn(&mut sh("echo hi"), Group::Own).unwrap();
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        assert!(s.finish(|| reader.is_finished(), || None::<()>).unwrap().unwrap().success());
        let s = Supervised::spawn(&mut sh("sleep 30"), Group::Runner).unwrap();
        let pid = s.child.id().to_string();
        drop(s);
        assert!(eventually(|| gone(&pid)), "dropped, it was killed and reaped");
    }
}
