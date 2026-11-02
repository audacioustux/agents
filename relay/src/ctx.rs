//! Paths and helpers every system shares.

use std::fs::{DirBuilder, File, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result};

use crate::bus::{self, Owned, Port, Stamped};
use crate::config::Config;
use crate::os::Role;
use crate::world::World;

#[derive(Debug)]
pub struct Ctx {
    pub cfg: Config,
    pub cfg_path: PathBuf,
    /// `$XDG_STATE_HOME/relay/<name>`: the bus, job records, locks.
    pub state: PathBuf,
    pub bus: PathBuf,
    pub jobs: PathBuf,
    /// `$XDG_DATA_HOME/relay/<name>/clones`: one private clone per build job.
    pub clones: PathBuf,
    /// `$XDG_DATA_HOME/relay/<name>/caches`: the sessions' own `sandbox.caches`.
    pub caches: PathBuf,
    /// Everything relay keeps for every project, hidden from sessions.
    pub state_root: PathBuf,
    pub data_root: PathBuf,
    /// When the repository is a linked worktree, its git directory and the
    /// main repository's, which its `.git` file and a clone's alternates name.
    pub git_dirs: Vec<PathBuf>,
}

/// A linked worktree's git directories outside it (none for an ordinary checkout).
fn git_dirs(repo: &Path) -> Vec<PathBuf> {
    let out = Ctx::git(repo).args(["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"]).output();
    let text = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let mut dirs: Vec<PathBuf> =
        text.unwrap_or_default().lines().map(PathBuf::from).filter(|d| !d.starts_with(repo)).collect();
    dirs.dedup();
    dirs
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::accounts::home().join(fallback))
}

impl Ctx {
    pub fn new(cfg: Config, cfg_path: PathBuf) -> Result<Self> {
        let state_root = xdg("XDG_STATE_HOME", ".local/state").join("relay");
        let data_root = xdg("XDG_DATA_HOME", ".local/share").join("relay");
        let state = state_root.join(&cfg.name);
        let data = data_root.join(&cfg.name);
        let ctx = Self {
            bus: state.join("bus"),
            jobs: state.join("jobs"),
            clones: data.join("clones"),
            caches: data.join("caches"),
            git_dirs: git_dirs(&cfg.repo),
            state: state.clone(),
            state_root,
            data_root,
            cfg,
            cfg_path,
        };
        // Briefs, answers and resume messages are private: owner-only from the project directory down.
        for d in [&state, &data] {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(d)
                .with_context(|| format!("creating {}", d.display()))?;
            std::fs::set_permissions(d, Permissions::from_mode(0o700))?;
        }
        for d in [&ctx.bus, &ctx.jobs] {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(d)
                .with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(ctx)
    }

    /// Held while a command checks a job's state and records a change to it,
    /// so two commands cannot both act on one snapshot (two adds of one name,
    /// a clean racing a resume, a dispatch racing either).
    pub fn jobs_lock(&self) -> Result<Role> {
        Role::take(&self.state.join("jobs.lock"))
    }

    pub fn port<E: Owned>(&self) -> Port<E> {
        Port::new(&self.bus)
    }

    pub fn events(&self) -> Result<Vec<Stamped>> {
        bus::read_all(&self.bus)
    }

    pub fn world(&self) -> Result<World> {
        Ok(World::fold(&self.events()?))
    }

    pub fn job_file(&self, job: &str, ext: &str) -> PathBuf {
        self.jobs.join(format!("{job}.{ext}"))
    }

    /// The job's private directory: its prompt file and, for harnesses that
    /// keep sessions with the task, its session store. Mounted writable in the sandbox.
    pub fn job_scratch(&self, job: &str) -> PathBuf {
        self.jobs.join(format!("{job}.d"))
    }

    /// Opens a log for appending, readable by its owner only.
    pub fn append_private(path: &Path) -> Result<File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))
    }

    /// This binary, re-run with the same config, as a detached background
    /// process in its own process group, so a closing terminal does not take it.
    pub fn spawn_self(&self, args: &[&str], log: &Path) -> Result<u32> {
        let child = self.spawn(args, log, Stdio::null())?;
        let pid = child.id();
        Self::reap(child);
        Ok(pid)
    }

    /// As `spawn_self`, held at a gate: the child's stdin, which it reads to
    /// its end (`wait_for_gate`) before it starts. Dropping the gate opens it,
    /// so the spawner can first record what the child will need, its pid included.
    pub fn spawn_gated(&self, args: &[&str], log: &Path) -> Result<(u32, ChildStdin)> {
        let mut child = self.spawn(args, log, Stdio::piped())?;
        let gate = child.stdin.take().context("the gate pipe")?;
        let pid = child.id();
        Self::reap(child);
        Ok((pid, gate))
    }

    fn spawn(&self, args: &[&str], log: &Path, stdin: Stdio) -> Result<Child> {
        let out = Self::append_private(log)?;
        Command::new(std::env::current_exe()?)
            .arg("--config")
            .arg(&self.cfg_path)
            .args(args)
            .stdin(stdin)
            .stdout(out.try_clone()?)
            .stderr(out)
            .current_dir(&self.cfg.repo)
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting relay {}", args.join(" ")))
    }

    /// Reaps `child` when it exits, so a long-lived dispatcher leaves no zombies.
    fn reap(mut child: Child) {
        std::thread::spawn(move || child.wait());
    }

    /// Makes sure the dispatcher is awake. If one already runs, the new one
    /// fails to take the lock and exits; the running one hears the change
    /// through its inotify watch.
    pub fn wait_for_gate() {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    }

    pub fn wake_dispatch(&self) -> Result<()> {
        self.spawn_self(&["dispatch", "serve"], &self.state.join("dispatch.log")).map(drop)
    }

    pub fn git(dir: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c
    }
}

use std::os::unix::process::CommandExt;
