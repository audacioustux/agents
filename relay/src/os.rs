//! The Linux features relay is built on: flock for single-holder roles,
//! inotify as the bus doorbell, /proc for process identity, and the
//! machine gauges (load, free disk) that hold back new sessions.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{FlockOperation, flock, inotify};
use rustix::process::{Pid, Signal, kill_process_group};

/// An exclusive role, such as "the dispatcher". Its descriptor is opened close-on-exec, so no child
/// that execs keeps it, and dropping the role unlocks it (`LOCK_UN`, [`Role::release`]) rather than
/// only closing its descriptor: a flock lives as long as any copy of its descriptor does, and a child
/// another thread is spawning holds a copy from its fork to its exec, so a role only closed could read
/// as busy, or stay held, for that window. The kernel drops the lock too when the holder dies.
///
/// The one exception is a role shared with a child ([`Role::share`]): that child's process tree holds
/// it on, and dropping the role then only closes this process's descriptor.
#[derive(Debug)]
pub struct Role {
    file: File,
    shared: bool,
}

impl Role {
    /// `None` if another live process holds the role.
    pub fn try_take(path: &Path) -> Result<Option<Self>> {
        Self::try_lock(Self::open(path)?)
    }

    /// Waits until the role is free, then takes it.
    pub fn take(path: &Path) -> Result<Self> {
        let f = Self::open(path)?;
        flock(&f, FlockOperation::LockExclusive).context("flock")?;
        Ok(Self { file: f, shared: false })
    }

    fn try_lock(file: File) -> Result<Option<Self>> {
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Self { file, shared: false })),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(e) => Err(e).context("flock"),
        }
    }

    /// Gives the role up now, whatever else holds a copy of the descriptor. A process that forks while
    /// the role is held (any other thread's `Command`) has a copy until its `exec` closes it, and the
    /// kernel keeps the lock for as long as one copy lives: a role only closed and taken again at once
    /// can find itself held. Dropping a role does this; a role shared with a child ([`Role::share`]) is
    /// that child's to give up, and dropping it does not.
    pub fn release(&self) {
        unlock(&self.file);
    }

    /// A copy of the role for one child to inherit as its stdin (an empty file, read as at its end).
    /// The role is then held until that child, and everything that inherits its stdin, has ended,
    /// even past this process: a role whose work runs in a process tree is that tree's, and dropping
    /// this one no longer unlocks it.
    pub fn share(&mut self) -> Result<std::process::Stdio> {
        let copy = self.file.try_clone().context("sharing a role")?;
        self.shared = true;
        Ok(copy.into())
    }

    /// A copy of the descriptor as a child forked (and not yet exec'd) by another thread has one.
    #[cfg(test)]
    pub(crate) fn forked_copy(&self) -> OwnedFd {
        self.file.try_clone().unwrap().into()
    }

    /// Opens (creating) a lock file. std opens every file close-on-exec; the flag is named here too, so
    /// that no lock descriptor ever outlives an exec, whatever opens it.
    fn open(path: &Path) -> Result<File> {
        File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::CLOEXEC.bits().cast_signed())
            .open(path)
            .with_context(|| format!("opening {}", path.display()))
    }
}

impl Drop for Role {
    fn drop(&mut self) {
        if !self.shared {
            self.release();
        }
    }
}

/// Unlocks `f`'s lock for every copy of its descriptor. A lock the kernel has already dropped, or a
/// descriptor that is gone, is released enough.
fn unlock(f: &File) {
    let _ = flock(f, FlockOperation::Unlock);
}

/// A shared hold on a seat's lock file ([`seat_held`]): the booker of a session that is no job's own
/// run takes one before it records the seat, and the session's supervisor another for its whole life,
/// which ends only once every process of the session's tree has (`supervised::supervise`), so the
/// seat stays taken until then, however the booker ends. Opened close-on-exec as a [`Role`] is, and
/// unlocked when dropped, so a dropped hold never reads as a taken seat through a forked child's copy.
#[derive(Debug)]
pub struct SeatHold(File);

impl SeatHold {
    /// Creates `path` if need be and holds it, shared (waiting out a [`seat_held`] look's moment).
    pub fn take(path: &Path) -> Result<Self> {
        let f = Role::open(path)?;
        flock(&f, FlockOperation::LockShared).context("flock")?;
        Ok(Self(f))
    }

