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

/// An exclusive role, such as "the dispatcher". The kernel drops it when the
/// holder dies, and the descriptor is close-on-exec, so children never inherit it.
#[derive(Debug)]
pub struct Role(#[allow(dead_code)] File);

impl Role {
    /// `None` if another live process holds the role.
    pub fn try_take(path: &Path) -> Result<Option<Self>> {
        let f = Self::open(path)?;
        match flock(&f, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Self(f))),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(e) => Err(e).context("flock"),
        }
    }

    /// Waits until the role is free, then takes it.
    pub fn take(path: &Path) -> Result<Self> {
        let f = Self::open(path)?;
        flock(&f, FlockOperation::LockExclusive).context("flock")?;
        Ok(Self(f))
    }

    fn open(path: &Path) -> Result<File> {
        File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))
    }
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

/// SIGTERM to a process group (relay starts each session runner as a group leader).
pub fn terminate_group(pid: u32) -> Result<()> {
    let pid = Pid::from_raw(i32::try_from(pid)?).context("pid 0")?;
    kill_process_group(pid, Signal::TERM).context("kill")
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
        // description (the descriptor is close-on-exec, so only until the exec): the lock frees then.
        let freed = (0..200).any(|_| {
            Role::try_take(&p).unwrap().is_some() || {
                std::thread::sleep(Duration::from_millis(10));
                false
            }
        });
        assert!(freed, "the role is free once its holder is dropped");
    }

    #[test]
    fn doorbell_rings_on_append() {
        let d = Dir::new();
        let bell = Doorbell::watch(&[d.path()]).unwrap();
        assert!(!bell.wait(Duration::from_millis(10)));
        std::fs::write(d.path().join("x"), "1").unwrap();
        assert!(bell.wait(Duration::from_secs(1)));
    }

    #[test]
    fn process_identity() {
        let me = std::process::id();
        let t = start_time(me).unwrap();
        assert!(same_process(me, t));
        assert!(!same_process(me, t + 1));
        assert!(load_average().is_some());
        assert!(own_binary().unwrap().exists());
        assert!(free_disk_gb(Path::new("/")).is_some());
        assert!(free_disk_gb(Path::new("/nonexistent/not/yet")).is_some(), "measured where it will be made");
    }
}
