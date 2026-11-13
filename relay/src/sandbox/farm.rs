//! A job's own copy of the project's linked caches, a whole private cache inside the mount that holds its
//! tree.
//!
//! `link()` works within one mount only: across two it fails with EXDEV, even on one filesystem. A
//! package manager links from its store into the tree it installs (bun, pnpm, uv), so a cache bound
//! apart from the tree made it copy every package. For the caches a project declares such a tool links
//! from (`sandbox.linked_caches`), a job with a tree gets a copy of its own, in the tree's own mount (a
//! directory in its clone's `.git`, or in its check's directory), and each such cache path is a symlink
//! into it ([`super::Mount::Symlink`]). Every other cache stays a bind of relay's directory for it.
//!
//! - **Whole and private.** A copy is a cache in its own right, not a view of relay's: a new one starts
//!   empty, and what a job fetches into it stays in it. Copies are kept in a pool beside the place they
//!   are used (on the same filesystem) and moved only by rename: a run takes an idle one into place, and
//!   gives it back the same way. Relay never links, copies or walks a cache's files; it renames, cuts a
//!   copy down to the caches this run links ([`purge`]), and removes.
//! - **What it costs.** A run pays a rename to take and a rename to give back. A new copy is cold (its
//!   first install fetches everything), and each copy warms on its own: a package new to the project is
//!   fetched once per copy that needs it.
//! - **What is shared.** A copy's next holders see what its earlier holders left, as any cache's users
//!   do. Its files share inodes with every tree installed from it (that is what linking is for), so a
//!   file edited in place in such a tree is edited in the copy, for its next holders too. Nothing else:
//!   no file is shared between a copy and relay's caches, between two copies, or between the pools of
//!   sessions and of checks (each a directory of its own, named for its purpose).
//! - **Only what the plan links.** A copy is taken for the caches [`super::linkable`] names, the same
//!   judge the plan asks, and anything else in it is removed when it is taken: a cache left a bind is
//!   never in a job's tree.
//! - **Only with proof it ended.** A copy goes back to the pool for another run only when it is given back
//!   with evidence that everything its sandbox started has ended ([`Ended`], [`Farm::give_back`]). Dropped
//!   any other way (an error, a panic, a sandbox killed from outside), it is retired: renamed into the
//!   pool as gone, for gc, never taken again; so is one that cannot be cut down for its next run.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::config::CachePath;

/// Evidence that everything a copy's sandbox started has ended: what a copy is given back to the pool with,
/// for another run ([`Farm::give_back`]). Made only from what saw it end.
#[derive(Debug, Clone, Copy)]
pub struct Ended(());

impl Ended {
    /// The supervisor exited after reaping its whole tree, sandboxed processes included.
    pub fn reaped(supervisor: &crate::supervised::Supervised) -> Option<Self> {
        supervisor.tree_ended().then_some(Self(()))
    }

    /// bubblewrap, waited for, exited with a code: it waits for its namespace's init, which the kernel
    /// lets end only after every other process of the namespace has. Killed by a signal instead (from
    /// outside), it proves nothing.
    pub fn waited(bwrap: std::process::ExitStatus) -> Option<Self> {
        bwrap.code().map(|_| Self(()))
    }
}

/// A job's copy of the linked caches, in place. Given back to its pool for another run only with [`Ended`];
/// dropped otherwise, retired.
#[derive(Debug)]
pub struct Farm {
    path: PathBuf,
    name: CString,
    /// The directory holding the copy, and the copy itself, as opened when it was put in place.
    parent: OwnedFd,
    root: OwnedFd,
    pool: PathBuf,
    released: bool,
}

