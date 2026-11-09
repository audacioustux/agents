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
//! | 3 | every home of every harness but the session's own; its own home; what in it the host runs or obeys (settings, hooks, scripts) | hidden; writable; read-only |
//! | 4 | `sandbox.caches`: relay's own directory for each, never the host's | writable |
//! | 5 | `sandbox.hide_home` (credential stores) | hidden |
//! | 6 | relay's own state and clones | hidden |
//! | 7 | the job's own clone and directory; its inbox (what relay writes for it) | writable; read-only |
//!
//! Layer 3 is one layer, ordered by depth, so a protected directory that holds other homes (omp's
//! `profiles`) is mounted before they are hidden inside it.
//!
//! A path that does not exist cannot be mounted, and is left out, unless a writable mount above it
//! would let the session create it: then a path the plan keeps from the session (protected config
//! that is not there yet, such as a `settings.json` or a `hooks/` the account never had) gets a
//! [`Mount::Placeholder`], made empty on the host and mounted read-only, so the session cannot be the
//! one to create it.
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
    /// A hidden or read-only path that is not there, under a writable mount: made empty first (a
    /// directory, or a file; see [`seed`]), then mounted read-only, so the session cannot create it.
    Placeholder {
        dir: bool,
    },
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
    let layers: [Vec<(PathBuf, Mount)>; 7] = [
        all(cfg.hidden_dirs().into_iter().chain([runtime_dir()]).chain(host_sockets()).collect(), Mount::Hidden),
        all([cfg.repo.clone()].into_iter().chain(ctx.git_dirs.iter().cloned()).collect(), Mount::ReadOnly),
        all(other_homes.collect(), Mount::Hidden)
            .into_iter()
            .chain(all(own_writable, Mount::Writable))
            .chain(all(own_protected, Mount::ReadOnly))
            .collect(),
        cfg.sandbox.caches.iter().map(|r| (user_home.join(r), Mount::From(ctx.caches.join(r)))).collect(),
        all(cfg.sandbox.hide_home.iter().map(|r| user_home.join(r)).collect(), Mount::Hidden),
        all(vec![ctx.state_root.clone(), ctx.data_root.clone()], Mount::Hidden),
        all(m.workdir.into_iter().chain([m.scratch]).map(Path::to_path_buf).collect(), Mount::Writable)
            .into_iter()
            .chain(all(m.inbox.map(Path::to_path_buf).into_iter().collect(), Mount::ReadOnly))
            .collect(),
    ];
    let mut out: Vec<(PathBuf, Mount)> = Vec::new();
    for (p, mount) in layers.into_iter().flat_map(|mut layer| {
        layer.sort_by_key(|(p, _)| p.components().count());
        layer
    }) {
        if p.exists() {
            out.push((p, mount));
            continue;
        }
        let kept_from_session = matches!(mount, Mount::Hidden | Mount::ReadOnly);
        // Under relay's own cache directory (`From`), what the session creates is not the host's.
        let creatable = out.iter().any(|(w, k)| *k == Mount::Writable && p.starts_with(w));
        if kept_from_session && creatable {
            // `settings.json`, `CLAUDE.md`, `config.yml` are files; `hooks`, `profiles` directories.
            out.push((p.clone(), Mount::Placeholder { dir: p.extension().is_none() }));
        }
    }
    out
}

