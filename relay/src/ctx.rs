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
    /// `$XDG_DATA_HOME/relay/<name>/scratch`: one directory per job, on disk, that is its `/tmp`.
    pub scratch: PathBuf,
    /// Everything relay keeps for every project, hidden from sessions.
    pub state_root: PathBuf,
    pub data_root: PathBuf,
    /// When the repository is a linked worktree, its git directory and the
    /// main repository's, which its `.git` file and a clone's alternates name.
    pub git_dirs: Vec<PathBuf>,
    /// `state` belongs to `cfg.repo`: a `Ctx` cannot be made without it, so no command reaches a
    /// project directory another checkout of the same `name` owns.
    _claim: Claim,
}

/// The file in a project's state directory naming the repository it belongs to.
const REPO_FILE: &str = "repo";

/// Proof that a project's state directory belongs to one repository. The state directory is
/// found by `name` alone, so two checkouts with one `name` would otherwise share a queue, and a
/// dispatcher started in one would run the other's jobs from its own repository. The first
/// command claims the directory by recording the repository's root in [`REPO_FILE`]; every later
/// one must be in that repository. Only [`Claim::take`] makes one, and [`Ctx`] holds one.
#[derive(Debug)]
struct Claim(());

impl Claim {
    /// Claims `state` (which exists, owner-only) for `repo` (canonical), or refuses because another
    /// repository has. A record already there is only read, so a full or read-only state file system
    /// still opens for its own repository. A directory from before claims were recorded is claimed
    /// by the first command that opens it; two first commands racing: one records, the other reads it.
    fn take(state: &Path, repo: &Path) -> Result<Self> {
        let file = state.join(REPO_FILE);
        match Self::read(&file) {
            Ok(recorded) => return Self::check(state, repo, &file, &recorded),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        }
        match Self::publish(state, repo, &file) {
            Ok(()) => Ok(Self(())),
            // Another command recorded one first: it is the record.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let recorded = Self::read(&file).with_context(|| format!("reading {}", file.display()))?;
                Self::check(state, repo, &file, &recorded)
            }
            Err(e) => Err(e).with_context(|| format!("recording the repository in {}", file.display())),
        }
    }

    /// The record, read without following a link: it is relay's own file, never a pointer elsewhere.
    fn read(file: &Path) -> std::io::Result<Vec<u8>> {
        let mut recorded = Vec::new();
        std::io::Read::read_to_end(
            &mut std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
                .open(file)?,
            &mut recorded,
        )?;
        Ok(recorded)
    }

    /// Records `repo` at `file`: written aside, then linked into place, so the record appears whole
    /// or not at all, and a link never replaces one that is there (`AlreadyExists`).
    fn publish(state: &Path, repo: &Path, file: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        let mut line = repo.as_os_str().as_bytes().to_vec();
        line.push(b'\n');
        let tmp = state.join(format!("{REPO_FILE}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let written = (|| {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(&line)?;
            f.sync_all()?;
            std::fs::hard_link(&tmp, file)
        })();
        let _ = std::fs::remove_file(&tmp);
        written
    }

    /// The claim when `recorded` names `repo`, else the refusal naming both.
    fn check(state: &Path, repo: &Path, file: &Path, recorded: &[u8]) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let recorded = recorded.strip_suffix(b"\n").unwrap_or(recorded);
        let recorded = Path::new(std::ffi::OsStr::from_bytes(recorded));
        anyhow::ensure!(
            recorded == repo,
            "the state directory {} belongs to the repository {}, not {}: give this project another \
             `name` in its relay.toml (if that repository moved here, remove {})",
            state.display(),
            recorded.display(),
            repo.display(),
            file.display()
        );
        Ok(Self(()))
    }
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

/// `$XDG_STATE_HOME/relay`: every project's state directory, by name.
pub fn state_root() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("relay")
}

/// Every project's state directory under `root`: each one with a bus. A listing that fails is
/// none; for a reader that must tell the two apart, [`try_project_dirs`].
pub fn project_dirs(root: &Path) -> impl Iterator<Item = PathBuf> {
    try_project_dirs(root).unwrap_or_default().into_iter()
}

