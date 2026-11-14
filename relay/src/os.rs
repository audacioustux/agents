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

    /// A copy of the role for one child to inherit as its stdin (an empty file, read as at its end).
    /// The role is then held until that child, and everything that inherits its stdin, has ended,
    /// even past this process: a role whose work runs in a process tree is that tree's.
    pub fn share(&self) -> Result<std::process::Stdio> {
        Ok(self.0.try_clone().context("sharing a role")?.into())
    }

    fn open(path: &Path) -> Result<File> {
        File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))
    }
}

/// A shared hold on a seat's lock file ([`seat_held`]): the booker of a session that is no job's own
/// run takes one before it records the seat, and the session's supervisor another for its whole life,
/// which ends only once every process of the session's tree has (`supervised::supervise`), so the
/// seat stays taken until then, however the booker ends.
#[derive(Debug)]
pub struct SeatHold(#[allow(dead_code)] File);

impl SeatHold {
    /// Creates `path` if need be and holds it, shared (waiting out a [`seat_held`] look's moment).
    pub fn take(path: &Path) -> Result<Self> {
        let f = Role::open(path)?;
        flock(&f, FlockOperation::LockShared).context("flock")?;
        Ok(Self(f))
    }
}

/// Whether a live process holds the seat lock `path` ([`SeatHold`]). A missing file is held by nobody.
pub fn seat_held(path: &Path) -> bool {
    let Ok(f) = File::options().read(true).write(true).open(path) else { return false };
    matches!(flock(&f, FlockOperation::NonBlockingLockExclusive), Err(rustix::io::Errno::WOULDBLOCK))
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

/// The disk a running job's directories hold together, as its disk use: each counted whole (in bytes
/// allocated, no symlink followed, a hard-linked file once), but for `apart`, the copy of the linked
/// caches this run was given in its clone ([`crate::sandbox::farm`]), which is the project's cache and
/// not the job's work. Only that directory, by its path: where no copy was given, or anywhere else (a
/// directory of that name in the job's `/tmp`, or deeper in its tree), what a job writes is its own and
/// counts. Each is walked for at most `within` (a lower bound past it).
pub fn dirs_size(paths: &[std::path::PathBuf], apart: Option<&Path>, within: Duration) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::HashSet::new();
    let mut once = |meta: &std::fs::Metadata| meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino()));
    paths.iter().map(|p| walk(p, within, apart, &mut once).0).sum()
}

/// What removing places frees, one after another, in bytes allocated. A file's blocks go with its last
/// link only: a package in a copy of the linked caches is hard-linked into every tree installed from it,
/// and removing the copy frees nothing of it while such a tree stays (nor does removing the tree while
/// the copy stays). So a file with several links counts once every one of them is among what was
/// removed, with the place that held the last. What gc counts.
#[derive(Debug, Default)]
pub struct Freed {
    /// The links already removed of each file that has several, by device and inode.
    gone: std::collections::HashMap<(u64, u64), u64>,
}

/// The links a place holds of each file that has several: what [`Freed::removed`] is told once it is gone.
#[derive(Debug, Default)]
pub struct Links(std::collections::HashMap<(u64, u64), u64>);

impl Freed {
    /// The bytes removing `path` frees, after every place [`Freed::removed`] before it: a tree walked
    /// without following a symlink, its root included (one there, or a file, is itself, never what it
    /// leads to). Stops when `within` has passed: then the second value is `false` and the first is a
    /// lower bound. With the links it holds, for [`Freed::removed`].
    pub fn by_removing(&self, path: &Path, within: Duration) -> (u64, bool, Links) {
        use std::os::unix::fs::MetadataExt;
        let mut links = Links::default();
        let mut last = |meta: &std::fs::Metadata| {
            if meta.nlink() < 2 {
                return true;
            }
            let id = (meta.dev(), meta.ino());
            let here = links.0.entry(id).or_insert(0);
            *here += 1;
            self.gone.get(&id).copied().unwrap_or(0) + *here == meta.nlink()
        };
        let (bytes, whole) = walk(path, within, None, &mut last);
        (bytes, whole, links)
    }

