//! A job's own copy of the project's linked caches, made of hard links, inside the mount that holds its tree.
//!
//! `link()` works within one mount only: across two it fails with EXDEV, even on one filesystem. A
//! package manager links from its store into the tree it installs (bun, pnpm, uv), so a cache bound
//! apart from the tree made it copy every package. For the caches a project declares such a tool links
//! from (`sandbox.linked_caches`), a job with a tree therefore gets this copy, in the tree's own mount
//! (a directory in its clone's `.git`, or in its check's directory), and each such cache path is a
//! symlink into it ([`super::Mount::Symlink`]). Every other cache stays a bind: the copy costs every run
//! a pass over its directories. gc removes idle copies (`gc.cache_copies_idle_days`).
//!
//! - **Cost.** A copy is a directory for each directory of the caches and a hard link for each file:
//!   no file's data, and no inode for a link. Copies are kept in a pool beside the place they are used
//!   (on the same filesystem), and a run takes an idle one by renaming it into place and gives it back
//!   the same way, so the link for each file is made once per copy, not once per run. A run's own cost
//!   is bringing its copy up to date when it starts and linking back what it added when it ends: each
//!   compares the two sides' directory listings, so it reads every directory once and touches no file
//!   that both sides have.
//! - **Shared and persistent.** Every name a job added is linked back into the project's caches when
//!   it ends, so the next job has it. Two jobs that add one entry at once: each writes its own copy,
//!   and the first linked back is the one kept. An entry either side already has stays as it is there,
//!   so a file a job replaced (renamed over) stays in its copy only; one changed in place is the shared
//!   file itself (one inode), as it was when the caches were one shared bind.
//! - **Only what the plan links.** A copy is taken for the caches [`super::linkable`] names, the same
//!   judge the plan asks, and anything else in it (a pooled copy's cache that a hide rule now covers) is
//!   removed when it is taken: a cache left a bind is never in a job's tree.
//! - **Nothing followed.** A copy is written by the jobs that had it, and the shared caches by jobs with
//!   no tree (a review binds them as before), so both are walked through directory handles opened
//!   without following a symlink, from relay's caches' root down, one component at a time; a symlink
//!   is linked as a symlink, and anything but a directory on the way to a cache is refused, never
//!   replaced. A copy is walked only after its job's sandbox has ended (its PID namespace with it),
//!   through the handles relay opened when it put it in place, so a job that moves or replaces the
//!   directories around it changes nothing relay reads.
//! - **Whole or not at all.** A directory a job added appears in the shared cache whole: it is built in
//!   `.relay-staging` beside the caches and renamed into place, never over anything there.
//! - **A real link first.** Before a copy is used, a file of the shared cache is linked into it (an
//!   unnamed `O_TMPFILE`, so no name shows): two mounts of one filesystem share a device number, but
//!   `link()` between them fails, and an empty cache would make no link to find out. On failure the
//!   cache is a bind.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

/// What bringing a copy up to date, or linking back from it, did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Directories read.
    pub read: u64,
    /// Directories made.
    pub dirs: u64,
    /// Hard links made.
    pub links: u64,
    /// Entries that could not be read or linked, and were left out.
    pub failed: u64,
}

/// A job's copy of the caches, in place; linked back and returned to its pool when dropped.
#[derive(Debug)]
pub struct Farm {
    path: PathBuf,
    name: CString,
    /// The directory holding the copy, and the copy itself, as opened when it was put in place.
    parent: OwnedFd,
    root: OwnedFd,
    pool: PathBuf,
    shared: PathBuf,
    rels: Vec<PathBuf>,
    released: bool,
}

impl Farm {
    /// Puts a copy of the shared caches `shared/<rel>`, for each of `rels`, at `name` in `parent` (a
    /// directory only relay writes, or a `.git` reached from one without following a link): the copy
    /// a run that never ended left there, else an idle one from `pool`, else a new one; then links into
    /// it what the shared caches have that it lacks. Fails, leaving none, when no link can be made at all,
    /// as when `parent` is on another filesystem than `shared`.
    pub fn take(parent: &Path, name: &str, pool: &Path, shared: &Path, rels: &[PathBuf]) -> io::Result<(Self, Tally)> {
        let parent_fd = open_path(parent)?;
        let cname = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let root = if let Ok(root) = open_dir(&parent_fd, &cname) {
            root
        } else {
            // Not a directory (a link left in its place): it goes.
            let _ = rustix::fs::unlinkat(&parent_fd, &cname, AtFlags::empty());
            if !from_pool(pool, &parent_fd, &cname) {
                rustix::fs::mkdirat(&parent_fd, &cname, Mode::from_raw_mode(0o700))?;
            }
            open_dir(&parent_fd, &cname)?
        };
        let mut farm = Self {
            path: parent.join(name),
            name: cname,
            parent: parent_fd,
            root,
            pool: pool.to_owned(),
            shared: shared.to_owned(),
            rels: rels.to_vec(),
            released: false,
        };
        let mut tally = Tally::default();
        let brought = (|| -> io::Result<()> {
            // A copy from the pool may hold caches this run does not link (one a hide rule now covers):
            // it holds these and nothing else.
            purge(&farm.root, &rels.iter().map(PathBuf::as_path).collect::<Vec<_>>())?;
            let root = shared_root(shared)?;
            for rel in rels {
                let from = make_shared(&root, rel)?;
                let to = make_beneath(&farm.root, rel)?;
                // A real link, made before anything is walked: two mounts of one filesystem share a device
                // number but link() between them fails, and an empty cache would make no link to find out.
                probe_link(&from, &to)?;
                link_tree(&from, &to, Dest::Copy, &mut tally)?;
            }
            Ok(())
        })();
        if let Err(e) = brought {
            // Nothing the job did is in it: removed, not linked back.
            farm.released = true;
            let _ = crate::hostdir::remove_beneath(farm.parent.as_fd(), &farm.name);
            return Err(e);
        }
        Ok((farm, tally))
    }