/// As [`project_dirs`], failing when `root` cannot be listed; a `root` that does not exist yet
/// holds no projects. A directory whose bus cannot be looked at is listed, for its reader to fail on.
pub fn try_project_dirs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let d = entry?.path();
        // An entry without a bus is no project; one whose bus cannot even be looked at (no search
        // permission) is, and its reader is the one to say it cannot be read.
        let project = match std::fs::metadata(d.join("bus")) {
            Ok(m) => m.is_dir(),
            Err(e) => !matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory),
        };
        if project {
            dirs.push(d);
        }
    }
    Ok(dirs)
}

/// Every project's event directories under `root`, for a doorbell.
pub fn buses_in(root: &Path) -> Vec<PathBuf> {
    project_dirs(root).flat_map(|d| [d.join("bus"), d.join("bus/session")]).filter(|d| d.is_dir()).collect()
}

/// A job's stream log in the jobs directory `jobs`, where it is now: the plain
/// file while it is written, else the gzip gc made of it.
pub fn log_in(jobs: &Path, job: &str, ext: &str) -> PathBuf {
    let plain = jobs.join(format!("{job}.{ext}"));
    let gz = jobs.join(format!("{job}.{ext}.gz"));
    if !plain.exists() && gz.exists() { gz } else { plain }
}

impl Ctx {
    pub fn new(cfg: Config, cfg_path: PathBuf, json: bool) -> Result<Self> {
        let state_root = state_root();
        let data_root = xdg("XDG_DATA_HOME", ".local/share").join("relay");
        let state = state_root.join(&cfg.name);
        let data = data_root.join(&cfg.name);
        // Briefs, answers and resume messages are private: owner-only from the project directory down.
        let private = |d: &Path| -> Result<()> {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(d)
                .with_context(|| format!("creating {}", d.display()))?;
            Ok(std::fs::set_permissions(d, Permissions::from_mode(0o700))?)
        };
        // The state directory is claimed before anything else of the project's is touched.
        private(&state)?;
        let claim = Claim::take(&state, &cfg.repo)?;
        private(&data)?;
        let ctx = Self {
            bus: state.join("bus"),
            jobs: state.join("jobs"),
            clones: data.join("clones"),
            caches: data.join("caches"),
            scratch: data.join("scratch"),
            git_dirs: git_dirs(&cfg.repo),
            state,
            state_root,
            data_root,
            cfg,
            cfg_path,
            json,
            _claim: claim,
        };
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
        project_dirs(&self.state_root)
    }

    /// Every project's event directories, for a doorbell.
    pub fn buses(&self) -> Vec<PathBuf> {
        buses_in(&self.state_root)
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
        log_in(&self.jobs, job, ext)
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

    /// The job's `/tmp`: a directory on disk (never RAM), bound there in its sandbox. It is counted in
    /// the job's disk use and emptied by gc with the clone's build outputs.
    pub fn job_tmp(&self, job: &str) -> PathBuf {
        self.scratch.join(job)
    }

    /// Removes everything a job left under its name: each `<job>.*` file in the jobs directory (prompt,
    /// logs, results, decision requests, the scratch directory `<job>.d`) and its `/tmp`. Job names hold no
    /// dots, so the `<job>.` prefix is exactly this job's. Used when a name is cleaned away and when it is
    /// added again: a new job never inherits, or appends to, a file of the one that held the name before.
    pub fn wipe_job_files(&self, job: &str) -> Result<()> {
        let prefix = format!("{job}.");
        let gone = |r: std::io::Result<()>| {
            r.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
        };
        for entry in std::fs::read_dir(&self.jobs).with_context(|| format!("reading {}", self.jobs.display()))? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                continue;
            }
            let path = entry.path();
            let removed =
                if entry.file_type()?.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            gone(removed).with_context(|| format!("removing {}", path.display()))?;
        }
        let tmp = self.job_tmp(job);
        gone(std::fs::remove_dir_all(&tmp)).with_context(|| format!("removing {}", tmp.display()))
    }

    /// Writes a file readable by its owner only, replacing what was there.
    pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        std::io::Write::write_all(&mut file, bytes).with_context(|| format!("writing {}", path.display()))
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

