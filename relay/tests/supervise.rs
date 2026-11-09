//! `relay supervise`, the leader of every session's and handler's process group, run as the real
//! executable cargo builds from these sources (PR #39 sixth review, issue 1: no test resolves a
//! sibling artifact). The runner's side (`supervised::Supervised`) is unit-tested in the crate.

#![allow(clippy::disallowed_methods, reason = "a test waits on the processes it starts itself")]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const RELAY: &str = env!("CARGO_BIN_EXE_relay");

/// A scratch directory of the test's own, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("relay-supervise-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }

    fn join(&self, f: &str) -> PathBuf {
        self.0.join(f)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `pid` has gone (or is a zombie no one has reaped yet).
fn gone(pid: &str) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    stat.is_empty() || stat.contains(") Z ")
}

fn eventually(f: impl Fn() -> bool) -> bool {
    (0..100).any(|_| {
        let ok = f();
        if !ok {
            std::thread::sleep(Duration::from_millis(100));
        }
        ok
    })
}

fn read_pid(path: &Path) -> String {
    assert!(eventually(|| std::fs::read_to_string(path).is_ok_and(|s| s.ends_with('\n'))), "{}", path.display());
    std::fs::read_to_string(path).unwrap().trim().to_owned()
}

/// The live processes in process group `pgid`.
fn group_members(pgid: u32) -> Vec<u32> {
    let entries = std::fs::read_dir("/proc").unwrap();
    entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            let mut fields = stat.get(stat.rfind(')')? + 2..)?.split_whitespace();
            let state = fields.next()?;
            (state != "Z" && fields.nth(1)?.parse::<u32>().ok()? == pgid).then_some(pid)
        })
        .collect()
}

/// `relay supervise` for this test process, running `sh -c script`, with its stdout read to the end:
/// how it exited and how long until its output closed too (bounded at 20 s).
fn supervise(script: &str) -> (ExitStatus, Duration) {
    let mut child = Command::new(RELAY)
        .args(["supervise", "--parent", &std::process::id().to_string(), "--", "sh", "-c", script])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut out, &mut Vec::new()));
    let started = Instant::now();
    while !reader.is_finished() && started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let closed = started.elapsed();
    if !reader.is_finished() {
        let _ = child.kill();
    }
    (child.wait().unwrap(), closed)
}

/// It exits as its handler did, a program that cannot start with 127; and a background grandchild,
/// even one holding the handler's output, goes when the handler exits.
#[test]
fn it_exits_as_its_handler_did_and_ends_what_the_handler_left() {
    let dir = Dir::new("exits");
    assert_eq!(supervise("exit 3").0.code(), Some(3));
    let missing = Command::new(RELAY)
        .args(["supervise", "--parent", &std::process::id().to_string(), "--", "no-such-program-anywhere"])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(missing.code(), Some(127));

    let pid = dir.join("left.pid");
    let (status, closed) = supervise(&format!("sleep 300 & echo $! >{}; echo answer", pid.display()));
    assert!(status.success(), "{status}");
    assert!(closed < Duration::from_secs(5), "the grandchild holding its output was ended: {closed:?}");
    assert!(eventually(|| gone(&read_pid(&pid))), "the grandchild went when the handler exited");
}

/// PR #39 fifth review, gaps 1 and 3: nothing is recorded, and a runner killed at once after it
/// started a handler (before the supervisor has even looked) or while the handler works takes the
/// handler's whole group with it. The "runner" here is a shell that starts the supervisor and is
/// killed with SIGKILL.
#[test]
fn a_handler_goes_with_its_runner_however_soon_the_runner_is_killed() {
    let dir = Dir::new("runner");
    for wait_for_handler in [false, true] {
        let tag = if wait_for_handler { "working" } else { "at-once" };
        let (sup, handler, grandchild) =
            (dir.join(&format!("{tag}.sup")), dir.join(&format!("{tag}.handler")), dir.join(&format!("{tag}.gc")));
        let inner = format!("echo $$ >{}; sleep 300 & echo $! >{}; sleep 300", handler.display(), grandchild.display());
        let wait = if wait_for_handler {
            format!("while [ ! -s {} ]; do sleep 0.05; done; ", grandchild.display())
        } else {
            String::new()
        };
        let runner =
            format!("\"$0\" supervise --parent $$ -- sh -c '{inner}' & echo $! >{}; {wait}kill -9 $$", sup.display());
        let status = Command::new("sh").arg("-c").arg(&runner).arg(RELAY).status().unwrap();
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
        assert!(eventually(|| group_members(pgid).is_empty()), "{tag}: nothing is left in its group");
    }
}

/// Told to stop (TERM), it ends its whole group, itself included.
#[test]
fn a_terminated_supervisor_ends_its_group() {
    let dir = Dir::new("term");
    let pid = dir.join("gc.pid");
    let mut child = Command::new(RELAY)
        .args(["supervise", "--parent", &std::process::id().to_string(), "--", "sh", "-c"])
        .arg(format!("sleep 300 & echo $! >{}; sleep 300", pid.display()))
        .spawn()
        .unwrap();
    let grandchild = read_pid(&pid);
    assert!(Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap().success());
    let status = child.wait().unwrap();
    assert_eq!(std::os::unix::process::ExitStatusExt::signal(&status), Some(9), "{status}");
    assert!(eventually(|| gone(&grandchild)), "the grandchild went with the group");
}
