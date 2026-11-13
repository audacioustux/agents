//! A directory only the host writes, held open: where relay puts the files it hands a session (a
//! prompt file) or keeps beside one (a check's output, a scratch git index).
//!
//! A session's own directories are the session's: anything relay wrote there by path could be a
//! symlink the session planted, and the write would land wherever it points (`~/.bashrc`). So
//! host-written files never go in a directory a session can write. They go in a [`HostDir`], which no
//! sandbox mounts writable (a session sees it, at most, read-only), opened once without following a
//! symlink and written only through that handle: each file by a bare name, created afresh with
//! `O_CREAT | O_EXCL | O_NOFOLLOW`, so a stale or planted entry is replaced, never followed.

use std::ffi::{CStr, CString, OsStr};
use std::fs::{DirBuilder, File};
use std::io::{self, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

#[derive(Debug)]
pub struct HostDir {
    fd: OwnedFd,
    path: PathBuf,
}

/// Flags every open of a directory takes: a symlink in the last place is refused, not followed.
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// `name` when it names an entry directly inside a directory: not empty, no `/`, not `.` or `..`.
fn bare(name: &str) -> io::Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is not a bare file name")));
    }
    Ok(name)
}

impl HostDir {
    /// Opens `path`, creating it (owner-only) and what is missing above it. Every directory above it
    /// must be relay's own; the last one may not be a symlink.
    pub fn open(path: &Path) -> io::Result<Self> {
        DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        let fd = rustix::fs::open(path, dir_flags(), Mode::empty())?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    /// Makes `path` a new directory and opens it: fails when anything is there already. For a
    /// directory in a place others can write (`/tmp`).
    pub fn create_new(path: &Path) -> io::Result<Self> {
        DirBuilder::new().mode(0o700).create(path)?;
        let fd = rustix::fs::open(path, dir_flags(), Mode::empty())?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where `name` in it is, for a command line or a mount.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Creates `name` afresh, owner-only, for writing: whatever was there (a file, a symlink) is
    /// unlinked first, never followed, and the new file is made with `O_EXCL | O_NOFOLLOW`.
    pub fn create(&self, name: &str) -> io::Result<File> {
        let name = bare(name)?;
        self.remove(name)?;
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(rustix::fs::openat(&self.fd, name, flags, Mode::from_raw_mode(0o600))?))
    }

    /// Writes `bytes` to `name` afresh (see [`HostDir::create`]), and returns where it is.
    pub fn write(&self, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        self.create(name)?.write_all(bytes)?;
        Ok(self.join(name))
    }

    /// Opens `name` for reading, refusing a symlink.
    pub fn read(&self, name: &str) -> io::Result<File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(rustix::fs::openat(&self.fd, bare(name)?, flags, Mode::empty())?))
    }

    /// Removes the file (or symlink) `name`, if there is one.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        match rustix::fs::unlinkat(&self.fd, bare(name)?, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Removes `path` and everything under it, which relay owns and fully controls (a check's working
/// directory and its `TMPDIR`, a session's scratch directory), whatever modes a child process left on
/// it: a directory it set to `0o000` is made `u+rwx` (the owner's bits only) and then emptied. Nothing
/// is followed out of the tree: each entry is looked at without following a symlink, a symlink is
/// unlinked and its target is not touched (nor is its mode), and each directory is opened by name
/// from its parent's open handle, so a swap for a symlink is refused. A `path` already gone is
/// removed. An error names the entry that could not be removed. The one function that clears such a
/// directory: do not copy its loop, call it.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{} holds a NUL", path.display())))?;
    remove_at(CWD, &name, &mut path.to_path_buf())
}

/// [`remove_tree`] for the entry `name` in the open directory `parent` (writable already), so that nothing
/// on the way to it is a path either: a directory around it moved or swapped for a symlink changes nothing.
pub fn remove_beneath(parent: BorrowedFd<'_>, name: &CStr) -> io::Result<()> {
    remove_at(parent, name, &mut PathBuf::from(OsStr::from_bytes(name.to_bytes())))
}