impl Farm {
    /// Puts a copy of the linked caches `rels` at `name` in `parent` (a directory only relay writes, or a
    /// `.git` reached from one without following a link): the copy a run that never ended left there,
    /// else the idle one given back last to `pool`, else a new, empty one; then cuts it down to `rels`.
    /// Fails, leaving none, when it cannot be cut down.
    pub fn take(parent: &Path, name: &str, pool: &Path, rels: &[CachePath]) -> io::Result<Self> {
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
            released: false,
        };
        let cut = (|| -> io::Result<()> {
            // A copy from the pool may hold caches this run does not link (one a hide rule now covers):
            // it holds these and nothing else.
            purge(&farm.root, &rels.iter().map(|r| &**r).collect::<Vec<&Path>>())?;
            for rel in rels {
                make_beneath(&farm.root, rel)?;
            }
            Ok(())
        })();
        if let Err(e) = cut {
            // Retired (a rename, whatever it holds), so the next run starts afresh rather than fail on it.
            farm.release(GONE);
            return Err(e);
        }
        Ok(farm)
    }

    /// Where the copy is: each cache `rel` is `path/rel`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gives the copy back to the pool for another run: only with proof its sandbox has ended.
    pub fn give_back(mut self, _: Ended) {
        self.release(IDLE);
    }

    /// Moves the copy into the pool as `to` ([`IDLE`] or [`GONE`]), by rename (or, when it cannot, removes
    /// it). Only the directory relay put there goes: a job may have put another in its place. Once only.
    fn release(&mut self, to: &str) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        let same = |a: &rustix::fs::Stat, b: &rustix::fs::Stat| (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino);
        let ours = match (
            rustix::fs::fstat(&self.root),
            rustix::fs::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW),
        ) {
            (Ok(a), Ok(b)) => same(&a, &b),
            _ => false,
        };
        if ours && !to_pool(&self.pool, &self.parent, &self.name, to) {
            let _ = crate::hostdir::remove_beneath(self.parent.as_fd(), &self.name);
        }
    }
}

