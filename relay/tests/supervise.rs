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

/// Whether `pid` has gone (or is a zombie no one has reaped yet). It must be a pid: an empty one
/// would read `/proc//stat`, which is `/proc/stat`, and never be gone.
fn gone(pid: &str) -> bool {
    let pid: u32 = pid.parse().unwrap_or_else(|_| panic!("not a pid: {pid:?}"));
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
/// killed with SIGKILL. A handler killed as it writes a pid file leaves it empty, which names no
/// process (it once named `/proc/stat`, and failed "at-once" now and then under load).
#[test]
fn a_handler_goes_with_its_runner_however_soon_the_runner_is_killed() {
    let dir = Dir::new("runner");
    // How long the runner waits before it is killed, and whether the handler holds its grandchild's
    // pid file open, created and still empty, as it is killed: the moment "at-once" can only hit by chance.
    for (tag, wait_for, hold) in
        [("at-once", None, ""), ("mid-write", Some("-e"), "sleep 300; "), ("working", Some("-s"), "")]
    {
        let (sup, handler, grandchild) =
            (dir.join(&format!("{tag}.sup")), dir.join(&format!("{tag}.handler")), dir.join(&format!("{tag}.gc")));
        let inner = format!(
            "echo $$ >{}; sleep 300 & {{ {hold}echo $!; }} >{}; sleep 300",
            handler.display(),
            grandchild.display()
        );
        let wait = wait_for
            .map_or(String::new(), |t| format!("while [ ! {t} {} ]; do sleep 0.05; done; ", grandchild.display()));
        let runner =
            format!("\"$0\" supervise --parent $$ -- sh -c '{inner}' & echo $! >{}; {wait}kill -9 $$", sup.display());
        let status = Command::new("sh").arg("-c").arg(&runner).arg(RELAY).status().unwrap();
        assert!(!status.success());
        let sup = read_pid(&sup);
        assert!(eventually(|| gone(&sup)), "{tag}: the supervisor ended");
        // A pid file names a pid once its line is whole. One created but not yet written (the handler
        // killed between `>file` and `echo`) names none; the group check below covers what it started.
        for f in [&handler, &grandchild] {
            if let Some(pid) = std::fs::read_to_string(f).ok().filter(|s| s.ends_with('\n')) {
                assert!(eventually(|| gone(pid.trim())), "{tag}: {} went with the runner", f.display());
            }
        }
        match tag {
            "mid-write" => assert_eq!(std::fs::read_to_string(&grandchild).unwrap(), "", "killed as it wrote the pid"),
            "working" => assert!(grandchild.exists(), "the handler was at work when its runner was killed"),
            _ => {}
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
    assert_eq!(status.code(), Some(137), "it ended its tree and exited: {status}");
    assert!(gone(&grandchild), "the grandchild was ended and reaped before it exited");
}

/// Whether a live process holds the lock `path` (exclusive or shared).
fn lock_held(path: &Path) -> bool {
    let f = std::fs::File::options().read(true).write(true).open(path).unwrap();
    matches!(
        rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK)
    )
}

/// Copilot on PR #63, third pass, finding 2: a decision's seat must stay taken until its session's
/// whole tree has stopped, not only while its runner lives. `--hold` gives the supervisor a shared
/// hold on the seat's lock for its whole life, which ends only after every process of the tree has
/// (fourth pass: it is not handed down, which a sandbox would not keep, nor a process that closes it).
#[test]
fn a_held_seat_lock_lasts_as_long_as_the_supervised_tree() {
    let d = Dir::new("hold");
    let (lock, pid) = (d.join("seat.lock"), d.join("pid"));
    std::fs::write(&lock, "").unwrap();
    let script = format!("echo $$ > {}; exec sleep 30", pid.display());
    let mut sup = Command::new(RELAY)
        .args(["supervise", "--parent", &std::process::id().to_string(), "--hold"])
        .arg(&lock)
        .args(["--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    let child = read_pid(&pid);
    assert!(lock_held(&lock), "held while the session runs");
    assert!(Command::new("kill").args(["-TERM", &sup.id().to_string()]).status().unwrap().success());
    sup.wait().unwrap();
    assert!(gone(&child), "the tree has ended");
    assert!(!lock_held(&lock), "and its hold with it");
}

/// Copilot on PR #63, fourth pass, finding 2: a child that starts a grandchild in a session of its
/// own (`setsid`) leaves the supervisor's group, so ending the group missed it, and the seat was
/// given back while it ran. The supervisor is a subreaper: the orphan comes back to it, and it ends
/// and reaps every descendant before it exits and lets go of the seat's lock.
#[test]
fn a_setsid_grandchild_holds_the_seat_until_it_has_ended_and_never_outlives_the_supervisor() {
    let d = Dir::new("setsid");
    let (lock, grandchild) = (d.join("seat.lock"), d.join("grandchild"));
    std::fs::write(&lock, "").unwrap();
    let script = format!("setsid sh -c 'echo $$ > {}; exec sleep 30' & sleep 1; exit 0", grandchild.display());
    let mut sup = Command::new(RELAY)
        .args(["supervise", "--parent", &std::process::id().to_string(), "--hold"])
        .arg(&lock)
        .args(["--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    let g = read_pid(&grandchild);
    assert!(lock_held(&lock), "the seat is held while the session runs");
    let status = sup.wait().unwrap();
    assert!(gone(&g), "the setsid grandchild was ended before the supervisor exited ({status})");
    assert!(!lock_held(&lock), "and only then was the seat let go");
}

/// Whether a live process has `arg` among its arguments.
fn running_with_arg(arg: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().filter_map(Result::ok).any(|e| {
        let name = e.file_name();
        let Some(pid) = name.to_str().filter(|n| n.bytes().all(|b| b.is_ascii_digit())) else { return false };
        let cmdline = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        cmdline.split(|&b| b == 0).any(|a| a == arg.as_bytes()) && !gone(pid)
    })
}

fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-pid", "true"])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Copilot on PR #63, fourth pass, finding 1: a sandboxed session's processes are bwrap's, in a pid
/// namespace of their own; when its runner is killed, the seat must stay taken until the sandbox has
/// exited, not only until the supervisor sees its runner gone.
#[test]
fn a_sandboxed_session_whose_runner_is_killed_holds_the_seat_until_the_sandbox_has_exited() {
    if !bwrap_works() {
        return;
    }
    let d = Dir::new("sandbox");
    let lock = d.join("seat.lock");
    std::fs::write(&lock, "").unwrap();
    // A marker no other process on the machine has among its arguments.
    let marker = format!("31.{}", std::process::id());
    let sandbox = format!(
        "bwrap --ro-bind / / --dev /dev --unshare-pid --die-with-parent --new-session sh -c 'sleep {marker} & wait'"
    );
    let runner = format!("{RELAY} supervise --parent $$ --hold {} -- {sandbox} & wait", lock.display());
    let mut runner = Command::new("sh").args(["-c", &runner]).spawn().unwrap();
    assert!(eventually(|| running_with_arg(&marker)), "the sandboxed session runs");
    assert!(lock_held(&lock));
    runner.kill().unwrap();
    runner.wait().unwrap();
    // Look as often as a dispatcher might: the moment the seat is free, nothing of the sandbox runs.
    let start = Instant::now();
    while lock_held(&lock) {
        assert!(start.elapsed() < Duration::from_secs(10), "the seat is let go once the sandbox has ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!running_with_arg(&marker), "the seat was free while the sandbox still ran");
}