/// An error at `at`, saying where.
fn failed(at: &Path, e: Errno) -> io::Error {
    let e = io::Error::from(e);
    io::Error::new(e.kind(), format!("{}: {e}", at.display()))
}

/// Removes `name` in `parent` (a directory made writable already), `at` being its path for errors.
fn remove_at(parent: BorrowedFd<'_>, name: &CStr, at: &mut PathBuf) -> io::Result<()> {
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(e) => return Err(failed(at, e)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(e) => Err(failed(at, e)),
        };
    }
    let dir = open_writable_dir(parent, name).map_err(|e| failed(at, e))?;
    for entry in Dir::read_from(&dir).map_err(|e| failed(at, e))? {
        let entry = entry.map_err(|e| failed(at, e))?;
        let child = entry.file_name();
        if child == c"." || child == c".." {
            continue;
        }
        at.push(OsStr::from_bytes(child.to_bytes()));
        remove_at(dir.as_fd(), child, at)?;
        at.pop();
    }
    match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(failed(at, e)),
    }
}

/// Opens the directory `name` in `parent` (not following a symlink) for reading and removing from, giving
/// its owner `rwx` first when a child process took them away.
fn open_writable_dir(parent: BorrowedFd<'_>, name: &CStr) -> Result<OwnedFd, Errno> {
    let dir = match rustix::fs::openat(parent, name, dir_flags(), Mode::empty()) {
        Ok(dir) => dir,
        Err(Errno::ACCESS) => {
            // No read bit: open a handle that needs none and change the mode through it. The handle is
            // the directory itself, not a path that could be swapped for a symlink.
            let handle = rustix::fs::openat(
                parent,
                name,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            rustix::fs::chmod(format!("/proc/self/fd/{}", handle.as_raw_fd()), Mode::from_raw_mode(0o700))?;
            rustix::fs::openat(parent, name, dir_flags(), Mode::empty())?
        }
        Err(e) => return Err(e),
    };
    let mode = rustix::fs::fstat(&dir)?.st_mode & 0o7777;
    if mode & 0o700 != 0o700 {
        rustix::fs::fchmod(&dir, Mode::from_raw_mode(mode | 0o700))?;
    }
    Ok(dir)
}

/// Makes `path` an empty directory, owner-only, whatever was there: a tree is removed by
/// [`remove_tree`] (without following a symlink in it, whatever modes it holds), and the new directory
/// is made with `mkdir`, which does not follow one either. For a session's (or a check's) working
/// directory made fresh for each run, in a place only relay writes.
pub fn fresh_dir(path: &Path) -> io::Result<()> {
    remove_tree(path)?;
    if let Some(parent) = path.parent() {
        DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    rustix::fs::mkdirat(CWD, path, Mode::from_raw_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    /// Audit finding 1: a session plants a symlink where relay writes its prompt; the write replaces the
    /// link and never touches what it pointed at.
    #[test]
    fn a_planted_symlink_is_replaced_not_followed() {
        let d = Dir::new();
        let victim = d.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let inbox = HostDir::open(&d.path().join("jobs/j.in")).unwrap();
        std::os::unix::fs::symlink(&victim, inbox.join("prompt.md")).unwrap();
        let at = inbox.write("prompt.md", b"the brief").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious", "the write followed the symlink");
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "the brief");
        assert!(!std::fs::symlink_metadata(&at).unwrap().file_type().is_symlink());
        // Reading back refuses a symlink too.
        std::fs::remove_file(&at).unwrap();
        std::os::unix::fs::symlink(&victim, &at).unwrap();
        assert!(inbox.read("prompt.md").is_err(), "a read followed the symlink");
    }

    #[test]
    fn only_bare_names_and_no_symlinked_directory() {
        let d = Dir::new();
        let inbox = HostDir::open(&d.path().join("in")).unwrap();
        for bad in ["", ".", "..", "../x", "a/b", "/etc/passwd"] {
            assert!(inbox.write(bad, b"x").is_err(), "{bad:?}");
        }
        std::os::unix::fs::symlink(d.path().join("in"), d.path().join("link")).unwrap();
        assert!(HostDir::open(&d.path().join("link")).is_err(), "the directory itself may not be a symlink");
        assert!(HostDir::create_new(&d.path().join("in")).is_err(), "create_new takes nothing already there");
    }

    #[test]
    fn a_fresh_directory_replaces_a_planted_symlink_and_keeps_its_target() {
        let d = Dir::new();
        let target = d.path().join("home");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), "x").unwrap();
        let ws = d.path().join("j.check.d");
        std::os::unix::fs::symlink(&target, &ws).unwrap();
        fresh_dir(&ws).unwrap();
        assert!(std::fs::symlink_metadata(&ws).unwrap().is_dir(), "a real directory now");
        assert!(target.join("keep").exists(), "what the link pointed at is untouched");
        std::os::unix::fs::symlink(&target, ws.join("tmp")).unwrap();
        fresh_dir(&ws).unwrap();
        assert!(std::fs::read_dir(&ws).unwrap().next().is_none() && target.join("keep").exists());
    }

    use std::os::unix::fs::PermissionsExt as _;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
    }

    /// The incident: a test set its tempdir (inside the check's work dir) to `0o000`, with a file in it.
    #[test]
    fn a_tree_with_a_mode_zero_directory_is_cleared_and_the_next_check_runs() {
        let d = Dir::new();
        let work = d.path().join("jobs/j.bar.d");
        fresh_dir(&work).unwrap();
        let stuck = work.join("tmp/.tmpAbC1/deeper");
        std::fs::create_dir_all(&stuck).unwrap();
        std::fs::write(stuck.join("f"), "x").unwrap();
        std::fs::write(work.join("tmp/.tmpAbC1/g"), "x").unwrap();
        for p in [&stuck, &work.join("tmp/.tmpAbC1")] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o0)).unwrap();
        }
        // A read-only one, too: listable, not writable.
        let ro = work.join("tmp/ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("f"), "x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(work.join("tmp"), std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(std::fs::remove_dir_all(&work).is_err(), "std alone cannot clear it: the test shows nothing");
        fresh_dir(&work).unwrap();
        assert!(std::fs::read_dir(&work).unwrap().next().is_none(), "an empty directory for the next check");
        assert_eq!(mode(&work), 0o700);
        remove_tree(&work).unwrap();
        assert!(!work.exists());
        remove_tree(&work).unwrap();
    }

    #[test]
    fn a_symlink_in_the_tree_is_removed_not_followed() {
        let d = Dir::new();
        let outside = d.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o751)).unwrap();
        let tree = d.path().join("tree");
        std::fs::create_dir_all(tree.join("a")).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("a/dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep"), tree.join("file")).unwrap();
        std::os::unix::fs::symlink(d.path().join("missing"), tree.join("dangling")).unwrap();
        std::fs::set_permissions(tree.join("a"), std::fs::Permissions::from_mode(0o0)).unwrap();
        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        assert_eq!(mode(&outside), 0o751, "the target's mode is untouched");
        assert!(outside.join("keep").exists(), "the target's contents are untouched");
        // A symlink as the root of the removal is unlinked, not followed either.
        std::os::unix::fs::symlink(&outside, &tree).unwrap();
        remove_tree(&tree).unwrap();
        assert!(std::fs::symlink_metadata(&tree).is_err() && outside.join("keep").exists());
        assert_eq!(mode(&outside), 0o751);
    }

    #[test]
    fn a_failure_that_cannot_be_fixed_names_its_path() {
        let d = Dir::new();
        // A mount point cannot be removed: stand in for it with a directory whose parent is read-only
        // and not ours to change: the tree's root is in `locked`, which `remove_tree` does not touch.
        let locked = d.path().join("locked");
        let tree = locked.join("tree");
        std::fs::create_dir_all(tree.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let denied = std::fs::create_dir(locked.join("probe")).is_err();
        let err = remove_tree(&tree);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !denied {
            return; // running as root: nothing is denied
        }
        let err = err.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains(&tree.display().to_string()), "{err}");
    }
}