#[cfg(test)]
mod claim_tests {
    use super::*;

    fn dirs() -> (crate::config::tempdir::Dir, PathBuf, PathBuf, PathBuf) {
        let d = crate::config::tempdir::Dir::new();
        let (state, a, b) = (d.path().join("state/x"), d.path().join("a"), d.path().join("b"));
        for p in [&state, &a, &b] {
            std::fs::create_dir_all(p).unwrap();
        }
        (d, state, a, b)
    }

    #[test]
    fn the_first_repository_claims_the_state_directory_and_it_alone_opens_it_again() {
        let (_d, state, a, _b) = dirs();
        Claim::take(&state, &a).unwrap();
        assert_eq!(std::fs::read(state.join(REPO_FILE)).unwrap(), format!("{}\n", a.display()).into_bytes());
        Claim::take(&state, &a).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&state).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, [REPO_FILE], "only the record is left in the state directory");
    }

    /// Finding 6: two checkouts with one `name` share a state directory; the second is refused, by both paths.
    #[test]
    fn another_repository_with_the_same_name_is_refused_naming_both() {
        let (_d, state, a, b) = dirs();
        Claim::take(&state, &a).unwrap();
        let e = format!("{:#}", Claim::take(&state, &b).unwrap_err());
        assert!(e.contains(&a.display().to_string()) && e.contains(&b.display().to_string()), "{e}");
        assert!(e.contains(&state.display().to_string()), "{e}");
        // The refusal changed nothing: the first repository still owns it.
        Claim::take(&state, &a).unwrap();
    }

    /// Review of PR #37: an existing record is validated without writing anything, so a state file
    /// system that is full (relay's disk floor) or read-only still opens for its own repository.
    #[test]
    fn an_existing_claim_is_validated_without_writing() {
        let (_d, state, a, b) = dirs();
        Claim::take(&state, &a).unwrap();
        std::fs::set_permissions(&state, Permissions::from_mode(0o500)).unwrap();
        let writable = std::fs::write(state.join("probe"), "").is_ok();
        let (mine, theirs) = (Claim::take(&state, &a), Claim::take(&state, &b));
        std::fs::set_permissions(&state, Permissions::from_mode(0o700)).unwrap();
        assert!(!writable, "the fixture's state directory is still writable (running as root?)");
        mine.unwrap();
        // Another repository is still refused for what it is, not for the directory being read-only.
        let e = format!("{:#}", theirs.unwrap_err());
        assert!(e.contains("belongs to the repository") && e.contains(&a.display().to_string()), "{e}");
    }

    /// A state directory from before claims were recorded belongs to the first repository to open it.
    #[test]
    fn an_unclaimed_state_directory_is_claimed_by_the_first_to_open_it() {
        let (_d, state, a, b) = dirs();
        std::fs::create_dir_all(state.join("bus/session")).unwrap();
        std::fs::write(state.join("jobs.lock"), "").unwrap();
        Claim::take(&state, &b).unwrap();
        assert!(Claim::take(&state, &a).is_err());
    }

    /// The record is relay's own file: a symlink planted in its place, even to a file naming this very
    /// repository, is not read through, and a symlink is never written through either.
    #[test]
    fn a_record_planted_as_a_symlink_is_refused_not_followed() {
        let (d, state, a, _b) = dirs();
        let elsewhere = d.path().join("elsewhere");
        std::fs::write(&elsewhere, format!("{}\n", a.display())).unwrap();
        std::os::unix::fs::symlink(&elsewhere, state.join(REPO_FILE)).unwrap();
        let e = format!("{:#}", Claim::take(&state, &a).unwrap_err());
        assert!(e.contains("reading"), "{e}");
        // A dangling one is not created through.
        let target = d.path().join("made");
        std::fs::remove_file(state.join(REPO_FILE)).unwrap();
        std::os::unix::fs::symlink(&target, state.join(REPO_FILE)).unwrap();
        assert!(Claim::take(&state, &a).is_err());
        assert!(!target.exists(), "the claim was written through a symlink");
    }
}
