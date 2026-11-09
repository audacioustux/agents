//! The one way relay's tests run a real CLI (claude, omp, puku-cli, node, bun): where it can reach
//! no real sign-in. puku renews an expired sign-in on every start but `--version`, and the others
//! read or rewrite theirs under `HOME` or a config dir, so a check that only looks at `--help` could
//! still refresh, or spend, the developer's own credentials.
//!
//! Included by `tests/ask.rs` and by the binary's own tests (`#[path]`), so both use this one.
#![allow(dead_code, reason = "each crate that includes it uses part of it")]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway home, removed when dropped, and the environment a real CLI runs in there.
#[derive(Debug)]
pub struct Isolated {
    root: PathBuf,
    owned: bool,
}

impl Isolated {
    /// A new, empty home in the temporary directory.
    pub fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("relay-isolated-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a throwaway home");
        Self { root, owned: true }
    }

    /// `dir` (a test's own temporary directory, which it removes) as the home.
    pub fn at(dir: &Path) -> Self {
        Self { root: dir.to_path_buf(), owned: false }
    }

    pub fn home(&self) -> &Path {
        &self.root
    }

    /// `program` with nothing of the caller's environment but `PATH` (how the CLI and its `#!`
    /// interpreter are found), so no credential variable (`ANTHROPIC_API_KEY`, `PUKU_AUTH_TOKEN`)
    /// gets through, and with every variable that picks a home or config dir pointed inside the
    /// throwaway one. A test may add variables of its own, inside that home too.
    #[allow(clippy::disallowed_methods, reason = "a test's CLI is found on the PATH it was started with")]
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let home = &self.root;
        let mut c = Command::new(program);
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("TMPDIR", std::env::temp_dir())
            .env("LANG", "C.UTF-8")
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("PUKU_CONFIG_DIR", home.join(".puku-cli"))
            .env("APP_NAME", "relay-isolated")
            .current_dir(home);
        c
    }

    /// Where `name` is on the test's `PATH`, skipping mise shims: a shim picks its tool from mise's
    /// config and data under `HOME`, which this home does not have, and can stall resolving it. An
    /// absolute `name` is taken as it is.
    #[allow(clippy::disallowed_methods, reason = "a test's CLI is found on the PATH it was started with")]
    pub fn find(name: &str) -> Option<PathBuf> {
        let shim = |p: &Path| {
            let mise = |p: &Path| p.file_name().is_some_and(|n| n == "mise");
            p.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
                && std::fs::canonicalize(p).is_ok_and(|t| mise(&t))
        };
        let runnable = |p: &Path| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) && !shim(p)
        };
        if Path::new(name).is_absolute() {
            return runnable(Path::new(name)).then(|| PathBuf::from(name));
        }
        std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| runnable(p))
    }

    /// `name args` in this home, if `name` is an installed CLI that runs: the one probe of a real
    /// CLI, returning where it is and what it printed. `Err` says why there is none to ask, and the
    /// caller skips: not on `PATH` (past mise shims), does not start, no answer within 20 s (it is
    /// killed: a launcher whose `#!` interpreter is only a shim can stall), or a non-zero exit with
    /// nothing on stdout (a shim whose runtime is gone), whose help would say nothing of its flags.
    #[allow(
        clippy::disallowed_methods,
        reason = "a test's own bounded probe, waited for here and killed at its deadline; the integration \
                  tests cannot reach the binary's supervisor"
    )]
    pub fn ask(&self, name: &str, args: &[&str]) -> Result<(PathBuf, String), String> {
        use std::process::Stdio;
        let program = Self::find(name).ok_or_else(|| format!("{name} is not installed (a mise shim is not one)"))?;
        let mut child = self
            .command(&program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{name} does not start: {e}"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while child.try_wait().map_err(|e| format!("{name}: {e}"))?.is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} did not answer within 20 s"));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = child.wait_with_output().map_err(|e| format!("{name}: {e}"))?;
        if !out.status.success() && out.stdout.is_empty() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(format!("{name} is on PATH but does not run: {why}"));
        }
        Ok((program, String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
