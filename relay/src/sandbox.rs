//! The bubblewrap command line a session runs under.
//!
//! The session sees the machine read-only, minus the configured hidden
//! directories (by default the repository's siblings), every other account,
//! relay's own state and other jobs' clones, and credential stores. It may
//! write only its own clone, its own account dir and the configured caches.
//! The network stays shared, since the session needs the API.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::ctx::Ctx;

#[derive(Debug)]
pub struct Mounts<'a> {
    pub account_dir: &'a Path,
    pub workdir: Option<&'a Path>,
    pub cwd: &'a Path,
}

fn hide(p: &Path, args: &mut Vec<OsString>) {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => args.extend(["--tmpfs".into(), p.into()]),
        Ok(_) => args.extend(["--ro-bind".into(), "/dev/null".into(), p.into()]),
        Err(_) => {}
    }
}

fn bind(flag: &str, p: &Path, args: &mut Vec<OsString>) {
    args.extend([flag.into(), p.into(), p.into()]);
}

pub fn args(
    ctx: &Ctx,
    home: &Path,
    m: &Mounts<'_>,
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<OsString> {
    let cfg = &ctx.cfg;
    let mut a: Vec<OsString> =
        ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"].map(OsString::from).to_vec();
    for d in cfg.hidden_dirs() {
        hide(&d, &mut a);
    }
    // Back on top of the hidden dirs, in this order: an inner mount must come after its parent.
    bind("--ro-bind", &cfg.repo, &mut a);
    for root in [&ctx.state_root, &ctx.data_root] {
        hide(root, &mut a);
    }
    if let Some(w) = m.workdir {
        bind("--bind", w, &mut a);
    }
    bind("--bind", m.account_dir, &mut a);
    for rel in &cfg.sandbox.writable_home {
        let p = home.join(rel);
        if p.exists() {
            bind("--bind", &p, &mut a);
        }
    }
    // Hides come after the binds so they also cover secrets inside a bound cache.
    for p in crate::accounts::account_like_entries(home, &cfg.accounts.prefix) {
        if p != m.account_dir {
            hide(&p, &mut a);
        }
    }
    for rel in &cfg.sandbox.hide_home {
        hide(&home.join(rel), &mut a);
    }
    a.extend(
        ["--unshare-pid", "--unshare-ipc", "--unshare-uts", "--die-with-parent", "--new-session", "--clearenv"]
            .map(OsString::from),
    );
    let deny = Regex::new(&cfg.sandbox.env_deny).expect("validated at load");
    for (k, v) in env {
        let name = k.to_string_lossy();
        if deny.is_match(&name) || matches!(name.as_ref(), "TMPDIR" | "TEMP" | "TMP" | "PWD" | "OLDPWD") {
            continue;
        }
        a.extend(["--setenv".into(), k, v]);
    }
    let set = |a: &mut Vec<OsString>, k: &str, v: OsString| a.extend(["--setenv".into(), k.into(), v]);
    set(&mut a, "CLAUDE_CONFIG_DIR", m.account_dir.into());
    set(&mut a, "DISABLE_AUTOUPDATER", "1".into());
    set(&mut a, "TMPDIR", "/tmp".into());
    a.extend(["--chdir".into(), m.cwd.into()]);
    a
}

/// The environment outside the sandbox, for when it is turned off: the
/// account dir is still set, and the same names are still kept out.
pub fn plain_env(ctx: &Ctx, account_dir: &Path) -> Vec<(OsString, OsString)> {
    let deny = Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    std::env::vars_os()
        .filter(|(k, _)| !deny.is_match(&k.to_string_lossy()))
        .chain([
            ("CLAUDE_CONFIG_DIR".into(), PathBuf::from(account_dir).into()),
            ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ])
        .collect()
}
