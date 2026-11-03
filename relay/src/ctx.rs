//! Paths and helpers every system shares.

use std::fs::{DirBuilder, File, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result};

use crate::bus::{self, Owned, Port, Stamped};
use crate::config::Config;
use crate::os::{Doorbell, Role};
use crate::world::World;

#[derive(Debug)]
pub struct Ctx {
    pub cfg: Config,
    /// `--json`: commands answer with one JSON value.
    pub json: bool,
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
    pub fn new(cfg: Config, cfg_path: PathBuf, json: bool) -> Result<Self> {
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
            json,
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
        for d in [&ctx.bus, &ctx.bus.join("session"), &ctx.jobs] {
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

    /// Answers the caller: `value` with `--json`, else `text`.
    pub fn reply(&self, value: &serde_json::Value, text: &str) -> Result<()> {
        crate::reply::reply(self.json, value, text)
    }

    pub fn port<E: Owned>(&self, source: &E::Source) -> Port<E> {
        Port::new(&self.bus, source)
    }

    pub fn events(&self) -> Result<Vec<Stamped>> {
        bus::read_all(&self.bus)
    }

    pub fn world(&self) -> Result<World> {
        World::load(&self.bus, &self.state.join("world.json"))
    }

    /// The other projects on this machine, as their own folds say: every
    /// directory under `state_root` with a bus, other than ours. Their
    /// checkpoints are theirs, so they are read, never written; one that
    /// cannot be read is left out.
    pub fn neighbours(&self) -> Vec<World> {
        self.projects()
            .filter(|d| *d != self.state)
            .filter_map(|d| World::peek(&d.join("bus"), &d.join("world.json")).ok())
            .collect()
    }

    /// Every project's state directory on this machine, ours included.
    fn projects(&self) -> impl Iterator<Item = PathBuf> {
        std::fs::read_dir(&self.state_root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|d| d.join("bus").is_dir())
    }

    /// Every project's event directories, for a doorbell.
    pub fn buses(&self) -> Vec<PathBuf> {
        self.projects().flat_map(|d| [d.join("bus"), d.join("bus/session")]).filter(|d| d.is_dir()).collect()
    }

    /// Rings on any project's events.
    pub fn doorbell(&self) -> Result<Doorbell> {
        Doorbell::watch(&self.buses())
    }

    pub fn job_file(&self, job: &str, ext: &str) -> PathBuf {
        self.jobs.join(format!("{job}.{ext}"))
    }

    /// A job's stream log (`log.ndjson`, `check.ndjson`) where it is now: the
    /// plain file while it is written, else the gzip gc made of it.
    pub fn job_log(&self, job: &str, ext: &str) -> PathBuf {
        let plain = self.job_file(job, ext);
        let gz = self.job_file(job, &format!("{ext}.gz"));
        if !plain.exists() && gz.exists() { gz } else { plain }
    }

    /// A job's log as text, whether gc gzipped (all or part of) it or not: the
    /// compressed part first, then what was written since.
    pub fn read_job_log(&self, job: &str, ext: &str) -> String {
        let mut text = Vec::new();
        let gz = self.job_file(job, &format!("{ext}.gz"));
        if gz.exists() {
            let out = Command::new("gzip").arg("-dc").arg(&gz).stderr(Stdio::null()).output();
            text.extend(out.ok().filter(|o| o.status.success()).map(|o| o.stdout).unwrap_or_default());
        }
        text.extend(std::fs::read(self.job_file(job, ext)).unwrap_or_default());
        String::from_utf8_lossy(&text).into_owned()
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
        self.reap(child);
        Ok(pid)
    }

    /// As `spawn_self`, held at a gate: the child's stdin, which it reads to
    /// its end (`wait_for_gate`) before it starts. Dropping the gate opens it,
    /// so the spawner can first record what the child will need, its pid included.
    pub fn spawn_gated(&self, args: &[&str], log: &Path) -> Result<(u32, ChildStdin)> {
        let mut child = self.spawn(args, log, Stdio::piped())?;
        let gate = child.stdin.take().context("the gate pipe")?;
        let pid = child.id();
        self.reap(child);
        Ok((pid, gate))
    }

    fn spawn(&self, args: &[&str], log: &Path, stdin: Stdio) -> Result<Child> {
        let out = Self::append_private(log)?;
        Command::new(crate::os::own_binary()?)
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

    /// Reaps `child` when it exits, so a long-lived dispatcher leaves no
    /// zombies, and rings the bus doorbell: a runner killed outright reports
    /// nothing, so its exit is the only news that it has gone.
    fn reap(&self, mut child: Child) {
        let bell = self.bus.join(".exited");
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = std::fs::write(bell, "");
        });
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

    /// The repository's `HEAD` commit when its working tree is clean: then
    /// the commit names exactly what a read-only session reads.
    pub fn clean_head(repo: &Path) -> Option<String> {
        let out = |args: &[&str]| {
            let o = Self::git(repo).args(args).stderr(Stdio::null()).output().ok()?;
            o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        };
        out(&["status", "--porcelain"])?.is_empty().then(|| out(&["rev-parse", "HEAD"])).flatten()
    }

    pub fn git(dir: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c
    }

    /// As [`Ctx::git`], for a repository a session could write: its own configuration cannot name a
    /// program for git to run on the host through a file system monitor, hooks, or a filter driver
    /// (`git add` runs one for a `.gitattributes` line). Every `filter.*` key in effect is emptied, the
    /// user's global configuration included (so git-lfs does not run either). Diff drivers
    /// (`diff.external`, textconv) are not neutralised here: a caller that shows a diff passes
    /// `--no-ext-diff --no-textconv`.
    pub fn git_untrusted(dir: &Path) -> Command {
        let mut c = Self::git(dir);
        c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"]).env("GIT_CONFIG_NOSYSTEM", "1");
        // Every filter the repository's configuration defines is emptied (an empty command is no filter),
        // whatever its name; reading configuration runs nothing.
        let names = Self::git(dir)
            .args(["config", "--name-only", "--null", "--get-regexp", r"^filter\..*\.(clean|smudge|process|required)$"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        for key in names.split('\0').filter(|k| !k.is_empty()) {
            let off = if key.ends_with(".required") { "false" } else { "" };
            c.arg("-c").arg(format!("{key}={off}"));
        }
        c
    }
}

use std::os::unix::process::CommandExt;

#[cfg(test)]
mod git_tests {
    use super::*;

    /// A repository whose own configuration names a filter driver and a hook, and whose attributes use the filter.
    #[test]
    fn git_untrusted_runs_neither_filters_nor_hooks_of_the_repository() {
        let d = crate::config::tempdir::Dir::new();
        let (dir, marker) = (d.path().to_owned(), d.path().join("ran"));
        let run = |mut c: Command| {
            let o = c.env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        run({
            let mut c = Command::new("git");
            c.arg("-C").arg(&dir).args(["init", "-q"]);
            c
        });
        let touch = format!("touch {}; cat", marker.display());
        for (k, v) in [("filter.evil.clean", touch.as_str()), ("filter.evil.required", "true")] {
            run({
                let mut c = Ctx::git(&dir);
                c.args(["config", k, v]);
                c
            });
        }
        std::fs::write(dir.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        // Plain git runs the filter (so the test would notice a repository that stopped naming one) ...
        run({
            let mut c = Ctx::git(&dir);
            c.args(["add", "-A"]);
            c
        });
        assert!(marker.exists(), "the fixture's filter is not in effect");
        std::fs::remove_file(&marker).unwrap();
        run({
            let mut c = Ctx::git(&dir);
            c.args(["reset", "-q"]);
            c
        });
        // ... the untrusted one does not, and still adds the file.
        run({
            let mut c = Ctx::git_untrusted(&dir);
            c.args(["add", "-A"]);
            c
        });
        assert!(!marker.exists(), "git_untrusted ran the repository's filter");
        let staged = Ctx::git(&dir).args(["ls-files"]).output().unwrap().stdout;
        assert_eq!(String::from_utf8_lossy(&staged).trim(), ".gitattributes\na.txt");
    }
}
