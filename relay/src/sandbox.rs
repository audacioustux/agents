//! What a session can see, as a declarative mount plan, and its bubblewrap
//! command line. The same for every harness.
//!
//! The machine is read-only underneath. On top, in layers (a later layer
//! wins; within a layer, parents come before children):
//!
//! | layer | what | mount |
//! | --- | --- | --- |
//! | 1 | `sandbox.hide` (default: the repository's siblings) | hidden |
//! | 2 | the repository | read-only |
//! | 3 | every home of every harness but the session's own; its own home | hidden; writable |
//! | 4 | `sandbox.writable_home` (build caches) | writable |
//! | 5 | `sandbox.hide_home` (credential stores) | hidden |
//! | 6 | relay's own state and clones | hidden |
//! | 7 | the job's own clone and directory | writable |
//!
//! The network stays shared, since the session needs its provider's API.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::ctx::Ctx;

#[derive(Debug)]
pub struct Mounts<'a> {
    pub home: &'a crate::harness::Home,
    pub workdir: Option<&'a Path>,
    /// The job's private directory (prompt file, session store).
    pub scratch: &'a Path,
    pub cwd: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    Hidden,
    ReadOnly,
    Writable,
}

/// The mounts, in the order bubblewrap must apply them.
pub fn plan(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>) -> Vec<(PathBuf, Mount)> {
    let cfg = &ctx.cfg;
    let own = crate::accounts::harness_of(m.home);
    let other_homes = crate::harness::ALL.iter().flat_map(|h| h.home_paths(user_home)).filter(|p| *p != m.home.dir);
    let in_home = |rels: &[PathBuf], mount| rels.iter().map(|r| (user_home.join(r), mount)).collect::<Vec<_>>();
    let layers: [Vec<(PathBuf, Mount)>; 7] = [
        cfg.hidden_dirs().into_iter().map(|d| (d, Mount::Hidden)).collect(),
        vec![(cfg.repo.clone(), Mount::ReadOnly)],
        other_homes
            .map(|p| (p, Mount::Hidden))
            .chain(own.writable(m.home, user_home).into_iter().map(|p| (p, Mount::Writable)))
            .collect(),
        in_home(&cfg.sandbox.writable_home, Mount::Writable),
        in_home(&cfg.sandbox.hide_home, Mount::Hidden),
        vec![(ctx.state_root.clone(), Mount::Hidden), (ctx.data_root.clone(), Mount::Hidden)],
        m.workdir.into_iter().chain([m.scratch]).map(|p| (p.to_path_buf(), Mount::Writable)).collect(),
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

pub fn args(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>, env: Vec<(OsString, OsString)>) -> Vec<OsString> {
    let mut a: Vec<OsString> =
        ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"].map(OsString::from).to_vec();
    for (p, mount) in plan(ctx, user_home, m) {
        let p = p.as_os_str();
        let flags: Vec<&OsStr> = match mount {
            Mount::Hidden if Path::new(p).is_dir() => vec!["--tmpfs".as_ref(), p],
            Mount::Hidden => vec!["--ro-bind".as_ref(), "/dev/null".as_ref(), p],
            Mount::ReadOnly => vec!["--ro-bind".as_ref(), p, p],
            Mount::Writable => vec!["--bind".as_ref(), p, p],
        };
        a.extend(flags.into_iter().map(OsString::from));
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
    for (k, v) in &inv.env {
        env.retain(|(name, _)| name != k);
        env.extend(v.clone().map(|v| (k.clone(), v)));
    }
    env
}
