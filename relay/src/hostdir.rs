//! A directory only the host writes, held open: where relay puts the files it hands a session (a
//! prompt file) or keeps beside one (a check's output, a scratch git index).
//!
//! A session's own directories are the session's: anything relay wrote there by path could be a
//! symlink the session planted, and the write would land wherever it points (`~/.bashrc`). So
//! host-written files never go in a directory a session can write. They go in a [`HostDir`], which no
//! sandbox mounts writable (a session sees it, at most, read-only), opened once without following a
//! symlink and written only through that handle: each file by a bare name, created afresh with
//! `O_CREAT | O_EXCL | O_NOFOLLOW`, so a stale or planted entry is replaced, never followed.

use std::fs::{DirBuilder, File};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, CWD, Mode, OFlags};
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

/// Makes `path` an empty directory, owner-only, whatever was there: a tree is removed without
/// following a symlink in it, and the new directory is made with `mkdir`, which does not follow
/// one either. For a session's (or a check's) working directory made fresh for each run, in a place
/// only relay writes.
pub fn fresh_dir(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
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
}
