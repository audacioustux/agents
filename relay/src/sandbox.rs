//! What a session can see, as a declarative mount plan, and its bubblewrap
//! command line. The same for every harness.
//!
//! The machine is read-only underneath. On top, in layers (a later layer
//! wins; within a layer, parents come before children):
//!
//! | layer | what | mount |
//! | --- | --- | --- |
//! | 1 | `sandbox.hide` (default: the repository's siblings); the user's runtime dir and host sockets | hidden |
//! | 2 | the repository, and its git directories when it is a linked worktree | read-only |
//! | 3 | every home of every harness but the session's own; its own home | hidden; writable |
//! | 4 | what in its own home the host runs or obeys (settings, hooks, scripts) | read-only |
//! | 5 | `sandbox.caches`: relay's own directory for each, never the host's | writable |
//! | 6 | `sandbox.hide_home` (credential stores) | hidden |
//! | 7 | relay's own state and clones | hidden |
//! | 8 | the job's own clone and directory; its inbox (what relay writes for it) | writable; read-only |
//!
//! `/tmp` is not a layer: it is the job's scratch directory on disk (`Ctx::job_tmp`), bound there,
//! never a RAM tmpfs. A hidden directory is an empty tmpfs made read-only once everything inside it
//! has been mounted, so a write to it fails instead of filling RAM.
//!
//! A read-only mount does not stop a process connecting to a Unix socket in
//! it, so sockets are hidden, not just read-only. The network stays shared,
//! since the session needs its provider's API, and with it abstract sockets.

use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::ctx::Ctx;

#[derive(Debug)]
pub struct Mounts<'a> {
    pub home: &'a crate::harness::Home,
    pub workdir: Option<&'a Path>,
    /// The session's own directory (its session store), writable: relay writes nothing in it.
    pub scratch: &'a Path,
    /// What relay wrote for the session (its prompt file), read-only: a session that could write
    /// there could plant a symlink for relay's next write to follow.
    pub inbox: Option<&'a Path>,
    /// A directory on disk, bound at `/tmp` (and named by `TMPDIR`); never RAM.
    pub tmp: &'a Path,
    pub cwd: &'a Path,
    /// Whether the session's own account is mounted. A check run on its work is not the session: it
    /// gets no sign-in to read and nothing of the account to write.
    pub account: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    Hidden,
    ReadOnly,
    Writable,
    /// Writable, with this directory's contents instead of the host's.
    From(PathBuf),
}

/// The mounts, in the order bubblewrap must apply them.
pub fn plan(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>) -> Vec<(PathBuf, Mount)> {
    let cfg = &ctx.cfg;
    let own = crate::accounts::harness_of(m.home);
    let other_homes =
        crate::harness::ALL.iter().flat_map(|h| h.home_paths(user_home)).filter(|p| !m.account || *p != m.home.dir);
    let own_writable = if m.account { own.writable(m.home, user_home) } else { Vec::new() };
    let own_protected = if m.account { own.protected(m.home) } else { Vec::new() };
    let all = |paths: Vec<PathBuf>, mount: Mount| paths.into_iter().map(|p| (p, mount.clone())).collect::<Vec<_>>();
    let layers: [Vec<(PathBuf, Mount)>; 8] = [
        all(cfg.hidden_dirs().into_iter().chain([runtime_dir()]).chain(host_sockets()).collect(), Mount::Hidden),
        all([cfg.repo.clone()].into_iter().chain(ctx.git_dirs.iter().cloned()).collect(), Mount::ReadOnly),
        all(other_homes.collect(), Mount::Hidden).into_iter().chain(all(own_writable, Mount::Writable)).collect(),
        all(own_protected, Mount::ReadOnly),
        cfg.sandbox.caches.iter().map(|r| (user_home.join(r), Mount::From(ctx.caches.join(r)))).collect(),
        all(cfg.sandbox.hide_home.iter().map(|r| user_home.join(r)).collect(), Mount::Hidden),
        all(vec![ctx.state_root.clone(), ctx.data_root.clone()], Mount::Hidden),
        all(m.workdir.into_iter().chain([m.scratch]).map(Path::to_path_buf).collect(), Mount::Writable)
            .into_iter()
            .chain(all(m.inbox.map(Path::to_path_buf).into_iter().collect(), Mount::ReadOnly))
            .collect(),
    ];
    layers
        .into_iter()
        .flat_map(|mut layer| {
            layer.sort_by_key(|(p, _)| p.components().count());
            layer
        })
        .filter(|(p, _)| p.exists())
        .collect()
}