    /// Where the copy is: each cache `rel` is `path/rel`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Links back into the shared caches every name the job added, then gives the copy back to the pool
    /// (or, when it cannot, removes it). Once only.
    pub fn release(&mut self) -> Tally {
        let mut tally = Tally::default();
        if std::mem::replace(&mut self.released, true) {
            return tally;
        }
        let root = shared_root(&self.shared);
        let staging =
            root.as_ref().map_err(|e| io::Error::from(e.kind())).and_then(|r| make_shared(r, Path::new(STAGING)));
        // What a release that never finished left being built: no run is building it any more.
        if let Ok(staging) = &staging {
            sweep(staging, NEW);
        }
        for rel in &self.rels {
            let Ok(from) = open_beneath(&self.root, rel) else { continue };
            let to = root.as_ref().map_err(|e| io::Error::from(e.kind())).and_then(|r| make_shared(r, rel));
            match (to, &staging) {
                (Ok(to), Ok(staging)) => {
                    let _ = link_tree(&from, &to, Dest::Shared { staging }, &mut tally);
                }
                _ => tally.failed += 1,
            }
        }
        // Only the directory relay put there goes back: a job may have put another in its place.
        let same = |a: &rustix::fs::Stat, b: &rustix::fs::Stat| (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino);
        let ours = match (
            rustix::fs::fstat(&self.root),
            rustix::fs::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW),
        ) {
            (Ok(a), Ok(b)) => same(&a, &b),
            _ => false,
        };
        if ours && !to_pool(&self.pool, &self.parent, &self.name) {
            let _ = crate::hostdir::remove_beneath(self.parent.as_fd(), &self.name);
        }
        tally
    }
}

impl Drop for Farm {
    fn drop(&mut self) {
        self.release();
    }
}

/// Moves an idle copy from `pool` to `name` in `parent`; false when there is none (or the pool is on
/// another filesystem). Taking one is a rename, so two runs never take the same.
fn from_pool(pool: &Path, parent: &OwnedFd, name: &CStr) -> bool {
    let Ok(pool_fd) = open_path(pool) else { return false };
    // The one used last first: it is the most up to date, and the others age out (`idle`).
    for (entry, _) in idle(pool).into_iter().rev() {
        let Ok(entry) = CString::new(entry.into_os_string().into_encoded_bytes()) else { continue };
        match rustix::fs::renameat(&pool_fd, &entry, parent, name) {
            Ok(()) => return true,
            Err(Errno::XDEV) => return false,
            Err(_) => {}
        }
    }
    false
}

/// Moves the copy `name` in `parent` into `pool`, idle; false when it cannot.
fn to_pool(pool: &Path, parent: &OwnedFd, name: &CStr) -> bool {
    let made = std::fs::DirBuilder::new().recursive(true).create(pool);
    let Ok(pool_fd) = made.and_then(|()| open_path(pool)) else { return false };
    let idle = format!("{IDLE}{}", uuid::Uuid::now_v7());
    rustix::fs::renameat(parent, name, &pool_fd, idle.as_str()).is_ok()
}

/// An idle copy's name in a pool: this, then a v7 UUID made when it was given back, so it says when.
const IDLE: &str = "idle-";

/// The idle copies in `pool`, each with when it was last given back (Unix microseconds), oldest first.
pub fn idle(pool: &Path) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = std::fs::read_dir(pool)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let at = made_at(&name, IDLE)?;
            Some((PathBuf::from(name), at))
        })
        .collect();
    out.sort_by_key(|(_, at)| *at);
    out
}

/// Copies in `pool` whose removal began and did not finish (`gone-*`): nothing takes them, and gc
/// removes them again.
pub fn abandoned(pool: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(pool)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| PathBuf::from(e.file_name()))
        .filter(|n| n.to_str().is_some_and(|n| n.starts_with(GONE)))
        .collect()
}

/// The name a copy is renamed to in its pool while it is removed, then a v7 UUID.
const GONE: &str = "gone-";

/// Removes the idle copy `name` from `pool`: first renamed out of the pool, so a run taking it at the
/// same moment has either it whole or none; then removed whatever modes a job left in it (a directory
/// it made mode 000), never following a symlink ([`crate::hostdir::remove_tree`]).
/// Its files are the caches' own (hard links), so this frees directories, not data.
pub fn remove_idle(pool: &Path, name: &Path) -> io::Result<()> {
    let pool_fd = open_path(pool)?;
    let gone = format!("{GONE}{}", uuid::Uuid::now_v7());
    rustix::fs::renameat(&pool_fd, name, &pool_fd, gone.as_str())?;
    crate::hostdir::remove_tree(&pool.join(gone))
}

/// A directory of relay's own, by path: only its last component is not followed.
fn open_path(p: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::open(p, flags, Mode::empty())?)
}

/// The directory `name` in `at`, unless it is anything else (a symlink included).
fn open_dir(at: &OwnedFd, name: impl rustix::path::Arg) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(at, name, flags, Mode::empty())?)
}

fn names(rel: &Path) -> impl Iterator<Item = &OsStr> {
    rel.components().filter_map(|c| match c {
        Component::Normal(n) => Some(n),
        _ => None,
    })
}

/// `rel` under `at`, one directory at a time, following no symlink.
fn open_beneath(at: &OwnedFd, rel: &Path) -> io::Result<OwnedFd> {
    let mut fd = at.try_clone()?;
    for n in names(rel) {
        fd = open_dir(&fd, n)?;
    }
    Ok(fd)
}