impl Drop for Farm {
    /// Without proof its sandbox ended: retired, for gc.
    fn drop(&mut self) {
        self.release(GONE);
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

/// Moves the copy `name` in `parent` into `pool`, named `prefix` and a v7 UUID ([`IDLE`] for a run to
/// take, [`GONE`] for gc to remove); false when it cannot.
fn to_pool(pool: &Path, parent: &OwnedFd, name: &CStr, prefix: &str) -> bool {
    let made = std::fs::DirBuilder::new().recursive(true).create(pool);
    let Ok(pool_fd) = made.and_then(|()| open_path(pool)) else { return false };
    let to = format!("{prefix}{}", uuid::Uuid::now_v7());
    rustix::fs::renameat(parent, name, &pool_fd, to.as_str()).is_ok()
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
/// it made mode 000), never following a symlink, at any depth ([`crate::hostdir::remove_tree`]).
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
        let cn = CString::new(n.as_bytes()).map_err(|_| Errno::INVAL)?;
        // A directory a job made unreadable is made usable again (and kept); anything else under the name
        // (a symlink to elsewhere) goes.
        fd = match crate::hostdir::open_writable_dir(fd.as_fd(), &cn) {
            Ok(sub) => sub,
            Err(Errno::NOTDIR | Errno::LOOP) => {
                rustix::fs::unlinkat(&fd, &cn, AtFlags::empty())?;
                rustix::fs::mkdirat(&fd, &cn, Mode::from_raw_mode(0o700))?;
                open_dir(&fd, &cn)?
            }
            Err(e) => return Err(e.into()),
        };
    }
    Ok(fd)
}

/// Takes the copy `name` out of `parent` (a directory only relay writes, or a `.git` reached from one
/// without following a link), if one is there: one a run left that never ended, which nothing will take
/// (this run links nothing). It is retired into `pool` (a rename, whatever it holds), else removed
/// through `parent`'s handle, never following a symlink, whatever modes or depth a job left in it. An
/// error only when it is still there after both.
pub fn clear(parent: &Path, name: &str, pool: &Path) -> io::Result<()> {
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
    if !to_pool(pool, &parent_fd, &cname, GONE) {
        let _ = crate::hostdir::remove_beneath(parent_fd.as_fd(), &cname);
    }
    match rustix::fs::statat(&parent_fd, &cname, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        _ => Err(io::Error::other(format!("{} could not be removed", parent.join(name).display()))),
    }
}

/// Makes the cache `rel` in relay's caches `shared`, from their root one directory at a time, following
/// no symlink: what a cache bound into a sandbox is made with.
pub fn make_cache_dir(shared: &Path, rel: &CachePath) -> io::Result<()> {
    make_shared(&shared_root(shared)?, rel).map(drop)
}

/// Puts the empty file `name` in the cache `rel` of relay's caches `shared`, where nothing of that name is,
/// following no symlink on the way or at it (the probe's marker).
pub fn put_in_cache(shared: &Path, rel: &CachePath, name: &str) -> io::Result<()> {
    let dir = make_shared(&shared_root(shared)?, rel)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(&dir, name, flags, Mode::from_raw_mode(0o644)).map(drop).map_err(Into::into)
}

/// Removes the file `name` from the cache `rel` of relay's caches `shared`, following no symlink.
pub fn remove_in_cache(shared: &Path, rel: &CachePath, name: &str) -> io::Result<()> {
    let dir = open_beneath(&open_path(shared)?, rel)?;
    rustix::fs::unlinkat(&dir, name, AtFlags::empty()).map_err(Into::into)
}

/// The names in the cache `rel` of relay's caches `shared` (not below), read through handles that follow
/// no symlink.
pub fn names_in_cache(shared: &Path, rel: &CachePath) -> io::Result<Vec<String>> {
    let dir = open_beneath(&open_path(shared)?, rel)?;
    Ok(entries(&dir)?.into_iter().filter_map(|(n, _)| n.into_string().ok()).collect())
}

/// The name of `relay probe`'s marker in a cache, then the probe's process id.
pub const MARKER: &str = ".relay-probe-";

/// When the entry `name` (`prefix`, then a v7 UUID) was made, in Unix microseconds.
fn made_at(name: &str, prefix: &str) -> Option<u64> {
    let id = uuid::Uuid::parse_str(name.strip_prefix(prefix)?).ok()?;
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    Some(secs * 1_000_000 + u64::from(nanos) / 1000)
}

/// Relay's caches (`<data dir>/<project>/caches`), opened: a directory only relay writes, reached by a
/// path only relay writes.
fn shared_root(shared: &Path) -> io::Result<OwnedFd> {
    std::fs::DirBuilder::new().recursive(true).create(shared)?;
    open_path(shared)
}

/// `rel` in relay's caches, from their opened root one directory at a time, following no symlink and
/// making each directory that is not there. Anything else in the way is refused, never replaced: what a
/// bound cache and the probe's markers are made through.
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

/// Removes from a copy everything but the caches `rels` (whole) and the directories on the way to them.
fn purge(at: &OwnedFd, rels: &[&Path]) -> io::Result<()> {
    for (name, is_dir) in entries(at)? {
        let here = Path::new(OsStr::from_bytes(name.to_bytes()));
        let below: Vec<&Path> = rels.iter().filter_map(|r| r.strip_prefix(here).ok()).collect();
        if below.iter().any(|b| b.as_os_str().is_empty()) {
            continue;
        }
        match (is_dir && !below.is_empty()).then(|| crate::hostdir::open_writable_dir(at.as_fd(), &name)) {
            Some(Ok(sub)) => purge(&sub, &below)?,
            _ => crate::hostdir::remove_beneath(at.as_fd(), &name)?,
        }
    }
    Ok(())
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

    fn cache(spelled: &str) -> CachePath {
        CachePath::new(spelled).unwrap()
    }

    fn rels() -> Vec<CachePath> {
        vec![cache(".cache"), cache(".cargo/registry")]
    }

    struct Place {
        _d: TempDir,
        job: PathBuf,
        pool: PathBuf,
    }

    fn place() -> Place {
        let d = TempDir::new();
        let (job, pool) = (d.path().join("job"), d.path().join("farms"));
        std::fs::create_dir_all(&job).unwrap();
        Place { _d: d, job, pool }
    }

    fn take(p: &Place) -> Farm {
        Farm::take(&p.job, "f", &p.pool, &rels()).unwrap()
    }

    /// What only supervision makes outside tests: the proof a copy's sandbox ended.
    fn ended() -> Ended {
        Ended(())
    }

    fn pooled(p: &Place) -> usize {
        std::fs::read_dir(&p.pool).map_or(0, Iterator::count)
    }

    /// A new copy is empty; what a job leaves in it goes back to the pool with it, by rename, and the next
    /// run takes that same copy (the one given back last); two runs at once hold two copies.
    #[test]
    fn a_copy_is_private_and_goes_back_whole() {
        let p = place();
        let farm = take(&p);
        let f = farm.path().to_owned();
        assert!(f.join(".cache").is_dir() && f.join(".cargo/registry").is_dir());
        assert_eq!(std::fs::read_dir(f.join(".cache")).unwrap().count(), 0, "a new copy is cold");
        std::fs::create_dir_all(f.join(".cache/bun/pkg")).unwrap();
        std::fs::write(f.join(".cache/bun/pkg/index.js"), "x").unwrap();
        farm.give_back(ended());
        assert!(!f.exists() && pooled(&p) == 1, "the copy went back to the pool");
        let farm = take(&p);
        assert_eq!(std::fs::read_to_string(farm.path().join(".cache/bun/pkg/index.js")).unwrap(), "x");
        assert_eq!(pooled(&p), 0, "taken from the pool");
        let other = Farm::take(&p.job, "g", &p.pool, &rels()).unwrap();
        assert_eq!(std::fs::read_dir(other.path().join(".cache")).unwrap().count(), 0, "none idle: a new one");
        farm.give_back(ended());
        other.give_back(ended());
        assert_eq!(pooled(&p), 2);
    }

    /// A copy is the job's to write, and the directory around it too: it goes back through the handles
    /// relay opened when it put it there, so a job that moved the directory and left a link to elsewhere
    /// in its place changes nothing relay touches; and a directory the job put where its copy was is not
    /// taken into the pool.
    #[test]
    fn a_release_follows_nothing_the_job_left() {
        let p = place();
        let outside = p.job.with_file_name("outside");
        std::fs::create_dir_all(outside.join("secret")).unwrap();
        let farm = take(&p);
        std::os::unix::fs::symlink(&outside, farm.path().join(".cache/planted")).unwrap();
        let moved = p.job.with_file_name("moved");
        std::fs::rename(&p.job, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &p.job).unwrap();
        farm.give_back(ended());
        assert!(!moved.join("f").exists() && idle(&p.pool).len() == 1, "given back through its handle");
        assert!(outside.join("secret").is_dir() && !outside.join("f").exists(), "nothing outside touched");

        std::fs::remove_file(&p.job).unwrap();
        std::fs::rename(&moved, &p.job).unwrap();
        let farm = take(&p);
        std::fs::rename(farm.path(), p.job.join("away")).unwrap();
        std::fs::create_dir_all(p.job.join("f/planted")).unwrap();
        farm.give_back(ended());
        assert!(p.job.join("f/planted").exists() && pooled(&p) == 0, "another directory is not pooled");
    }

    /// Dropped without proof its sandbox ended (supervision could not see it end whole, a check's bwrap was
    /// killed from outside): the copy is never given to another run, only to gc.
    #[test]
    fn a_copy_without_proof_its_sandbox_ended_never_goes_back_to_a_run() {
        let p = place();
        let farm = take(&p);
        drop(farm);
        assert!(idle(&p.pool).is_empty(), "a retired copy is idle for the next run");
        assert_eq!(abandoned(&p.pool).len(), 1, "gc removes it");
        let farm = take(&p);
        assert_eq!(std::fs::read_dir(farm.path().join(".cache")).unwrap().count(), 0, "a new copy, not that one");
    }

    /// PR #67 review 3: a pooled copy kept a cache a later run does not link (one a hide rule now covers),
    /// so that run's job could read it in its clone. A copy taken holds the caches it is taken for, only.
    #[test]
    fn a_pooled_copy_holds_only_the_caches_it_is_taken_for() {
        let p = place();
        let farm = take(&p);
        std::fs::create_dir_all(farm.path().join(".cargo/registry/secret")).unwrap();
        std::fs::write(farm.path().join("stray"), "x").unwrap();
        farm.give_back(ended());
        let farm = Farm::take(&p.job, "f", &p.pool, &[cache(".cache")]).unwrap();
        assert!(farm.path().join(".cache").is_dir());
        assert!(!farm.path().join(".cargo").exists() && !farm.path().join("stray").exists(), "not purged");
    }

    /// PR #67 review: a copy holding a directory a job made mode 000 could not be emptied once renamed out
    /// of the pool, so it stayed as `gone-*`, which nothing lists again: a leak. It goes whole.
    #[test]
    fn an_idle_copy_holding_an_unreadable_directory_is_removed() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        let farm = take(&p);
        std::fs::create_dir_all(farm.path().join(".cache/locked/inner")).unwrap();
        std::fs::write(farm.path().join(".cache/locked/inner/f"), "x").unwrap();
        std::fs::set_permissions(farm.path().join(".cache/locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        farm.give_back(ended());
        let idle = idle(&p.pool);
        assert_eq!(idle.len(), 1);
        remove_idle(&p.pool, &idle[0].0).unwrap();
        assert_eq!(pooled(&p), 0, "nothing left in the pool, not even renamed");
    }

    /// An idle copy says when it was given back, and goes whole on removal; one already gone is an error.
    #[test]
    fn an_idle_copy_says_when_it_was_used_and_can_be_removed() {
        let p = place();
        let before = crate::bus::now_micros();
        take(&p).give_back(ended());
        let idle = idle(&p.pool);
        assert_eq!(idle.len(), 1);
        assert!(idle[0].1 >= before / 1000 * 1000 && idle[0].1 <= crate::bus::now_micros(), "{idle:?}");
        remove_idle(&p.pool, &idle[0].0).unwrap();
        assert_eq!(pooled(&p), 0);
        assert!(remove_idle(&p.pool, &idle[0].0).is_err(), "taken or gone already: nothing to do");
    }

    /// Fable review of (e): a copy dropped without proof that its sandbox ended (here, on a panic; else an
    /// error from the runner) went back to the pool for the next run. A copy is retired unless given back
    /// with that proof.
    #[test]
    fn a_copy_dropped_on_a_panic_is_retired() {
        let p = place();
        let farm = take(&p);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _farm = farm;
            panic!("the runner failed");
        }));
        assert!(panicked.is_err());
        assert!(idle(&p.pool).is_empty(), "a copy dropped on a panic went to the next run");
        assert_eq!(abandoned(&p.pool).len(), 1, "retired, for gc");
    }

    /// Fable and Copilot on (e): a check gave its copy back whatever became of its bwrap; killed from
    /// outside, bwrap returns before its namespace has ended. Only an exit with a code proves the end: a
    /// bwrap killed by a signal gives no proof, and the check's copy is retired.
    #[test]
    fn a_check_whose_sandbox_was_killed_retires_its_copy() {
        let p = place();
        let farm = take(&p);
        let killed = std::process::Command::new("sh").args(["-c", "kill -9 $$"]).status().unwrap();
        let exited = std::process::Command::new("sh").args(["-c", "exit 1"]).status().unwrap();
        assert!(Ended::waited(killed).is_none() && Ended::waited(exited).is_some());
        match Ended::waited(killed) {
            Some(ended) => farm.give_back(ended),
            None => drop(farm),
        }
        assert!(idle(&p.pool).is_empty() && abandoned(&p.pool).len() == 1, "a killed check's copy was pooled");
    }

    /// Fable review of (e): removal held a handle for every level, so a tree deeper than the process may
    /// open (here 600 levels under a limit of 128) could be neither cut out of a copy nor removed from the
    /// pool, and failed every take after. Run with the limit lowered, in a process of its own.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "the inner run is told its directory by the outer")]
    fn a_tree_deeper_than_the_open_handles_allowed_is_still_cut_and_removed() {
        if let Some(dir) = std::env::var_os("RELAY_FARM_INNER_DEEP") {
            let job = PathBuf::from(dir);
            let pool = job.with_file_name("farms");
            let farm = Farm::take(&job, "f", &pool, &rels()).unwrap();
            let deep: PathBuf = std::iter::repeat_n("d", 600).collect();
            std::fs::create_dir_all(farm.path().join("stray").join(&deep)).unwrap();
            std::fs::create_dir_all(farm.path().join(".cache/pkg").join(&deep)).unwrap();
            std::mem::forget(farm);
            let farm = Farm::take(&job, "f", &pool, &[cache(".cargo/registry")]).unwrap();
            assert!(!farm.path().join("stray").exists() && !farm.path().join(".cache").exists(), "not cut");
            drop(farm);
            for (name, _) in idle(&pool).into_iter().chain(abandoned(&pool).into_iter().map(|n| (n, 0))) {
                remove_idle(&pool, &name).unwrap();
            }
            assert_eq!(std::fs::read_dir(&pool).map_or(0, Iterator::count), 0, "left in the pool");
            return;
        }
        let p = place();
        let exe = std::env::current_exe().unwrap();
        let test = "sandbox::farm::tests::a_tree_deeper_than_the_open_handles_allowed_is_still_cut_and_removed";
        let out = std::process::Command::new("sh")
            .args(["-c", "ulimit -n 128 && exec \"$0\" --exact \"$1\" --test-threads=1"])
            .arg(&exe)
            .arg(test)
            .env("RELAY_FARM_INNER_DEEP", &p.job)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("1 passed"), "{text}");
    }

    /// Fable review of (e): a cache directory a job made mode 000 was unlinked as if it were not a directory
    /// (which fails), so the next run lost the whole copy. It is made usable again and kept.
    #[test]
    fn a_cache_directory_made_unreadable_is_kept_by_the_next_taker() {
        use std::os::unix::fs::PermissionsExt;
        let p = place();
        let farm = take(&p);
        std::fs::write(farm.path().join(".cache/pkg"), "warm").unwrap();
        std::fs::set_permissions(farm.path().join(".cache"), std::fs::Permissions::from_mode(0o000)).unwrap();
        std::mem::forget(farm);
        let farm = take(&p);
        assert_eq!(std::fs::read_to_string(farm.path().join(".cache/pkg")).unwrap(), "warm", "the copy was lost");
    }

    /// A copy left by a run relay never saw end is used again in place, and nothing it held is lost.
    #[test]
    fn a_copy_left_in_place_is_used_again() {
        let p = place();
        let farm = take(&p);
        std::fs::write(farm.path().join(".cache/left"), "x").unwrap();
        std::mem::forget(farm);
        let farm = take(&p);
        assert!(farm.path().join(".cache/left").exists());
    }
}