/// Makes the placeholder `p` exist on the host, empty: a directory, or a file (`{}` for JSON, so the
/// owner's harness finds valid, empty settings there later). Only through its parent opened without
/// following a symlink, and only where nothing is (`mkdir`, `O_EXCL | O_NOFOLLOW`).
fn seed(p: &Path, dir: bool) -> std::io::Result<()> {
    use rustix::fs::{Mode, OFlags};
    let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let at = rustix::fs::open(parent, flags, Mode::empty())?;
    if dir {
        rustix::fs::mkdirat(&at, name, Mode::from_raw_mode(0o755))?;
    } else {
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = rustix::fs::openat(&at, name, flags, Mode::from_raw_mode(0o644))?;
        if p.extension().is_some_and(|x| x == "json") {
            std::io::Write::write_all(&mut std::fs::File::from(file), b"{}\n")?;
        }
    }
    Ok(())
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
            // Made empty on the host, then mounted read-only like any protected path. Should that fail
            // (its parent is missing, or not a real directory), bubblewrap's own empty, read-only stand-in.
            Mount::Placeholder { dir } => match seed(&p, *dir) {
                Ok(()) => ("--ro-bind", Some(p.as_path())),
                Err(_) if p.exists() => ("--ro-bind", Some(p.as_path())),
                Err(_) if *dir => {
                    sealed.push(p.clone());
                    ("--tmpfs", None)
                }
                Err(_) => ("--ro-bind", Some(Path::new("/dev/null"))),
            },
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

/// The environment a session gets: [`Ctx::job_env`] (built from the config, never
/// the caller's) minus names matching `sandbox.env_deny`, then the invocation's own variables
/// (which may name what the filter removes, such as `CLAUDE_CONFIG_DIR`).
pub fn session_env(ctx: &Ctx, inv: &crate::harness::Invocation) -> Vec<(OsString, OsString)> {
    let deny = Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    let mut env: Vec<(OsString, OsString)> = ctx
        .job_env()
        .without(&deny)
        .vars()
        .filter(|(k, _)| !matches!(k.to_string_lossy().as_ref(), "TMPDIR" | "TEMP" | "TMP" | "PWD" | "OLDPWD"))
        .map(|(k, v)| (k.clone(), v.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, tempdir::Dir};
    use crate::harness::Home;

    fn ctx(root: &Path) -> Ctx {
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = Config::parse("name = \"t\"\n[sandbox]\nhide = []\ncaches = []\nhide_home = []\n", &repo).unwrap();
        Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"))
    }

    fn plan_for(root: &Path, home: &Home) -> Vec<(PathBuf, Mount)> {
        let ctx = ctx(root);
        let (scratch, tmp) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"));
        let m = Mounts {
            home,
            workdir: None,
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &ctx.cfg.repo,
            account: true,
        };
        plan(&ctx, &root.join("user"), &m)
    }

    fn mount_of<'p>(plan: &'p [(PathBuf, Mount)], p: &Path) -> Option<&'p Mount> {
        plan.iter().find(|(q, _)| q == p).map(|(_, m)| m)
    }

    /// Audit finding 4: protected config the account does not have yet was left out of the plan, so the
    /// session could create it (a `settings.json` with a hook the owner's next claude runs).
    #[test]
    fn protected_config_that_is_not_there_gets_a_placeholder() {
        let d = Dir::new();
        let dir = d.path().join("user/.claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "mine").unwrap();
        let home = Home { harness: "claude", label: "default".into(), dir: dir.clone() };
        let plan = plan_for(d.path(), &home);
        assert_eq!(mount_of(&plan, &dir.join("settings.json")), Some(&Mount::Placeholder { dir: false }), "{plan:?}");
        assert_eq!(mount_of(&plan, &dir.join("hooks")), Some(&Mount::Placeholder { dir: true }));
        assert_eq!(
            mount_of(&plan, &dir.join("CLAUDE.md")),
            Some(&Mount::ReadOnly),
            "what is there is mounted as before"
        );
        let at = |p: &Path| plan.iter().position(|(q, _)| q == p).unwrap();
        assert!(at(&dir) < at(&dir.join("hooks")), "the writable home comes first, its placeholders on top");
        // Nothing is created by planning; a path under no writable mount gets no placeholder at all.
        assert!(!dir.join("settings.json").exists());
        assert!(mount_of(&plan, &d.path().join("user/.ssh")).is_none());
    }

    /// The default omp profile is `~/.omp`, which holds the others: its session may not make a new
    /// profile (relay would discover it as an account), and the protected `profiles` must not show the
    /// other profiles again, which are hidden inside it.
    #[test]
    fn omp_profiles_are_protected_and_still_hidden() {
        let d = Dir::new();
        let root = d.path().join("user/.omp");
        std::fs::create_dir_all(root.join("agent")).unwrap();
        let home = Home { harness: "omp", label: "default".into(), dir: root.clone() };
        let plan = plan_for(d.path(), &home);
        assert_eq!(mount_of(&plan, &root.join("profiles")), Some(&Mount::Placeholder { dir: true }), "{plan:?}");
        std::fs::create_dir_all(root.join("profiles/work")).unwrap();
        let plan = plan_for(d.path(), &home);
        let at = |p: &Path| plan.iter().position(|(q, _)| q == p).unwrap();
        assert_eq!(mount_of(&plan, &root.join("profiles")), Some(&Mount::ReadOnly));
        assert_eq!(mount_of(&plan, &root.join("profiles/work")), Some(&Mount::Hidden));
        assert!(at(&root.join("profiles")) < at(&root.join("profiles/work")), "hidden after, so it stays hidden");
    }

    #[test]
    fn a_placeholder_is_seeded_empty_and_never_through_a_symlink() {
        let d = Dir::new();
        let home = d.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        seed(&home.join("settings.json"), false).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("settings.json")).unwrap(), "{}\n");
        seed(&home.join("CLAUDE.md"), false).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("CLAUDE.md")).unwrap(), "");
        seed(&home.join("hooks"), true).unwrap();
        assert!(home.join("hooks").is_dir());
        // Something already there (a planted symlink included) is left alone, and nothing is followed.
        let elsewhere = d.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, home.join("settings.local.json")).unwrap();
        assert!(seed(&home.join("settings.local.json"), false).is_err());
        assert!(!elsewhere.exists());
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join("agent")).unwrap();
        assert!(seed(&home.join("agent/config.yml"), false).is_err(), "a symlinked parent is not followed");
        assert!(!elsewhere.join("config.yml").exists());
    }
}