/// The same in a copy, making each directory that is not there, in place of anything else a job left
/// under its name (a symlink to elsewhere).
fn make_beneath(at: &OwnedFd, rel: &Path) -> io::Result<OwnedFd> {
    let mut fd = at.try_clone()?;
    for n in names(rel) {
        match rustix::fs::mkdirat(&fd, n, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        fd = if let Ok(sub) = open_dir(&fd, n) {
            sub
        } else {
            rustix::fs::unlinkat(&fd, n, AtFlags::empty())?;
            rustix::fs::mkdirat(&fd, n, Mode::from_raw_mode(0o700))?;
            open_dir(&fd, n)?
        };
    }
    Ok(fd)
}

/// Removes the copy `name` in `parent` (a directory only relay writes, or a `.git` reached from one
/// without following a link), if one is there: one a run left that never ended, which nothing will take
/// (this run links nothing). Through `parent`'s handle, never following a symlink, whatever modes a job
/// left in it. An error when something is still there after.
pub fn clear(parent: &Path, name: &str) -> io::Result<()> {
    let parent_fd = match open_path(parent) {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let cname = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    match rustix::fs::statat(&parent_fd, &cname, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    crate::hostdir::remove_beneath(parent_fd.as_fd(), &cname)?;
    match rustix::fs::statat(&parent_fd, &cname, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        _ => Err(io::Error::other(format!("{} could not be removed", parent.join(name).display()))),
    }
}

/// Makes the cache `rel` in relay's caches `shared`, from their root one directory at a time, following
/// no symlink: what a cache bound into a sandbox is made with.
pub fn make_cache_dir(shared: &Path, rel: &Path) -> io::Result<()> {
    make_shared(&shared_root(shared)?, rel).map(drop)
}

/// Puts the empty file `name` in the cache `rel` of relay's caches `shared`, where nothing of that name is,
/// following no symlink on the way or at it (the probe's marker).
pub fn put_in_cache(shared: &Path, rel: &Path, name: &str) -> io::Result<()> {
    let dir = make_shared(&shared_root(shared)?, rel)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(&dir, name, flags, Mode::from_raw_mode(0o644)).map(drop).map_err(Into::into)
}

/// Removes the file `name` from the cache `rel` of relay's caches `shared`, following no symlink.
pub fn remove_in_cache(shared: &Path, rel: &Path, name: &str) -> io::Result<()> {
    let dir = open_beneath(&open_path(shared)?, rel)?;
    rustix::fs::unlinkat(&dir, name, AtFlags::empty()).map_err(Into::into)
}

/// The name a link probe's file has while it is in a cache, then a v7 UUID; never linked in either direction.
const PROBE: &str = ".relay-link-probe-";

/// The name of a directory being built in [`STAGING`], then a v7 UUID.
const NEW: &str = "new-";

/// How old something a run makes for a moment (a probe, a directory being built) must be before another
/// run takes it for one a run that never finished left: no run takes this long over it.
const STALE_US: u64 = 3_600_000_000;

/// When the entry `name` (`prefix`, then a v7 UUID) was made, in Unix microseconds.
fn made_at(name: &str, prefix: &str) -> Option<u64> {
    let id = uuid::Uuid::parse_str(name.strip_prefix(prefix)?).ok()?;
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    Some(secs * 1_000_000 + u64::from(nanos) / 1000)
}

/// Removes from `dir` each entry named `prefix` and a v7 UUID older than [`STALE_US`], never following a
/// symlink: what an interrupted run left, which would otherwise stay for ever.
fn sweep(dir: &OwnedFd, prefix: &str) {
    let now = crate::bus::now_micros();
    for (name, _) in entries(dir).unwrap_or_default() {
        let old =
            name.to_str().ok().and_then(|n| made_at(n, prefix)).is_some_and(|at| now.saturating_sub(at) > STALE_US);
        if old {
            let _ = crate::hostdir::remove_beneath(dir.as_fd(), &name);
        }
    }
}

/// Tests only: the link probe takes its fallback, as on a filesystem with no `O_TMPFILE`.
#[cfg(test)]
static NO_TMPFILE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The directory in relay's caches where a new directory is built before it is renamed into a cache whole;
/// beside the caches, in no cache's mount.
const STAGING: &str = ".relay-staging";

/// Relay's caches (`<data dir>/<project>/caches`), opened: a directory only relay writes, reached by a
/// path only relay writes. Every walk of a shared cache starts from this handle.
fn shared_root(shared: &Path) -> io::Result<OwnedFd> {
    std::fs::DirBuilder::new().recursive(true).create(shared)?;
    open_path(shared)
}

/// `rel` in relay's caches, from their opened root one directory at a time, following no symlink and
/// making each directory that is not there. Anything else in the way is refused, never replaced: a
/// shared cache is another job's too, and a symlink there (a job that binds a cache around this one
/// could make it) would take the walk out of relay's caches.
fn make_shared(root: &OwnedFd, rel: &Path) -> io::Result<OwnedFd> {
    let mut fd = root.try_clone()?;
    for n in names(rel) {
        match rustix::fs::mkdirat(&fd, n, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        fd = open_dir(&fd, n)?;
    }
    Ok(fd)
}

/// Whether a file in the shared cache `shared` can be hard-linked into the copy `copy`: an unnamed file
/// made there (`O_TMPFILE`, so no name ever shows in the shared cache) is linked into the copy and the link
/// removed. Where the filesystem has no unnamed files, a file made in the copy is linked into the shared
/// cache itself and removed at once (a run taking its copy meanwhile skips the name: [`PROBE`]). EXDEV
/// when not.
fn probe_link(shared: &OwnedFd, copy: &OwnedFd) -> io::Result<()> {
    let name = format!("{PROBE}{}", uuid::Uuid::now_v7());
    let flags = OFlags::WRONLY | OFlags::TMPFILE | OFlags::CLOEXEC;
    #[cfg(test)]
    let forced = NO_TMPFILE.load(std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let forced = false;
    if !forced && let Ok(tmp) = rustix::fs::openat(shared, ".", flags, Mode::from_raw_mode(0o600)) {
        let magic = format!("/proc/self/fd/{}", tmp.as_raw_fd());
        rustix::fs::linkat(CWD, magic.as_str(), copy, name.as_str(), AtFlags::SYMLINK_FOLLOW)?;
        let _ = rustix::fs::unlinkat(copy, name.as_str(), AtFlags::empty());
        return Ok(());
    }
    // A probe a run that never finished left here (it is linked nowhere: every walk skips the name).
    sweep(shared, PROBE);
    let made = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    drop(rustix::fs::openat(copy, name.as_str(), made, Mode::from_raw_mode(0o600))?);
    let linked = rustix::fs::linkat(copy, name.as_str(), shared, name.as_str(), AtFlags::empty());
    if linked.is_ok() {
        let _ = rustix::fs::unlinkat(shared, name.as_str(), AtFlags::empty());
    }
    let _ = rustix::fs::unlinkat(copy, name.as_str(), AtFlags::empty());
    Ok(linked?)
}

/// Removes from a copy everything but the caches `rels` (whole) and the directories on the way to them.
fn purge(at: &OwnedFd, rels: &[&Path]) -> io::Result<()> {
    for (name, is_dir) in entries(at)? {
        let here = Path::new(OsStr::from_bytes(name.to_bytes()));
        let below: Vec<&Path> = rels.iter().filter_map(|r| r.strip_prefix(here).ok()).collect();
        if below.iter().any(|b| b.as_os_str().is_empty()) {
            continue;
        }
        match (is_dir && !below.is_empty()).then(|| open_dir(at, name.as_c_str())) {
            Some(Ok(sub)) => purge(&sub, &below)?,
            _ => crate::hostdir::remove_beneath(at.as_fd(), &name)?,
        }
    }
    Ok(())
}

/// Where [`link_tree`] links to.
#[derive(Clone, Copy)]
enum Dest<'a> {
    /// A job's copy, only relay's while it walks: a directory is made there and filled.
    Copy,
    /// A shared cache, read by runs taking their copies meanwhile: a new directory is built in `staging`
    /// and renamed into place whole, never over anything there, so no run sees one half-filled.
    Shared { staging: &'a OwnedFd },
}

/// The entries of `fd` but `.` and `..`, each with whether it is a directory (not a symlink to one).
fn entries(fd: &OwnedFd) -> io::Result<Vec<(CString, bool)>> {
    let mut out = Vec::new();
    let mut dir = Dir::read_from(fd)?;
    while let Some(e) = dir.read() {
        let e = e?;
        let name = e.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let is_dir = match e.file_type() {
            FileType::Directory => true,
            FileType::Unknown => rustix::fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|s| FileType::from_raw_mode(s.st_mode) == FileType::Directory),
            _ => false,
        };
        out.push((name.to_owned(), is_dir));
    }
    Ok(out)
}

/// Links into `to` every entry of `from` that `to` has no name for: a directory is made there, anything
/// else hard-linked (a symlink as itself). A directory both have is walked; any other name both have is
/// left as it is on each side, untouched. Directories made keep their mode, plus the owner's `rwx`, so
/// that names can be added to them later and a copy can be removed (Go makes its module cache read-only);
/// a copy linked back from (into a shared cache) gets the same. A new directory in a shared cache appears
/// whole ([`Dest::Shared`]). EXDEV stops it (no link can be made from there to here); any other failure
/// leaves that entry out.
fn link_tree(from: &OwnedFd, to: &OwnedFd, dest: Dest<'_>, tally: &mut Tally) -> io::Result<()> {
    let own = matches!(dest, Dest::Shared { .. });
    tally.read += 1;
    let have: HashMap<CString, bool> = entries(to)?.into_iter().collect();
    for (name, is_dir) in entries(from)? {
        let name: &CStr = &name;
        if name.to_bytes().starts_with(PROBE.as_bytes()) {
            continue;
        }
        let there = have.get(name).copied();
        if !is_dir {
            if there.is_some() {
                continue;
            }
            match rustix::fs::linkat(from, name, to, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::EXIST) => tally.links += 1,
                Err(Errno::XDEV) => return Err(Errno::XDEV.into()),
                Err(_) => tally.failed += 1,
            }
            continue;
        }
        // Something else under that name there (a file, or a symlink): it stays, and nothing below is linked.
        if there == Some(false) {
            continue;
        }
        let Ok(sub_from) = open_dir(from, name) else {
            tally.failed += 1;
            continue;
        };
        let mode = rustix::fs::fstat(&sub_from).map_or(0o755, |s| s.st_mode & 0o7777);
        if own && mode & 0o700 != 0o700 {
            let _ = rustix::fs::fchmod(&sub_from, Mode::from_raw_mode(mode | 0o700));
        }
        match (there, dest) {
            (None, Dest::Shared { staging }) => publish(&sub_from, to, name, mode, staging, tally)?,
            (None, Dest::Copy) => {
                match rustix::fs::mkdirat(to, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(Errno::EXIST) => tally.dirs += 1,
                    Err(_) => {
                        tally.failed += 1;
                        continue;
                    }
                }
                let Ok(sub_to) = open_dir(to, name) else {
                    tally.failed += 1;
                    continue;
                };
                link_tree(&sub_from, &sub_to, dest, tally)?;
                let _ = rustix::fs::fchmod(&sub_to, Mode::from_raw_mode(mode | 0o700));
            }
            (Some(_), _) => {
                let Ok(sub_to) = open_dir(to, name) else {
                    tally.failed += 1;
                    continue;
                };
                link_tree(&sub_from, &sub_to, dest, tally)?;
            }
        }
    }
    Ok(())
}

/// Puts the copy's new directory `from` at `name` in the shared directory `to`, whole: built in `staging`
/// (in relay's caches, beside them, which no run reads), then renamed into place unless something took
/// the name meanwhile (another job's release), in which case what it lacks is linked into that one.
fn publish(
    from: &OwnedFd,
    to: &OwnedFd,
    name: &CStr,
    mode: u32,
    staging: &OwnedFd,
    tally: &mut Tally,
) -> io::Result<()> {
    let stage = format!("{NEW}{}", uuid::Uuid::now_v7());
    if rustix::fs::mkdirat(staging, stage.as_str(), Mode::from_raw_mode(0o700)).is_err() {
        tally.failed += 1;
        return Ok(());
    }
    tally.dirs += 1;
    let failed_before = tally.failed;
    let built = open_dir(staging, stage.as_str()).and_then(|staged| {
        link_tree(from, &staged, Dest::Copy, tally)?;
        rustix::fs::fchmod(&staged, Mode::from_raw_mode(mode | 0o700))?;
        // Anything left out (an entry the job made unreadable) and it is not the job's directory whole.
        if tally.failed != failed_before {
            return Err(io::Error::other("an entry could not be staged"));
        }
        Ok(())
    });
    let placed = built.and_then(|()| {
        Ok(rustix::fs::renameat_with(staging, stage.as_str(), to, name, rustix::fs::RenameFlags::NOREPLACE)?)
    });
    match placed {
        Ok(()) => return Ok(()),
        Err(e) if e.raw_os_error() == Some(Errno::XDEV.raw_os_error()) => {
            let _ = crate::hostdir::remove_beneath(staging.as_fd(), &CString::new(stage).unwrap_or_default());
            return Err(e);
        }
        Err(e) if e.raw_os_error() == Some(Errno::EXIST.raw_os_error()) => {
            if let Ok(there) = open_dir(to, name) {
                link_tree(from, &there, Dest::Shared { staging }, tally)?;
            }
        }
        Err(_) => tally.failed += 1,
    }
    let _ = crate::hostdir::remove_beneath(staging.as_fd(), &CString::new(stage).unwrap_or_default());
    Ok(())
}

/// The name of the copy in a session's clone: in `.git`, where git and the session's work ignore it.
pub const IN_CLONE: &str = "relay-caches";

/// A session clone's `.git`, when it is a directory (not a link the session left in its place).
pub fn clone_git(clone: &Path) -> Option<PathBuf> {
    let git = clone.join(".git");
    git.symlink_metadata().is_ok_and(|m| m.is_dir()).then_some(git)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir as TempDir;
    use std::os::unix::fs::MetadataExt;

    fn ino(p: &Path) -> u64 {
        p.symlink_metadata().unwrap().ino()
    }

    fn rels() -> Vec<PathBuf> {
        vec![PathBuf::from(".cache"), PathBuf::from(".cargo/registry")]
    }

    struct Place {
        _d: TempDir,
        shared: PathBuf,
        job: PathBuf,
        pool: PathBuf,
    }

    fn place() -> Place {
        let d = TempDir::new();
        let (shared, job, pool) = (d.path().join("caches"), d.path().join("job"), d.path().join("farms"));
        std::fs::create_dir_all(&job).unwrap();
        Place { _d: d, shared, job, pool }
    }

    fn take(p: &Place) -> (Farm, Tally) {
        Farm::take(&p.job, "f", &p.pool, &p.shared, &rels()).unwrap()
    }

    fn pooled(p: &Place) -> usize {
        std::fs::read_dir(&p.pool).map_or(0, Iterator::count)
    }

    #[test]
    fn a_copy_links_every_file_and_a_release_adds_only_what_is_new() {
        let p = place();
        let shared = &p.shared;
        std::fs::create_dir_all(shared.join(".cache/bun/pkg")).unwrap();
        std::fs::write(shared.join(".cache/bun/pkg/index.js"), "old").unwrap();
        std::os::unix::fs::symlink("index.js", shared.join(".cache/bun/pkg/main.js")).unwrap();
        let (mut farm, made) = take(&p);
        assert_eq!((made.links, made.dirs, made.failed), (2, 2, 0), "{made:?}");
        let f = farm.path().to_owned();
        assert_eq!(ino(&f.join(".cache/bun/pkg/index.js")), ino(&shared.join(".cache/bun/pkg/index.js")));
        assert!(f.join(".cache/bun/pkg/main.js").symlink_metadata().unwrap().is_symlink(), "a link as a link");
        assert!(f.join(".cargo/registry").is_dir() && shared.join(".cargo/registry").is_dir());

        // The job adds a package and replaces a file by renaming over it; another job meanwhile added `other`.
        std::fs::create_dir_all(f.join(".cache/bun/new")).unwrap();
        std::fs::write(f.join(".cache/bun/new/a.js"), "new").unwrap();
        std::fs::write(f.join(".cache/bun/pkg/tmp"), "replaced").unwrap();
        std::fs::rename(f.join(".cache/bun/pkg/tmp"), f.join(".cache/bun/pkg/index.js")).unwrap();
        std::fs::write(f.join(".cache/other"), "mine").unwrap();
        std::fs::write(shared.join(".cache/other"), "theirs").unwrap();
        let back = farm.release();
        assert_eq!((back.links, back.dirs), (1, 1), "{back:?}");
        assert_eq!(std::fs::read_to_string(shared.join(".cache/bun/new/a.js")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(shared.join(".cache/bun/pkg/index.js")).unwrap(), "old", "kept");
        assert_eq!(std::fs::read_to_string(shared.join(".cache/other")).unwrap(), "theirs", "the first one kept");
        assert!(!f.exists() && pooled(&p) == 1, "the copy went back to the pool");
        assert_eq!(farm.release(), Tally::default(), "once only");
    }

    /// The next run takes the idle copy back and links in only what it lacks: what the run before saw
    /// is not linked again, file by file.
    #[test]
    fn a_pooled_copy_is_taken_again_and_brought_up_to_date() {
        let p = place();
        std::fs::create_dir_all(p.shared.join(".cache/a")).unwrap();
        for i in 0..50 {
            std::fs::write(p.shared.join(format!(".cache/a/{i}")), "x").unwrap();
        }
        let (farm, first) = take(&p);
        assert_eq!(first.links, 50);
        drop(farm);
        std::fs::write(p.shared.join(".cache/a/later"), "y").unwrap();
        let (farm, again) = take(&p);
        assert_eq!((again.links, again.dirs, pooled(&p)), (1, 0, 0), "{again:?}");
        assert_eq!(ino(&farm.path().join(".cache/a/later")), ino(&p.shared.join(".cache/a/later")));
        // Two at once: the second has none idle, and makes its own; both go back.
        let (other, _) = Farm::take(&p.job, "g", &p.pool, &p.shared, &rels()).unwrap();
        drop((farm, other));
        assert_eq!(pooled(&p), 2);
    }

    /// A copy is the job's to write: a symlink it leaves, or a directory it swaps for one, is linked as
    /// a link (never walked); a read-only directory (Go's module cache) still takes new names; and a
    /// directory put where the copy was is not taken into the pool.
    #[test]
    fn a_release_follows_nothing_the_job_left() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        let outside = p.job.with_file_name("outside");
        std::fs::create_dir_all(outside.join("secret")).unwrap();
        std::fs::write(outside.join("secret/key"), "k").unwrap();
        let (mut farm, _) = take(&p);
        let f = farm.path().to_owned();
        std::os::unix::fs::symlink(&outside, f.join(".cache/planted")).unwrap();
        // The job replaced a whole cache with a link elsewhere: nothing is read through it.
        std::fs::remove_dir_all(f.join(".cargo/registry")).unwrap();
        std::os::unix::fs::symlink(&outside, f.join(".cargo/registry")).unwrap();
        std::fs::create_dir_all(f.join(".cache/go/mod")).unwrap();
        std::fs::write(f.join(".cache/go/mod/x.go"), "x").unwrap();
        std::fs::set_permissions(f.join(".cache/go/mod"), std::fs::Permissions::from_mode(0o555)).unwrap();
        // And moved the directory that held its copy, leaving a link to elsewhere in its place.
        let moved = p.job.with_file_name("moved");
        std::fs::rename(&p.job, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &p.job).unwrap();
        farm.release();
        assert!(p.shared.join(".cache/planted").symlink_metadata().unwrap().is_symlink(), "linked as a link");
        assert!(!p.shared.join(".cargo/registry/secret").exists(), "a cache swapped for a link is not walked");
        assert_eq!(std::fs::read_to_string(p.shared.join(".cache/go/mod/x.go")).unwrap(), "x");
        assert!(!moved.join("f").exists() && pooled(&p) == 1, "returned through its handle");
        assert_eq!(std::fs::read_to_string(outside.join("secret/key")).unwrap(), "k", "nothing outside touched");

        // A directory the job put in place of its copy stays where it is, out of the pool.
        std::fs::remove_file(&p.job).unwrap();
        std::fs::rename(&moved, &p.job).unwrap();
        let (mut farm, _) = take(&p);
        assert!(farm.path().join(".cargo/registry").symlink_metadata().unwrap().is_dir(), "the link it left is gone");
        std::fs::rename(farm.path(), p.job.join("away")).unwrap();
        std::fs::create_dir_all(p.job.join("f/planted")).unwrap();
        farm.release();
        assert!(p.job.join("f/planted").exists() && pooled(&p) == 0);
    }

    /// What a copy costs on a real cache: `RELAY_FARM_MEASURE=<dir holding the caches>` (each of its
    /// subdirectories is one cache), on the filesystem of the system temporary directory. Prints the time
    /// and the counts of the first run (no copy idle: one is made), a release, and a second run (the copy
    /// taken back from the pool).
    #[test]
    #[ignore = "measures a real cache; set RELAY_FARM_MEASURE"]
    #[allow(clippy::disallowed_methods, reason = "the caller names the cache to measure")]
    fn measure() {
        let Some(shared) = std::env::var_os("RELAY_FARM_MEASURE").map(PathBuf::from) else { return };
        let rels: Vec<PathBuf> =
            std::fs::read_dir(&shared).unwrap().flatten().map(|e| PathBuf::from(e.file_name())).collect();
        let d = TempDir::new();
        let (job, pool) = (d.path().join("job"), d.path().join("farms"));
        std::fs::create_dir_all(&job).unwrap();
        let timed = |what: &str, f: &mut dyn FnMut() -> Tally| {
            let t = std::time::Instant::now();
            let tally = f();
            eprintln!("{what}: {:.2} s, {tally:?}", t.elapsed().as_secs_f64());
        };
        let mut farm = None;
        timed("first run, a copy made", &mut || {
            let (f, t) = Farm::take(&job, "f", &pool, &shared, &rels).unwrap();
            farm = Some(f);
            t
        });
        timed("release", &mut || farm.as_mut().unwrap().release());
        timed("next run, the copy taken from the pool", &mut || {
            let (f, t) = Farm::take(&job, "f", &pool, &shared, &rels).unwrap();
            farm = Some(f);
            t
        });
        timed("release", &mut || farm.as_mut().unwrap().release());
    }

    /// PR #67 review: a copy holding a directory a job made mode 000 could not be emptied once renamed
    /// out of the pool, so it stayed as `gone-*`, which nothing lists again: a leak. It goes whole.
    #[test]
    fn an_idle_copy_holding_an_unreadable_directory_is_removed() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        let (farm, _) = take(&p);
        std::fs::create_dir_all(farm.path().join(".cache/locked/inner")).unwrap();
        std::fs::write(farm.path().join(".cache/locked/inner/f"), "x").unwrap();
        std::fs::set_permissions(farm.path().join(".cache/locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        drop(farm);
        let idle = idle(&p.pool);
        assert_eq!(idle.len(), 1);
        remove_idle(&p.pool, &idle[0].0).unwrap();
        assert_eq!(pooled(&p), 0, "nothing left in the pool, not even renamed");
    }

    /// PR #67 review: an empty shared cache made no link, so a copy on another filesystem than the
    /// shared caches was taken; what the job then added could not be linked back (EXDEV) and stayed in the
    /// pool, where gc removes it. A copy on another filesystem is refused before anything is walked.
    #[test]
    fn a_copy_on_another_filesystem_is_refused_even_for_an_empty_cache() {
        use std::os::unix::fs::MetadataExt;
        let p = place();
        let shm = Path::new("/dev/shm");
        let dev = |q: &Path| q.metadata().map(|m| m.dev()).ok();
        if dev(shm).is_none() || dev(shm) == dev(&p.job) {
            eprintln!("SKIPPED: /dev/shm is not another filesystem here");
            return;
        }
        let shared = shm.join(format!("relay-farm-test-{}", std::process::id()));
        std::fs::create_dir_all(&shared).unwrap();
        let taken = Farm::take(&p.job, "f", &p.pool, &shared, &rels());
        let _ = std::fs::remove_dir_all(&shared);
        assert_eq!(taken.err().and_then(|e| e.raw_os_error()), Some(Errno::XDEV.raw_os_error()));
        assert!(!p.job.join("f").exists(), "no copy left");
    }

    /// PR #67 review 3: a pooled copy kept a cache a later run does not link (one a hide rule now covers),
    /// so that run's job could read it in its clone. A copy taken holds the caches it is taken for, only.
    #[test]
    fn a_pooled_copy_holds_only_the_caches_it_is_taken_for() {
        let p = place();
        std::fs::create_dir_all(p.shared.join(".cargo/registry/secret")).unwrap();
        std::fs::write(p.shared.join(".cargo/registry/secret/key"), "k").unwrap();
        std::fs::create_dir_all(p.shared.join(".cache")).unwrap();
        drop(take(&p));
        std::fs::create_dir_all(p.job.join("f-stray")).unwrap();
        let (farm, _) = Farm::take(&p.job, "f", &p.pool, &p.shared, &[PathBuf::from(".cache")]).unwrap();
        let f = farm.path().to_owned();
        assert!(f.join(".cache").is_dir());
        assert!(!f.join(".cargo").exists(), "a cache it was not taken for is purged from the copy");
    }

    /// PR #67 review 3: the shared side was reached by path, every component but the last followed. A
    /// directory on the way to a cache replaced by a symlink (by a job that binds a cache around it) took
    /// the walk out of relay's caches: `take()` made directories and hard links of host files, `release()`
    /// wrote the job's files there. Every component is opened from the caches' root, following none.
    #[test]
    fn the_way_to_a_shared_cache_follows_no_symlink() {
        let p = place();
        let outside = p.job.with_file_name("outside");
        std::fs::create_dir_all(outside.join("hooks")).unwrap();
        std::fs::write(outside.join("hooks/host-file"), "host").unwrap();
        std::fs::create_dir_all(&p.shared).unwrap();
        let rels = [PathBuf::from(".cache/redirect/hooks")];
        std::fs::create_dir_all(p.shared.join(".cache")).unwrap();
        std::os::unix::fs::symlink(&outside, p.shared.join(".cache/redirect")).unwrap();
        let taken = Farm::take(&p.job, "f", &p.pool, &p.shared, &rels);
        assert!(taken.is_err(), "taken through a symlink");
        assert!(!p.job.join("f").exists(), "no copy left");
        // A copy taken while the way was whole, released after it was swapped: nothing written outside.
        std::fs::remove_file(p.shared.join(".cache/redirect")).unwrap();
        let (farm, _) = Farm::take(&p.job, "f", &p.pool, &p.shared, &rels).unwrap();
        std::fs::write(farm.path().join(".cache/redirect/hooks/planted"), "job").unwrap();
        std::fs::remove_dir_all(p.shared.join(".cache/redirect")).unwrap();
        std::os::unix::fs::symlink(&outside, p.shared.join(".cache/redirect")).unwrap();
        drop(farm);
        assert!(!outside.join("hooks/planted").exists(), "release wrote outside relay's caches");
        assert_eq!(std::fs::read_dir(outside.join("hooks")).unwrap().count(), 1);
    }

    /// PR #67 review 3: release made a new directory in the shared caches before linking its files, so a
    /// run taking a copy at that moment kept a half-written package. A new directory appears whole.
    #[test]
    fn a_take_during_a_release_sees_each_new_package_whole_or_not_at_all() {
        const PACKAGES: usize = 60;
        const FILES: usize = 40;
        let p = place();
        std::fs::create_dir_all(p.shared.join(".cache")).unwrap();
        let rels = [PathBuf::from(".cache")];
        let (farm, _) = Farm::take(&p.job, "f", &p.pool, &p.shared, &rels).unwrap();
        for i in 0..PACKAGES {
            let pkg = farm.path().join(format!(".cache/pkg{i}"));
            std::fs::create_dir_all(&pkg).unwrap();
            for j in 0..FILES {
                std::fs::write(pkg.join(format!("f{j}")), "x").unwrap();
            }
        }
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (done, shared, job, pool) = (done.clone(), p.shared.clone(), p.job.clone(), p.pool.with_extension("r"));
            std::thread::spawn(move || {
                let mut partial = Vec::new();
                let mut n = 0;
                while !done.load(std::sync::atomic::Ordering::SeqCst) || n == 0 {
                    n += 1;
                    let (copy, _) = Farm::take(&job, "r", &pool, &shared, &[PathBuf::from(".cache")]).unwrap();
                    for e in std::fs::read_dir(copy.path().join(".cache")).unwrap().flatten() {
                        let files = std::fs::read_dir(e.path()).map_or(0, Iterator::count);
                        if files != FILES {
                            partial.push(format!("{:?}: {files}", e.file_name()));
                        }
                    }
                    drop(copy);
                }
                partial
            })
        };
        drop(farm);
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let partial = reader.join().unwrap();
        assert!(partial.is_empty(), "packages seen half-written: {partial:?}");
        assert_eq!(std::fs::read_dir(p.shared.join(".cache")).unwrap().count(), PACKAGES, "all published");
    }

    /// PR #67 review 3: two bind mounts of one filesystem have one `st_dev`, but `link()` between them fails
    /// with EXDEV, so comparing devices let an empty cache take a copy it could never link back from. Run
    /// in bubblewrap with the caches a bind mount of their own: taking a copy beside them is refused.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "the inner run is told its directories by the outer")]
    fn a_copy_on_another_mount_of_one_filesystem_is_refused() {
        if let Some(dirs) = std::env::var_os("RELAY_FARM_INNER") {
            let dirs = PathBuf::from(dirs);
            let (shared, job, pool) = (dirs.join("caches"), dirs.join("job"), dirs.join("farms"));
            let taken = Farm::take(&job, "f", &pool, &shared, &rels());
            assert_eq!(taken.err().and_then(|e| e.raw_os_error()), Some(Errno::XDEV.raw_os_error()));
            assert!(!job.join("f").exists(), "no copy left");
            return;
        }
        if std::process::Command::new("bwrap").arg("--version").output().is_err() {
            eprintln!("SKIPPED: no bubblewrap");
            return;
        }
        let p = place();
        std::fs::create_dir_all(&p.shared).unwrap();
        let dirs = p.job.parent().unwrap();
        let out = std::process::Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--bind"])
            .args([&p.shared, &p.shared])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "sandbox::farm::tests::a_copy_on_another_mount_of_one_filesystem_is_refused"])
            .env("RELAY_FARM_INNER", dirs)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("1 passed"), "{text}");
    }

    /// What an interrupted run left for a moment (a directory being built, a link probe) goes once it is
    /// older than any run takes over it; what a run is making now stays.
    #[test]
    fn what_an_interrupted_run_left_is_swept_and_what_a_run_makes_now_is_not() {
        let p = place();
        let (farm, _) = take(&p);
        drop(farm);
        let staging = p.shared.join(STAGING);
        let old = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, 1_000_000, 0));
        std::fs::create_dir_all(staging.join(format!("{NEW}{old}/pkg"))).unwrap();
        std::fs::write(staging.join(format!("{NEW}{old}/pkg/f")), "x").unwrap();
        let fresh = format!("{NEW}{}", uuid::Uuid::now_v7());
        std::fs::create_dir_all(staging.join(&fresh)).unwrap();
        std::fs::write(p.shared.join(format!(".cache/{PROBE}{old}")), "").unwrap();
        let (farm, _) = take(&p);
        assert!(!farm.path().join(format!(".cache/{PROBE}{old}")).exists(), "a probe's name is never linked");
        drop(farm);
        assert!(!staging.join(format!("{NEW}{old}")).exists(), "an interrupted build stays for ever");
        assert!(staging.join(&fresh).exists(), "a build under way is not taken for an interrupted one");
        sweep(&open_path(&p.shared.join(".cache")).unwrap(), PROBE);
        assert!(!p.shared.join(format!(".cache/{PROBE}{old}")).exists(), "an old probe stays for ever");
    }

    /// PR #67 review 4: a new directory whose staging failed for an entry (one the job made unreadable) was
    /// published anyway, half-filled. A directory is published whole, or not at all.
    #[test]
    fn a_new_directory_that_could_not_be_staged_whole_is_not_published() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        let (farm, _) = take(&p);
        let pkg = farm.path().join(".cache/pkg");
        std::fs::create_dir_all(pkg.join("locked")).unwrap();
        std::fs::write(pkg.join("index.js"), "x").unwrap();
        std::fs::write(pkg.join("locked/part.js"), "y").unwrap();
        std::fs::set_permissions(pkg.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        drop(farm);
        assert!(!p.shared.join(".cache/pkg").exists(), "a half-staged package was published");
        assert_eq!(std::fs::read_dir(p.shared.join(STAGING)).map_or(0, Iterator::count), 0, "staging left behind");
    }

    /// PR #67 review 4: without `O_TMPFILE`, the link probe linked into the staging directory at the caches'
    /// root, not into the cache: a cache on a mount of its own, empty, was taken though no link could be
    /// made to it. The fallback probes the cache directory itself. Run in bubblewrap, with the cache (not
    /// the root) a bind mount of its own, and the fallback forced.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "the inner run is told its directories by the outer")]
    fn without_tmpfile_the_probe_still_links_against_the_cache_itself() {
        if let Some(dirs) = std::env::var_os("RELAY_FARM_INNER_CACHE") {
            NO_TMPFILE.store(true, std::sync::atomic::Ordering::SeqCst);
            let dirs = PathBuf::from(dirs);
            let (shared, job, pool) = (dirs.join("caches"), dirs.join("job"), dirs.join("farms"));
            let taken = Farm::take(&job, "f", &pool, &shared, &[PathBuf::from(".cache")]);
            assert_eq!(taken.err().and_then(|e| e.raw_os_error()), Some(Errno::XDEV.raw_os_error()));
            assert!(!job.join("f").exists(), "no copy left");
            assert_eq!(std::fs::read_dir(shared.join(".cache")).unwrap().count(), 0, "no probe left in the cache");
            return;
        }
        if std::process::Command::new("bwrap").arg("--version").output().is_err() {
            eprintln!("SKIPPED: no bubblewrap");
            return;
        }
        let p = place();
        std::fs::create_dir_all(p.shared.join(".cache")).unwrap();
        let cache = p.shared.join(".cache");
        let out = std::process::Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--bind"])
            .args([&cache, &cache])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "sandbox::farm::tests::without_tmpfile_the_probe_still_links_against_the_cache_itself"])
            .env("RELAY_FARM_INNER_CACHE", p.job.parent().unwrap())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("1 passed"), "{text}");
    }

    /// An idle copy says when it was given back, and goes whole, read-only directories and all, while
    /// the shared caches keep every file.
    #[test]
    fn an_idle_copy_says_when_it_was_used_and_can_be_removed() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        std::fs::create_dir_all(p.shared.join(".cache/a")).unwrap();
        std::fs::write(p.shared.join(".cache/a/f"), "x").unwrap();
        let before = crate::bus::now_micros();
        let (farm, _) = take(&p);
        std::fs::create_dir_all(farm.path().join(".cache/go")).unwrap();
        std::fs::set_permissions(farm.path().join(".cache/go"), std::fs::Permissions::from_mode(0o555)).unwrap();
        drop(farm);
        let idle = idle(&p.pool);
        assert_eq!(idle.len(), 1);
        assert!(idle[0].1 >= before / 1000 * 1000 && idle[0].1 <= crate::bus::now_micros(), "{idle:?}");
        remove_idle(&p.pool, &idle[0].0).unwrap();
        assert_eq!(pooled(&p), 0);
        assert_eq!(std::fs::read_to_string(p.shared.join(".cache/a/f")).unwrap(), "x");
        assert!(remove_idle(&p.pool, &idle[0].0).is_err(), "taken or gone already: nothing to do");
    }

    /// A copy left by a run relay never saw end is used again in place, and nothing it held is lost.
    #[test]
    fn a_copy_left_in_place_is_used_again() {
        let p = place();
        let (farm, _) = take(&p);
        std::fs::write(farm.path().join(".cache/left"), "x").unwrap();
        std::mem::forget(farm);
        let (farm, _) = take(&p);
        assert!(farm.path().join(".cache/left").exists());
        drop(farm);
        assert!(p.shared.join(".cache/left").exists());
    }
}