    /// The place those `links` were counted in is gone.
    pub fn removed(&mut self, links: Links) {
        for (id, n) in links.0 {
            *self.gone.entry(id).or_insert(0) += n;
        }
    }
}

/// The bytes allocated to the tree at `path`, from a recursive stat walk that follows no symlink: every
/// directory but `apart`, and each other entry `counts` says to count (asked once per link of a file).
/// Stops when `within` has passed: then the second value is `false` and the first is a lower bound.
fn walk(
    path: &Path,
    within: Duration,
    apart: Option<&Path>,
    counts: &mut dyn FnMut(&std::fs::Metadata) -> bool,
) -> (u64, bool) {
    use std::os::unix::fs::MetadataExt;
    // The root too is looked at as itself: a symlink (or a file) there is its own size, never what it
    // leads to, which removing it would not free.
    match path.symlink_metadata() {
        Ok(meta) if !meta.is_dir() => return (if counts(&meta) { meta.blocks() * 512 } else { 0 }, true),
        Err(_) => return (0, true),
        Ok(_) => {}
    }
    let end = std::time::Instant::now() + within;
    let mut total = 0;
    let mut todo = vec![path.to_path_buf()];
    while let Some(dir) = todo.pop() {
        if std::time::Instant::now() >= end {
            return (total, false);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let at = entry.path();
            let Ok(meta) = at.symlink_metadata() else { continue };
            if meta.is_dir() {
                if apart == Some(at.as_path()) {
                    continue;
                }
                total += meta.blocks() * 512;
                todo.push(at);
            } else if counts(&meta) {
                total += meta.blocks() * 512;
            }
        }
    }
    (total, true)
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

    /// All a tree holds that goes with it: what gc counts for a place, nothing else removed before it.
    fn dir_size(path: &Path, within: Duration) -> (u64, bool) {
        let (bytes, whole, _) = Freed::default().by_removing(path, within);
        (bytes, whole)
    }

    /// A running job's clone, as its disk use, with the copy of the linked caches it was given in its `.git`.
    fn job_work_size(clone: &Path, within: Duration) -> (u64, bool) {
        let copy = clone.join(".git").join(crate::sandbox::farm::IN_CLONE);
        (dirs_size(&[clone.to_path_buf()], Some(&copy), within), true)
    }

    /// The clone's own copy of the linked caches is the project's cache, not the job's work, so it is left
    /// out of the clone's size; a directory of that name anywhere else in the clone is the job's, and counts.
    #[test]
    fn a_clones_size_leaves_out_its_own_copy_of_the_caches_only() {
        let d = Dir::new();
        let clone = d.path().join("clone");
        std::fs::create_dir_all(clone.join(".git/relay-caches/.cache")).unwrap();
        std::fs::write(clone.join(".git/relay-caches/.cache/pkg"), vec![1u8; 1 << 20]).unwrap();
        let without = job_work_size(&clone, Duration::from_secs(10)).0;
        assert!(without < 1 << 20, "the copy was counted: {without}");
        std::fs::create_dir_all(clone.join("sub/.git/relay-caches")).unwrap();
        std::fs::write(clone.join("sub/.git/relay-caches/work"), vec![1u8; 1 << 20]).unwrap();
        assert!(job_work_size(&clone, Duration::from_secs(10)).0 >= without + (1 << 20), "the job's own was not");
    }

    /// Copilot at a6b6099: gc counted all a copy of the linked caches holds as freed, but its packages are
    /// hard-linked into the trees installed from it, and those blocks stay while such a tree does. A file
    /// with several links is freed with the last of them: with the place that held it, once every other
    /// is among what was removed, and never while one is outside.
    #[test]
    fn a_file_is_freed_with_its_last_link_only() {
        let d = Dir::new();
        let (copy, tree, kept) = (d.path().join("copy"), d.path().join("tree"), d.path().join("kept"));
        for dir in [&copy, &tree, &kept] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(copy.join("both"), vec![1u8; 1 << 20]).unwrap();
        std::fs::hard_link(copy.join("both"), tree.join("both")).unwrap();
        std::fs::write(copy.join("outside"), vec![1u8; 1 << 20]).unwrap();
        std::fs::hard_link(copy.join("outside"), kept.join("outside")).unwrap();
        std::fs::write(copy.join("own"), vec![1u8; 1 << 18]).unwrap();
        let within = Duration::from_secs(10);
        let mut freed = Freed::default();
        let (bytes, whole, links) = freed.by_removing(&copy, within);
        assert!(whole && (1 << 18..1 << 20).contains(&bytes), "only what nothing else links: {bytes}");
        // Kept (its removal failed): its links are still there, and the tree frees nothing of the shared file.
        assert!(freed.by_removing(&tree, within).0 < 1 << 20);
        // Gone: the tree holds the last link of the file they shared, and never the one a kept place links.
        freed.removed(links);
        let (bytes, _, links) = freed.by_removing(&tree, within);
        assert!((1 << 20..2 << 20).contains(&bytes), "the last link frees the file, once: {bytes}");
        freed.removed(links);
        assert!(freed.by_removing(&kept, within).0 >= 1 << 20, "and so does the last of the other");
    }

    /// Copilot at a6b6099: every directory a running job is measured by was sized as a clone, so a
    /// `.git/relay-caches` in its `/tmp` was left out too: work written there never counted against
    /// `limits.job_disk_gb`. Only the clone has a copy of the caches; its `/tmp` is counted whole.
    #[test]
    fn a_jobs_tmp_is_counted_whole_whatever_it_names_its_directories() {
        let d = Dir::new();
        let (clone, tmp) = (d.path().join("clone"), d.path().join("tmp"));
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        std::fs::create_dir_all(tmp.join(".git/relay-caches")).unwrap();
        std::fs::write(tmp.join(".git/relay-caches/work"), vec![1u8; 1 << 20]).unwrap();
        let copy = clone.join(".git/relay-caches");
        let bytes = dirs_size(&[clone.clone(), tmp.clone()], Some(&copy), Duration::from_secs(10));
        assert!(bytes >= 1 << 20, "work in /tmp/.git/relay-caches was left out of the job's disk use: {bytes}");
        // Nor is a directory of that name in the clone left out when this run was given no copy there (no
        // cache is linked): the job made it, and what it writes there is its work.
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(copy.join("work"), vec![1u8; 1 << 20]).unwrap();
        let paths = [clone, tmp];
        let given = dirs_size(&paths, Some(&copy), Duration::from_secs(10));
        let not_given = dirs_size(&paths, None, Duration::from_secs(10));
        assert!(not_given >= given + (1 << 20), "a job with no copy hid its work under the copy's name: {not_given}");
    }

    /// Copilot on (e): the skip of a clone's copy of the caches was in `dir_size` itself, so gc's sizes left
    /// it out too, and a dead probe's warmed copy freed "nothing". A whole tree is counted whole; only a
    /// live job's disk accounting leaves its copy out, by asking for that.
    /// Copilot at fdbf183: a retired entry can be a symlink (one left where a copy was), and the size walk
    /// followed its root, so gc reported its target's bytes freed though removal unlinks only the link.
    /// No link is followed at any depth, the root included.
    #[test]
    fn a_symlink_is_sized_as_itself_never_as_what_it_points_at() {
        let d = Dir::new();
        let target = d.path().join("big");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("data"), vec![1u8; 1 << 20]).unwrap();
        let link = d.path().join("gone-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (bytes, whole) = dir_size(&link, Duration::from_secs(10));
        assert!(whole && bytes < 1 << 20, "the link's target was counted: {bytes}");
        let file = d.path().join("gone-file");
        std::fs::write(&file, vec![1u8; 1 << 16]).unwrap();
        assert!(dir_size(&file, Duration::from_secs(10)).0 >= 1 << 16, "a file is counted as itself");
    }

    #[test]
    fn a_whole_tree_counts_a_copy_of_the_caches_in_it() {
        let d = Dir::new();
        let clone = d.path().join("clone");
        std::fs::create_dir_all(clone.join(".git/relay-caches/.cache")).unwrap();
        std::fs::write(clone.join(".git/relay-caches/.cache/pkg"), vec![1u8; 1 << 20]).unwrap();
        assert!(dir_size(&clone, Duration::from_secs(10)).0 >= 1 << 20, "gc's size left the copy out");
    }

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
