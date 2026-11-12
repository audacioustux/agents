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
//! - **Nothing followed.** A copy is written by the jobs that had it, and the shared caches by jobs with
//!   no tree (a review binds them as before), so both are walked through directory handles opened
//!   without following a symlink, and a symlink is linked as a symlink. A copy is walked only after its
//!   job's sandbox has ended (its PID namespace with it), through the handles relay opened when it put
//!   it in place, so a job that moves or replaces the directories around it changes nothing relay reads.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
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
        for rel in rels {
            let from = shared.join(rel);
            std::fs::create_dir_all(&from)?;
            let brought = open_path(&from).and_then(|from| {
                let to = make_beneath(&farm.root, rel)?;
                link_tree(&from, &to, false, &mut tally)
            });
            if let Err(e) = brought {
                // Nothing the job did is in it: removed, not linked back.
                farm.released = true;
                let _ = empty(&farm.root);
                let _ = rustix::fs::unlinkat(&farm.parent, &farm.name, AtFlags::REMOVEDIR);
                return Err(e);
            }
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
        for rel in &self.rels {
            let Ok(from) = open_beneath(&self.root, rel) else { continue };
            let to = std::fs::create_dir_all(self.shared.join(rel)).and_then(|()| open_path(&self.shared.join(rel)));
            match to {
                Ok(to) => {
                    let _ = link_tree(&from, &to, true, &mut tally);
                }
                Err(_) => tally.failed += 1,
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
            let _ = empty(&self.root);
            let _ = rustix::fs::unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR);
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
            let id = uuid::Uuid::parse_str(name.strip_prefix(IDLE)?).ok()?;
            let (secs, nanos) = id.get_timestamp()?.to_unix();
            Some((PathBuf::from(name), secs * 1_000_000 + u64::from(nanos) / 1000))
        })
        .collect();
    out.sort_by_key(|(_, at)| *at);
    out
}

/// Removes the idle copy `name` from `pool`: first renamed out of the pool, so a run taking it at the
/// same moment has either it whole or none; then emptied, never following a symlink a job left in it.
/// Its files are the caches' own (hard links), so this frees directories, not data.
pub fn remove_idle(pool: &Path, name: &Path) -> io::Result<()> {
    let pool_fd = open_path(pool)?;
    let gone = format!("gone-{}", uuid::Uuid::now_v7());
    rustix::fs::renameat(&pool_fd, name, &pool_fd, gone.as_str())?;
    let copy = open_dir(&pool_fd, gone.as_str())?;
    empty(&copy)?;
    Ok(rustix::fs::unlinkat(&pool_fd, gone.as_str(), AtFlags::REMOVEDIR)?)
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
/// `own`: `from` is a copy, whose directories get the same. EXDEV stops it (no link can be made from
/// there to here); any other failure leaves that entry out.
fn link_tree(from: &OwnedFd, to: &OwnedFd, own: bool, tally: &mut Tally) -> io::Result<()> {
    tally.read += 1;
    let have: HashMap<CString, bool> = entries(to)?.into_iter().collect();
    for (name, is_dir) in entries(from)? {
        let name: &CStr = &name;
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
        if there.is_none() {
            match rustix::fs::mkdirat(to, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => tally.dirs += 1,
                Err(Errno::EXIST) => {}
                Err(_) => {
                    tally.failed += 1;
                    continue;
                }
            }
        }
        let Ok(sub_to) = open_dir(to, name) else {
            tally.failed += 1;
            continue;
        };
        link_tree(&sub_from, &sub_to, own, tally)?;
        if there.is_none() {
            let _ = rustix::fs::fchmod(&sub_to, Mode::from_raw_mode(mode | 0o700));
        }
    }
    Ok(())
}

/// Removes everything in `fd`, never following a symlink; directories are made writable first.
fn empty(fd: &OwnedFd) -> io::Result<()> {
    let _ = rustix::fs::fchmod(fd, Mode::from_raw_mode(0o700));
    for (name, is_dir) in entries(fd)? {
        if is_dir && let Ok(sub) = open_dir(fd, name.as_c_str()) {
            let _ = empty(&sub);
            let _ = rustix::fs::unlinkat(fd, name.as_c_str(), AtFlags::REMOVEDIR);
        } else {
            let _ = rustix::fs::unlinkat(fd, name.as_c_str(), AtFlags::empty());
        }
    }
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