/// `$XDG_RUNTIME_DIR` (`/run/user/<uid>`): the user's D-Bus, systemd and agent sockets.
fn runtime_dir() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
}

/// Unix sockets under `/run` (outside the runtime dir, which is hidden whole),
/// such as a container engine's, a few levels deep.
fn host_sockets() -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_socket() {
                out.push(e.path());
            } else if t.is_dir() && depth > 0 && e.path() != runtime_dir() {
                walk(&e.path(), depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new("/run"), 4, &mut out);
    out
}

pub fn args(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>, env: Vec<(OsString, OsString)>) -> Vec<OsString> {
    let mut a: Vec<OsString> =
        ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--bind"].map(OsString::from).to_vec();
    a.extend([m.tmp.as_os_str().to_owned(), "/tmp".into()]);
    // A hidden directory is an empty tmpfs, made read-only once every mount has been made inside it:
    // a write fails loudly instead of filling RAM.
    // `/dev/shm` is a RAM tmpfs too, capped at `sandbox.shm_mb` (a write past it fails), or at 0 sealed
    // like a hidden directory.
    let shm_mb = ctx.cfg.sandbox.shm_mb;
    let mut sealed = Vec::new();
    if shm_mb == 0 {
        sealed.push(PathBuf::from("/dev/shm"));
    } else {
        a.extend(["--size".into(), (shm_mb << 20).to_string().into()]);
    }
    a.extend(["--tmpfs", "/dev/shm"].map(OsString::from));
    for (p, mount) in plan(ctx, user_home, m) {
        let (flag, src) = match &mount {
            Mount::Hidden if p.is_dir() => {
                sealed.push(p.clone());
                ("--tmpfs", None)
            }
            Mount::Hidden => ("--ro-bind", Some(Path::new("/dev/null"))),
            Mount::ReadOnly => ("--ro-bind", Some(p.as_path())),
            Mount::Writable => ("--bind", Some(p.as_path())),
            Mount::From(src) => {
                let _ = std::fs::create_dir_all(src);
                ("--bind", Some(src.as_path()))
            }
        };
        a.push(flag.into());
        a.extend(src.map(OsString::from));
        a.push(p.into());
    }
    for p in sealed {
        a.extend(["--remount-ro".into(), p.into()]);
    }
    a.extend(
        ["--unshare-pid", "--unshare-ipc", "--unshare-uts", "--die-with-parent", "--new-session", "--clearenv"]
            .map(OsString::from),
    );
    for (k, v) in env.into_iter().chain([("TMPDIR".into(), "/tmp".into())]) {
        a.extend(["--setenv".into(), k, v]);
    }
    a.extend(["--chdir".into(), m.cwd.into()]);
    a
}

/// The environment a session gets: the caller's minus names matching
/// `sandbox.env_deny`, then the invocation's own variables (which may name
/// what the filter removes, such as `CLAUDE_CONFIG_DIR`).
pub fn session_env(ctx: &Ctx, inv: &crate::harness::Invocation) -> Vec<(OsString, OsString)> {
    let deny = Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| {
            let name = k.to_string_lossy();
            !deny.is_match(&name) && !matches!(name.as_ref(), "TMPDIR" | "TEMP" | "TMP" | "PWD" | "OLDPWD")
        })
        .collect();
    // Signing keys are hidden, so git inside cannot sign: say so up front
    // rather than let every session find out by a failed commit.
    let git = [
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "tag.gpgsign"),
        ("GIT_CONFIG_VALUE_1", "false"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    env.retain(|(name, _)| !name.to_string_lossy().starts_with("GIT_CONFIG_"));
    env.extend(git);
    for (k, v) in &inv.env {
        env.retain(|(name, _)| name != k);
        env.extend(v.clone().map(|v| (k.clone(), v)));
    }
    env
}