    /// As [`Role::forked_copy`].
    #[cfg(test)]
    pub(crate) fn forked_copy(&self) -> OwnedFd {
        self.0.try_clone().unwrap().into()
    }
}

impl Drop for SeatHold {
    fn drop(&mut self) {
        unlock(&self.0);
    }
}

/// Whether a live process holds the seat lock `path` ([`SeatHold`]). A missing file is held by nobody.
/// The look takes the lock for a moment, as a [`Role`], so it is unlocked again however it ends.
pub fn seat_held(path: &Path) -> bool {
    let opened = File::options()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::CLOEXEC.bits().cast_signed())
        .open(path);
    let Ok(f) = opened else { return false };
    matches!(Role::try_lock(f), Ok(None))
}

/// Rings when files in a directory are written or created.
#[derive(Debug)]
pub struct Doorbell(OwnedFd);

impl Doorbell {
    /// As [`Doorbell::watch`], leaving out each directory that cannot be watched (another
    /// project's, unreadable), for a reader that names such projects itself.
    pub fn watch_readable(dirs: &[impl AsRef<Path>]) -> Result<Self> {
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK).context("inotify")?;
        let flags = inotify::WatchFlags::MODIFY | inotify::WatchFlags::CREATE | inotify::WatchFlags::MOVED_TO;
        for dir in dirs.iter().map(AsRef::as_ref) {
            let _ = inotify::add_watch(&fd, dir, flags);
        }
        Ok(Self(fd))
    }

    pub fn watch(dirs: &[impl AsRef<Path>]) -> Result<Self> {
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK).context("inotify")?;
        let flags = inotify::WatchFlags::MODIFY | inotify::WatchFlags::CREATE | inotify::WatchFlags::MOVED_TO;
        for dir in dirs.iter().map(AsRef::as_ref) {
            inotify::add_watch(&fd, dir, flags).with_context(|| format!("watching {}", dir.display()))?;
        }
        Ok(Self(fd))
    }

    /// Waits until a change or the timeout, then drains pending notices.
    /// Returns whether it rang.
    pub fn wait(&self, timeout: Duration) -> bool {
        let ts = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
        let rang = matches!(poll(&mut fds, Some(&ts)), Ok(n) if n > 0);
        let mut buf = [0u8; 4096];
        while matches!(rustix::io::read(&self.0, &mut buf), Ok(n) if n > 0) {}
        rang
    }
}

/// A process's start time in clock ticks since boot (field 22 of /proc/<pid>/stat).
/// Together with the pid it names one process: a reused pid has a different start time.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) is in parentheses and may contain spaces.
    let after = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // `after` starts at field 3 (state); a zombie has exited.
    if fields.first() == Some(&"Z") {
        return None;
    }
    fields.get(19)?.parse().ok()
}

/// Whether `pid` is still the process that started at `started`.
pub fn same_process(pid: u32, started: u64) -> bool {
    start_time(pid) == Some(started)
}

/// Sends `sig` to `pid` only while it is still the process that started at `started`, and says
/// whether it did. The signal goes through a pidfd opened before the check, so it reaches the
/// process checked, never one that has taken over its pid since.
pub fn signal_if_same(pid: u32, started: u64, sig: Signal) -> Result<bool> {
    let Some(p) = Pid::from_raw(i32::try_from(pid)?) else { return Ok(false) };
    let fd = match rustix::process::pidfd_open(p, rustix::process::PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::SRCH) => return Ok(false),
        Err(e) => return Err(e).context("pidfd_open"),
    };
    if !same_process(pid, started) {
        return Ok(false);
    }
    match rustix::process::pidfd_send_signal(&fd, sig) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(e) => Err(e).context("pidfd_send_signal"),
    }
}

/// The parent of `pid` while it lives (a zombie has none here).
pub fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.get(stat.rfind(')')? + 2..)?;
    let mut fields = after.split_whitespace();
    // `after` starts at field 3 (state); field 4 is the parent.
    if fields.next()? == "Z" {
        return None;
    }
    fields.next()?.parse().ok()
}

