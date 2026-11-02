//! Paths and helpers every system shares.

use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::bus::{self, Owned, Port, Stamped};
use crate::config::Config;
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
    /// Everything relay keeps for every project, hidden from sessions.
    pub state_root: PathBuf,
    pub data_root: PathBuf,
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
        let ctx = Self {
            bus: state.join("bus"),
            jobs: state.join("jobs"),
            clones: data_root.join(&cfg.name).join("clones"),
            state,
            state_root,
            data_root,
            cfg,
            cfg_path,
        };
        for d in [&ctx.bus, &ctx.jobs] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(ctx)
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
        let exe = std::env::current_exe()?;
        let out = Self::append_private(log)?;
        let child = std::process::Command::new(exe)
            .arg("--config")
            .arg(&self.cfg_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .current_dir(&self.cfg.repo)
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting relay {}", args.join(" ")))?;
        Ok(child.id())
    }

    /// Makes sure the dispatcher is awake. If one already runs, the new one
    /// fails to take the lock and exits; the running one hears the change
    /// through its inotify watch.
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
