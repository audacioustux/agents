//! The one way relay waits for a child it starts for a session or a decision handler.
//!
//! A child is done only when it has exited *and* its output has closed: a process can close its
//! output and run on, or exit and leave a helper holding the output open. Until both, the caller's
//! `stop` is asked every look (its limits, whether the child is still wanted), and an answer kills
//! the child, with its whole process group when it leads one. Dropped unreaped, it is killed, so no
//! error path leaves one running unwatched. `Child::wait` and `Child::try_wait` are disallowed
//! elsewhere (`clippy.toml`); the few children no session or handler path waits on say why.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

/// How often a supervised child is looked at.
pub const LOOK: Duration = Duration::from_millis(100);

/// Which process group a supervised child runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Group {
    /// A group of its own, killed whole: a decision handler and everything it starts. Recorded in the
    /// job's [`Ledger`] while it runs, so whoever sees the runner die kills it.
    Own(Ledger),
    /// The runner's group, so a `queue stop` (a TERM to that group) reaches it too: a session, whose
    /// sandbox (bubblewrap's pid namespace, `--die-with-parent`) goes with it when it is killed, and
    /// whose remains the reaper kills with the dead runner's group.
    Runner,
}

/// The process groups a job's runner has handlers running in, as files `<job>.group-<pgid>` among
/// the job's records. A handler's group is its own, so neither a signal to the runner's group nor
/// the runner's death reaches it: whoever sees the runner die (`queue stop`, the dispatcher's reaper,
/// the job's next runner) calls [`Ledger::kill_all`]. A group is signalled only while a process is
/// still in it, which also keeps its id from being anyone else's: an id in use as a group is not reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    jobs: PathBuf,
    job: String,
}

impl Ledger {
    /// The ledger of `job`, whose records are in `jobs` (`Ctx::jobs`).
    pub fn of(jobs: &Path, job: &str) -> Self {
        Self { jobs: jobs.to_path_buf(), job: job.to_owned() }
    }

    fn file(&self, pgid: u32) -> PathBuf {
        self.jobs.join(format!("{}.group-{pgid}", self.job))
    }

    fn record(&self, pgid: u32) {
        let _ = std::fs::write(self.file(pgid), pgid.to_string());
    }

    fn forget(&self, pgid: u32) {
        let _ = std::fs::remove_file(self.file(pgid));
    }

    /// Kills every recorded group that still has a process in it (never this process's own group),
    /// and forgets them all: the runner that started them is gone, or the job is stopped. Returns how
    /// many groups were killed.
    pub fn kill_all(&self) -> usize {
        let prefix = format!("{}.group-", self.job);
        let Ok(entries) = std::fs::read_dir(&self.jobs) else { return 0 };
        let mut killed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(pgid) = name.strip_prefix(&prefix).and_then(|n| n.parse::<u32>().ok()) else { continue };
            if kill_group_if_any(pgid) {
                killed += 1;
            }
            self.forget(pgid);
        }
        killed
    }
}

/// Kills group `pgid` when a process is still in it, and it is not this process's own; whether it did.
pub fn kill_group_if_any(pgid: u32) -> bool {
    let own = rustix::process::getpgrp().as_raw_nonzero().get().cast_unsigned();
    pgid != own && crate::os::group_has_members(pgid) && crate::os::kill_group(pgid).is_ok()
}

#[derive(Debug)]
pub struct Supervised {
    child: Child,
    group: Group,
    exited: Option<ExitStatus>,
}

impl Supervised {
    /// Starts `cmd` in `group`; a group of its own is recorded in its ledger until it is done.
    pub fn spawn(cmd: &mut Command, group: Group) -> io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        if matches!(group, Group::Own(_)) {
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        if let Group::Own(ledger) = &group {
            ledger.record(child.id());
        }
        Ok(Self { child, group, exited: None })
    }

    /// The child, for its pipes.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kills it (its whole group when it leads one) and reaps it. A group of its own is then done.
    pub fn kill(&mut self) {
        if self.exited.is_none() {
            if matches!(self.group, Group::Own(_)) {
                let _ = crate::os::kill_group(self.child.id());
            }
            let _ = self.child.kill();
            #[allow(clippy::disallowed_methods, reason = "the supervisor reaps a child it has just killed")]
            let status = self.child.wait();
            self.exited = status.ok();
        }
        self.done();
    }

    /// Its own group outlives it in nothing: whatever is left in it (a helper it started) is killed,
    /// and the group is forgotten.
    fn done(&self) {
        if let Group::Own(ledger) = &self.group {
            kill_group_if_any(self.child.id());
            ledger.forget(self.child.id());
        }
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
                self.done();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn own(dir: &crate::config::tempdir::Dir) -> Group {
        Group::Own(Ledger::of(dir.path(), "t"))
    }

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
        let dir = crate::config::tempdir::Dir::new();
        let mut s = Supervised::spawn(&mut sh("exec >&-; sleep 30"), own(&dir)).unwrap();
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
        let mut s = Supervised::spawn(&mut sh(&format!("sleep 30 & echo $! >{}", pid.display())), own(&dir)).unwrap();
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

    /// PR #39 fourth review: a group of its own is recorded while it runs, so whoever sees the runner
    /// die kills it; one that is done, or whose group has emptied, is forgotten and nothing is signalled.
    #[test]
    fn a_running_handler_group_is_recorded_and_killed_by_whoever_sees_the_runner_go() {
        let dir = crate::config::tempdir::Dir::new();
        let ledger = Ledger::of(dir.path(), "t");
        let s = Supervised::spawn(&mut sh("sleep 30 & sleep 30"), own(&dir)).unwrap();
        let pgid = s.child.id();
        assert!(dir.path().join(format!("t.group-{pgid}")).exists(), "recorded while it runs");
        // The runner is gone without dropping it: whoever sees that kills the group, helper and all.
        std::mem::forget(s);
        assert_eq!(ledger.kill_all(), 1);
        assert!(eventually(|| !crate::os::group_has_members(pgid)), "the whole group went");
        assert!(!dir.path().join(format!("t.group-{pgid}")).exists(), "and is forgotten");
        // A record whose group has emptied signals nothing.
        std::fs::write(dir.path().join(format!("t.group-{pgid}")), "").unwrap();
        assert_eq!(ledger.kill_all(), 0);
        // One that ended on its own forgot itself.
        let mut done = Supervised::spawn(&mut sh("true"), own(&dir)).unwrap();
        assert!(done.finish(|| true, || None::<()>).unwrap().unwrap().success());
        assert!(!dir.path().join(format!("t.group-{}", done.child.id())).exists());
    }

    /// Done on its own once it exited and its output closed; dropped unreaped, it is killed.
    #[test]
    fn a_child_ends_on_its_own_and_one_dropped_is_killed() {
        let dir = crate::config::tempdir::Dir::new();
        let mut s = Supervised::spawn(&mut sh("echo hi"), own(&dir)).unwrap();
        let mut out = s.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
        assert!(s.finish(|| reader.is_finished(), || None::<()>).unwrap().unwrap().success());
        let s = Supervised::spawn(&mut sh("sleep 30"), Group::Runner).unwrap();
        let pid = s.child.id().to_string();
        drop(s);
        assert!(eventually(|| gone(&pid)), "dropped, it was killed and reaped");
    }
}