/// Every live descendant of `root`, by the parents `/proc` gives now.
pub fn descendants(root: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    let all: Vec<(u32, u32)> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| Some((pid, parent_of(pid)?)))
        .collect();
    let mut tree = vec![root];
    let mut i = 0;
    while let Some(&parent) = tree.get(i) {
        tree.extend(all.iter().filter(|&&(_, pp)| pp == parent).map(|&(pid, _)| pid));
        i += 1;
    }
    tree.remove(0);
    tree
}

/// SIGKILL to a process group: a supervisor relay started as a group leader and has not reaped, with
/// everything in its group.
pub fn kill_group(pid: u32) -> Result<()> {
    let pid = Pid::from_raw(i32::try_from(pid)?).context("pid 0")?;
    kill_process_group(pid, Signal::KILL).context("kill")
}

/// The path this binary was started from. Once an upgrade replaces it, Linux
/// names the running image `<path> (deleted)`; the path itself then holds
/// the new binary, which is the one to start.
pub fn own_binary() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let s = exe.to_string_lossy();
    Ok(s.strip_suffix(" (deleted)").map_or(exe.clone(), std::path::PathBuf::from))
}

/// When the installed binary was last replaced: a change means a newer
/// relay should take over from this process.
pub fn binary_version() -> Option<std::time::SystemTime> {
    own_binary().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

/// A time in this machine's zone (`TZ`, else `/etc/localtime`): every time relay shows or logs is
/// the reader's local time, labelled with its zone.
pub fn local(t: jiff::Timestamp) -> jiff::Zoned {
    t.to_zoned(jiff::tz::TimeZone::system())
}

/// Now, as a log line's local stamp.
pub fn stamp() -> String {
    local(jiff::Timestamp::now()).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// The terminal's columns and rows, when stdout is one.
pub fn terminal_size() -> Option<(usize, usize)> {
    let w = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    (w.ws_col > 0 && w.ws_row > 0).then(|| (usize::from(w.ws_col), usize::from(w.ws_row)))
}

/// 1-minute load average.
pub fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

/// The memory the kernel says a new workload could have without swapping (`MemAvailable` in
/// `/proc/meminfo`), in GiB. `None` where it cannot be read.
pub fn mem_available_gb() -> Option<f64> {
    mem_available_of(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// `MemAvailable` of a `/proc/meminfo` text (`MemAvailable:    8123456 kB`), in GiB.
fn mem_available_of(meminfo: &str) -> Option<f64> {
    let rest = meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?;
    let kib: f64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kib / (1024.0 * 1024.0))
}

/// Free space for unprivileged users on the filesystem that holds, or will
/// hold, `path` (its nearest existing ancestor), in GB.
pub fn free_disk_gb(path: &Path) -> Option<f64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let s = rustix::fs::statvfs(existing).ok()?;
    #[allow(clippy::cast_precision_loss)]
    Some((s.f_bavail * s.f_frsize) as f64 / 1e9)
}

/// Disk space a tree holds, from a recursive stat walk that does not follow
/// symlinks and counts a hard-linked file once, in bytes allocated. Stops when
/// `within` has passed: then the second value is `false` and the first is a
/// lower bound.
pub fn dir_size(path: &Path, within: Duration) -> (u64, bool) {
    use std::os::unix::fs::MetadataExt;
    let end = std::time::Instant::now() + within;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    let mut todo = vec![path.to_path_buf()];
    while let Some(dir) = todo.pop() {
        if std::time::Instant::now() >= end {
            return (total, false);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else { continue };
            if meta.is_dir() {
                total += meta.blocks() * 512;
                todo.push(entry.path());
            } else if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
                total += meta.blocks() * 512;
            }
        }
    }
    (total, true)
}

/// The disk the directories hold together, each walked for at most `within` (a lower bound past it).
pub fn dirs_size(paths: &[std::path::PathBuf], within: Duration) -> u64 {
    paths.iter().map(|p| dir_size(p, within).0).sum()
}

/// A size for people and boards: `340K`, `12G`, `1.5G` (decimal units).
pub fn size_str(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    for (unit, scale) in [("G", 1e9), ("M", 1e6), ("K", 1e3)] {
        if b >= scale {
            let n = b / scale;
            let digits = if n < 10.0 { format!("{n:.1}") } else { format!("{n:.0}") };
            return format!("{}{unit}", digits.strip_suffix(".0").unwrap_or(&digits));
        }
    }
    format!("{bytes}B")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    #[test]
    fn sizes_are_summed_without_following_links_and_read_for_people() {
        let d = Dir::new();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/f"), vec![1u8; 20_000]).unwrap();
        std::fs::hard_link(d.path().join("a/b/f"), d.path().join("a/g")).unwrap();
        std::os::unix::fs::symlink("/usr", d.path().join("a/link")).unwrap();
        let (bytes, done) = dir_size(d.path(), Duration::from_secs(10));
        assert!(done && (20_000..60_000).contains(&bytes), "{bytes}: the file once, the link not followed");
        assert!(!dir_size(d.path(), Duration::ZERO).1, "out of time: a lower bound");
        assert_eq!(dir_size(&d.path().join("none"), Duration::from_secs(1)), (0, true));
        assert_eq!([999, 340_000, 1_500_000_000, 52_000_000_000].map(size_str), ["999B", "340K", "1.5G", "52G"]);
    }

    #[test]
    fn role_is_exclusive_and_released() {
        let d = Dir::new();
        let p = d.path().join("r.lock");
        let held = Role::try_take(&p).unwrap();
        assert!(held.is_some());
        // flock locks belong to the open file description, so a second open conflicts.
        assert!(Role::try_take(&p).unwrap().is_none());
        drop(held);
        // Another test thread may be between fork and exec, its child briefly sharing the open file
        // description: the drop unlocks it for every copy, so it is free at once.
        assert!(Role::try_take(&p).unwrap().is_some(), "the role is free once its holder is dropped");
    }

    /// Whether `fd` is close-on-exec.
    fn cloexec(fd: impl std::os::fd::AsFd) -> bool {
        rustix::io::fcntl_getfd(fd).unwrap().contains(rustix::io::FdFlags::CLOEXEC)
    }

    /// Every lock descriptor is opened close-on-exec: no exec'd child keeps one.
    #[test]
    fn every_lock_descriptor_is_close_on_exec() {
        let d = Dir::new();
        let role = Role::try_take(&d.path().join("a.lock")).unwrap().unwrap();
        assert!(cloexec(&role.file), "try_take");
        assert!(cloexec(&Role::take(&d.path().join("b.lock")).unwrap().file), "take");
        assert!(cloexec(&SeatHold::take(&d.path().join("c.lock")).unwrap().0), "a seat hold");
        assert!(!seat_held(&d.path().join("a.lock").with_extension("none")));
        let looked = File::options().read(true).write(true).open(d.path().join("a.lock")).unwrap();
        assert!(cloexec(&looked), "std's open, as seat_held's look uses");
    }

    /// A role taken without waiting (the dispatcher's, the gc's, the quota drainer's, the cost index's,
    /// a held job's, a seat file's last look): dropped while a forked child holds a copy, it is free.
    #[test]
    fn a_role_taken_at_once_is_free_once_dropped_though_a_forked_child_holds_a_copy() {
        let d = Dir::new();
        let p = d.path().join("dispatch.lock");
        let role = Role::try_take(&p).unwrap().unwrap();
        let child = role.forked_copy();
        drop(role);
        assert!(Role::try_take(&p).unwrap().is_some(), "free though the copy lives");
        drop(child);
    }

    /// A role waited for (the jobs lock of queue, land, decide and dispatch; the seats lock; the quota
    /// locks; the host's programs record): the same.
    #[test]
    fn a_role_waited_for_is_free_once_dropped_though_a_forked_child_holds_a_copy() {
        let d = Dir::new();
        let p = d.path().join("jobs.lock");
        let role = Role::take(&p).unwrap();
        let child = role.forked_copy();
        drop(role);
        assert!(Role::try_take(&p).unwrap().is_some(), "free though the copy lives");
        drop(child);
    }

    /// A seat hold (a session's booker and its supervisor): once dropped, the seat is free, and the last
    /// holder may remove its lock file, though a forked child holds a copy.
    #[test]
    fn a_seat_hold_is_free_once_dropped_though_a_forked_child_holds_a_copy() {
        let d = Dir::new();
        let p = d.path().join("claim.lock");
        let hold = SeatHold::take(&p).unwrap();
        let child = hold.forked_copy();
        assert!(seat_held(&p));
        drop(hold);
        assert!(!seat_held(&p), "free though the copy lives");
        // The look took the lock for a moment, and gave it up: a hold and a role are had at once.
        assert!(Role::try_take(&p).unwrap().is_some());
        drop(child);
    }

    /// The exception: a role shared with a child (a development-environment warm-up's) is that child's
    /// tree's, and stays held past its owner's drop until the child's copy is gone.
    #[test]
    fn a_role_shared_with_a_child_is_held_until_the_child_lets_go() {
        let d = Dir::new();
        let p = d.path().join("devshell.lock");
        let mut role = Role::try_take(&p).unwrap().unwrap();
        let stdin = role.share().unwrap();
        drop(role);
        assert!(Role::try_take(&p).unwrap().is_none(), "the child holds it");
        drop(stdin);
        assert!(Role::try_take(&p).unwrap().is_some(), "free once the child's copy is gone");
    }

    /// Threads that take and drop every kind of role while others spawn children (each forking with a
    /// copy of every descriptor until its exec) never find a role busy after its owner dropped it.
    #[test]
    fn roles_dropped_while_other_threads_spawn_children_are_never_busy() {
        use std::sync::atomic::{AtomicBool, Ordering};
        const TAKERS: usize = 4;
        const SPAWNERS: usize = 4;
        const ROUNDS: usize = 300;
        /// Stops the spawners however the takers end, a panic too, so the scope never waits on them.
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let d = Dir::new();
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let _stop = Stop(&stop);
            for _ in 0..SPAWNERS {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("true")
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                    }
                });
            }
            let takers: Vec<_> = (0..TAKERS)
                .map(|t| {
                    let p = d.path().join(format!("{t}.lock"));
                    s.spawn(move || {
                        // Nobody else takes this thread's role: busy is only ever a dropped owner's lock.
                        let mut busy = 0;
                        for round in 0..ROUNDS {
                            match round % 3 {
                                0 => busy += usize::from(Role::try_take(&p).unwrap().is_none()),
                                1 => drop(Role::take(&p).unwrap()),
                                _ => drop(SeatHold::take(&p).unwrap()),
                            }
                            busy += usize::from(Role::try_take(&p).unwrap().is_none());
                        }
                        busy
                    })
                })
                .collect();
            let busy: usize = takers.into_iter().map(|t| t.join().unwrap()).sum();
            assert_eq!(busy, 0, "a role read as busy after its owner dropped it");
        });
    }

    #[test]
    fn doorbell_rings_on_append() {
        let d = Dir::new();
        let bell = Doorbell::watch(&[d.path()]).unwrap();
        assert!(!bell.wait(Duration::from_millis(10)));
        std::fs::write(d.path().join("x"), "1").unwrap();
        assert!(bell.wait(Duration::from_secs(1)));
    }

    /// PR #39 fifth review, gap 2: a stored pid is signalled only through a pidfd checked to be the
    /// process recorded: one whose start time differs (a reused pid) is left alone.
    #[test]
    fn a_stored_pid_is_signalled_only_while_it_is_the_process_recorded() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let started = start_time(pid).unwrap();
        assert!(!signal_if_same(pid, started + 1, Signal::KILL).unwrap(), "another process's start time");
        std::thread::sleep(Duration::from_millis(50));
        assert!(same_process(pid, started), "left alone");
        assert!(signal_if_same(pid, started, Signal::KILL).unwrap());
        #[allow(clippy::disallowed_methods, reason = "a test reaps the child it killed")]
        let status = child.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&status), Some(9));
        assert!(!signal_if_same(pid, started, Signal::KILL).unwrap(), "gone, it is not signalled");
    }

    #[test]
    fn process_identity() {
        let me = std::process::id();
        let t = start_time(me).unwrap();
        assert!(same_process(me, t));
        assert!(!same_process(me, t + 1));
        assert!(load_average().is_some());
        assert!(mem_available_gb().is_some_and(|gb| gb > 0.0));
        let text = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    6291456 kB\nBuffers: 2 kB\n";
        assert_eq!(mem_available_of(text), Some(6.0), "kB there are KiB");
        assert_eq!(mem_available_of("MemTotal: 5 kB\n"), None, "a kernel that does not say");
        assert!(own_binary().unwrap().exists());
        assert!(free_disk_gb(Path::new("/")).is_some());
        assert!(free_disk_gb(Path::new("/nonexistent/not/yet")).is_some(), "measured where it will be made");
    }
}
