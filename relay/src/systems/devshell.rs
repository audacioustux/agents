//! devshell: enters the project's development environment (`session.env_shell`) once, on the
//! host, and records its variables for every session. Emits: warming, warmed, warm-failed.
//!
//! Entering it is a property of the project, not of a job: a cold Nix store can take hours to
//! build a shell, and every session entering its own would race the others and fail at its time
//! limit. So dispatch starts one warm-up (`relay devshell warm`) when a queued job needs the
//! environment and it is not recorded for the current key; the warm-up holds a lock, so at most
//! one runs per project, and records what it entered in `<state>/devshell.json`. Until that file
//! holds the current key, every job is held (`Room::judge`), never started and failed. Sessions
//! and acceptance checks read the file; none enters the shell itself.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use crate::bus::{DevShellEvent, now_micros};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::host::RelayEnv;
use crate::os::Role;
use crate::supervised::Supervised;
use crate::systems::dispatch::Hold;
use crate::world::{DevShell, World};

/// One attempt's time limit. A warm-up that needs longer is attempted again at once: what Nix
/// fetched stays in its store, so each attempt goes further, and jobs stay held meanwhile.
pub const ATTEMPT_SECS: u64 = 600;
/// How much of an attempt's stderr is kept on the bus.
const TAIL_BYTES: u64 = 2048;
/// The wait after the first failed attempt, doubled after each one since, up to `MAX_BACKOFF_US`.
const FIRST_BACKOFF_US: u64 = 60 * 1_000_000;
const MAX_BACKOFF_US: u64 = 15 * 60 * 1_000_000;
/// The files in the repository that define a development environment, for each shell the README
/// names: a change to any of them changes the key, and the environment is entered again.
const DEFINED_BY: &[&str] = &[
    // `nix develop` (a flake), and `nix-shell`.
    "flake.nix",
    "flake.lock",
    "shell.nix",
    "default.nix",
    // `devenv shell`: its inputs and imports are in devenv.yaml, local overrides in devenv.local.nix.
    "devenv.nix",
    "devenv.lock",
    "devenv.yaml",
    "devenv.local.nix",
    // `devbox run`.
    "devbox.json",
    "devbox.lock",
    // `mise exec --`: the project's config files, in each place mise reads one.
    "mise.toml",
    "mise.local.toml",
    ".mise.toml",
    ".mise.local.toml",
    ".config/mise.toml",
    ".config/mise/config.toml",
    ".mise/config.toml",
    ".tool-versions",
];

/// The capability to write devshell's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> PathBuf {
        crate::bus::system_file("devshell")
    }
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Cmd {
    /// Enter the development environment now, in the repository, and record it for the sessions.
    /// Dispatch starts this when a queued job needs it; exits 4 while another warm-up runs.
    Warm,
}

pub fn run(ctx: &Ctx, Cmd::Warm: Cmd) -> Result<()> {
    let said = warm(ctx)?;
    ctx.reply(&serde_json::json!({ "devshell": said }), &format!("devshell: {said}"))?;
    ctx.wake_dispatch()
}

/// The key the environment is recorded for: everything that changes what a warm-up captures. The
/// shell command, `session.env` (the environment it is entered from), `sandbox.env_deny` (what is
/// left out of the record) and the repository's files that define it (`DEFINED_BY`). `None` when
/// the project names no `session.env_shell`.
pub fn key(cfg: &Config) -> Option<String> {
    use sha2::{Digest, Sha256};
    if cfg.session.env_shell.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    for part in &cfg.session.env_shell {
        h.update(part.as_bytes());
        h.update([0]);
    }
    // Lengths before each part, so no two settings hash alike by moving a boundary.
    let mut part = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part(b"session.env");
    for (k, v) in &cfg.session.env {
        part(k.as_bytes());
        part(v.as_bytes());
    }
    part(b"sandbox.env_deny");
    part(cfg.sandbox.env_deny.as_bytes());
    for name in DEFINED_BY {
        h.update(name.as_bytes());
        match std::fs::read(cfg.repo.join(name)) {
            Ok(bytes) => {
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update([0xff]),
        }
    }
    Some(h.finalize().iter().take(12).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    }))
}

/// What a warm-up recorded: the environment's variables, after `sandbox.env_deny`, the key it was
/// entered for, and the paths it stands on. Kept in the project's state directory, which only its
/// owner can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub key: String,
    pub env: Vec<(String, String)>,
    /// The paths the environment named that existed when it was recorded ([`keeps`]): while one is
    /// gone (a Nix garbage collection took its store path) the record is not whole, and is not used.
    #[serde(default)]
    pub keeps: Vec<String>,
}

impl Recorded {
    /// The first path the environment stands on that is gone, if any: a stat each.
    pub fn missing(&self) -> Option<&str> {
        self.keeps.iter().map(String::as_str).find(|p| !Path::new(p).exists())
    }
}

/// The paths a recorded environment stands on, of those `exists` says are there: every absolute
/// `PATH` directory (split on `:` alone, so a directory may have spaces in it), and every store path
/// (`/nix/store/<name>`, `/gnu/store/<name>`) any value names, alone or in a `:` or space separated
/// list (Nix's `buildInputs` are spaced; a store path never has a space). A path that was not there when it was recorded (a
/// shell's temporary directory, the output a `nix develop` shell would build) is not one it stands on.
fn keeps(env: &[(String, String)], exists: impl Fn(&Path) -> bool) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for (k, v) in env {
        let parts: Vec<&str> = if k == "PATH" { v.split(':').collect() } else { v.split([':', ' ']).collect() };
        for part in parts.into_iter().filter(|p| p.starts_with('/')) {
            let store = ["/nix/store/", "/gnu/store/"].iter().find_map(|s| {
                let name = part.strip_prefix(s)?.split('/').next().filter(|n| !n.is_empty())?;
                Some(format!("{s}{name}"))
            });
            match store {
                Some(root) => {
                    out.insert(root);
                }
                None if k == "PATH" => {
                    out.insert(part.to_owned());
                }
                None => {}
            }
        }
    }
    out.into_iter().filter(|p| exists(Path::new(p))).collect()
}

fn recorded_file(ctx: &Ctx) -> PathBuf {
    ctx.state.join("devshell.json")
}

fn lock_file(ctx: &Ctx) -> PathBuf {
    ctx.state.join("devshell.lock")
}

/// Where an attempt's stderr goes, whole; its end is on the bus.
pub fn stderr_file(ctx: &Ctx) -> PathBuf {
    ctx.state.join("devshell.stderr")
}

pub fn recorded(ctx: &Ctx) -> Option<Recorded> {
    serde_json::from_slice(&std::fs::read(recorded_file(ctx)).ok()?).ok()
}

fn record(ctx: &Ctx, r: &Recorded) -> Result<()> {
    let file = recorded_file(ctx);
    let tmp = file.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(&serde_json::to_vec(r)?)?;
    f.sync_all()?;
    std::fs::rename(&tmp, &file).with_context(|| format!("writing {}", file.display()))
}

/// A development environment's variables, as a job's environment takes them.
pub type Env = Vec<(OsString, Option<OsString>)>;

/// The environment one attempt runs on, read once when it starts and given to its session and to
/// both runs of its acceptance check, so a warm-up for a new key during the run changes none of
/// them. Empty when the project names no development environment; `None` when the one recorded is
/// not for the current key (or none is), or no longer whole ([`Recorded::missing`]), and the
/// attempt must wait for its warm-up.
pub fn snapshot(ctx: &Ctx) -> Option<Env> {
    let Some(key) = key(&ctx.cfg) else { return Some(Vec::new()) };
    let r = recorded(ctx).filter(|r| r.key == key && r.missing().is_none())?;
    // Filtered again: what a session gets is judged by the setting it runs under (and a record from
    // before working directories were left out has none of them either).
    let deny = regex::Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    Some(r.env.into_iter().filter(|(k, _)| carried(k, &deny)).map(|(k, v)| (k.into(), Some(v.into()))).collect())
}

/// Whether the variable `name` of an entered environment is recorded and given to sessions: not
/// one `deny` names, and not one that says where the warm-up stood (`PWD`, the repository; its
/// `TMPDIR`), which [`crate::sandbox::session_env`] drops from every layer and must not get back.
fn carried(name: &str, deny: &regex::Regex) -> bool {
    !name.is_empty() && !deny.is_match(name) && !crate::sandbox::WHERE_IT_STANDS.contains(&name)
}

/// Whether the development environment lets jobs start, judged once per pass or board: the
/// current key, whether it is recorded for it, and what the bus says of its warm-ups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gate {
    key: String,
    ready: bool,
    /// A path the record for this key stands on that is gone: it must be entered again.
    gone: Option<String>,
    state: Option<DevShell>,
}

impl Gate {
    /// The gate for the project `ctx` serves, from its fold `world`; `None` when it names no
    /// development environment.
    pub fn of(ctx: &Ctx, world: &World) -> Option<Self> {
        let key = key(&ctx.cfg)?;
        let record = recorded(ctx).filter(|r| r.key == key);
        let gone = record.as_ref().and_then(|r| r.missing().map(str::to_owned));
        let ready = record.is_some() && gone.is_none();
        let state = world.devshell.clone().filter(|s| s.key == key);
        Some(Self { key, ready, gone, state })
    }

    /// When the next warm-up may start after the latest failed one: at once after a timeout,
    /// since that warm-up was still going, else after a back-off that doubles with each failure.
    fn retry_at(s: &DevShell) -> u64 {
        let Some(f) = &s.failed else { return 0 };
        if f.timed_out {
            return f.at;
        }
        let doublings = s.failures.saturating_sub(1).min(16);
        f.at.saturating_add((FIRST_BACKOFF_US << doublings).min(MAX_BACKOFF_US))
    }

    /// Why a job that needs the environment does not start at `now_us` (bus time), and when that
    /// could change with no event; `None` once it is recorded for the current key.
    pub fn hold(&self, now_us: u64) -> Option<Hold> {
        if self.ready {
            return None;
        }
        let not_yet = || {
            let why = match &self.gone {
                Some(path) => format!("dev shell needs warming again: {path} is gone"),
                None => "dev shell not warmed yet".into(),
            };
            Hold { why, until: None }
        };
        let Some(s) = &self.state else { return Some(not_yet()) };
        if let Some(since) = s.warming_since {
            let mut why = format!("dev shell warming ({})", crate::systems::view::ago(now_us, since));
            if s.failed.as_ref().is_some_and(|f| f.timed_out) {
                let _ = write!(why, "; attempt {} stopped at {ATTEMPT_SECS} s, going on", s.failures);
            }
            return Some(Hold { why, until: None });
        }
        let Some(f) = &s.failed else { return Some(not_yet()) };
        let retry = Self::retry_at(s);
        let at = i64::try_from(retry)
            .ok()
            .and_then(|us| jiff::Timestamp::from_microsecond(us).ok())
            .map_or_else(String::new, |t| crate::os::local(t).strftime("%H:%M %Z").to_string());
        let mut why = format!("dev shell failed: {} (attempt {}, retry {at})", f.why, s.failures);
        if let Some(line) = last_line(&f.tail) {
            why.push_str(": ");
            why.push_str(&line);
        }
        Some(Hold { why, until: Some(retry) })
    }

    /// Whether a warm-up should start at `now_us`: not recorded for the current key, and no
    /// failed one too recent. One may be under way; the lock says so.
    fn due(&self, now_us: u64) -> bool {
        !self.ready && self.state.as_ref().is_none_or(|s| s.warming_since.is_some() || Self::retry_at(s) <= now_us)
    }
}

/// The last line of `tail` that says something, cut to a board's width.
fn last_line(tail: &str) -> Option<String> {
    let line = tail.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let mut cut: String = line.chars().take(200).collect();
    if cut.len() < line.len() {
        cut.push('…');
    }
    Some(cut)
}

/// Starts a warm-up (`relay devshell warm`, a process of its own) when one is due by `gate` and
/// a queued job is free to start but for it, unless one already runs. Dispatch calls this on
/// every pass; the warm-up's end rings the doorbell.
pub fn tend(ctx: &Ctx, world: &World, gate: Option<&Gate>) -> Result<()> {
    let Some(_) = gate.filter(|g| g.due(now_micros())) else { return Ok(()) };
    if !world.jobs().values().any(|j| world.startable(j)) {
        return Ok(());
    }
    // Free now means none is under way: one dispatcher per project, so nothing else starts one meanwhile.
    if Role::try_take(&lock_file(ctx))?.is_none() {
        return Ok(());
    }
    ctx.spawn_self(&["devshell", "warm"], &ctx.state.join("devshell.log"))?;
    Ok(())
}

/// One warm-up: enters the environment under the project's lock and records it. Returns what
/// it did (`warm`, `entered`, `failed: ...`); a failure to start it is on the bus, not an error.
pub(crate) fn warm(ctx: &Ctx) -> Result<String> {
    let Some(key) = key(&ctx.cfg) else { return Ok("no session.env_shell: nothing to enter".into()) };
    let Some(role) = Role::try_take(&lock_file(ctx))? else {
        fail!(Conflict, "a warm-up of the development environment is already running")
    };
    // Recorded for this key and still whole; a record whose paths were collected is entered again.
    if recorded(ctx).is_some_and(|r| r.key == key && r.missing().is_none()) {
        return Ok("warm".into());
    }
    let shell = &ctx.cfg.session.env_shell;
    let port = ctx.port(&Source(()));
    port.emit(DevShellEvent::Warming { key: key.clone(), shell: shell.join(" ") })?;
    let deny = regex::Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    // The environment the dispatcher would give it, never the caller's: a warm-up run by hand records
    // the same variables as one dispatch started, and nothing of the shell that ran it.
    // The shell holds the lock too, as its stdin: if this process is killed, no warm-up can start
    // until every process of the attempt has ended (its supervisor ends them with this one).
    let lock = role.share()?;
    match enter(&ctx.relay_env(), lock, shell, &deny, &ctx.cfg.repo, &stderr_file(ctx), ATTEMPT_SECS) {
        Ok(env) => {
            let keeps = keeps(&env, Path::exists);
            record(ctx, &Recorded { key: key.clone(), env, keeps })?;
            port.emit(DevShellEvent::Warmed { key })?;
            Ok("entered".into())
        }
        Err(Failure { why, tail, timed_out }) => {
            let said = format!("failed: {why}");
            port.emit(DevShellEvent::WarmFailed { key, why, tail, timed_out })?;
            Ok(said)
        }
    }
}

/// Why an attempt did not start the environment.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    why: String,
    tail: String,
    timed_out: bool,
}

/// The development environment entered on the host in `dir` through `shell`, from `env` and
/// nothing of the caller's, under `timeout` for at most `limit` seconds, minus names `deny`
/// matches. Its stderr goes to `stderr`, and `stdin` (the warm-up's lock, shared) is its stdin.
/// It runs supervised: in a process group of its own, killed whole when `timeout` exits or this
/// process ends, so nothing the shell leaves behind (a hook's background child holding its stdout
/// or the lock) outlives the attempt.
fn enter(
    env: &RelayEnv,
    stdin: Stdio,
    shell: &[String],
    deny: &regex::Regex,
    dir: &Path,
    stderr: &Path,
    limit: u64,
) -> Result<Vec<(String, String)>, Failure> {
    // A marker first, so whatever the environment's own hooks print is not taken for variables.
    const MARK: &str = "\0relay-env\0";
    let Some((program, prefix)) = shell.split_first() else { return Ok(Vec::new()) };
    let fail = |why: String, tail: String, timed_out| Failure { why, tail, timed_out };
    let err = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(stderr)
        .map_err(|e| fail(format!("could not open {}: {e}", stderr.display()), String::new(), false))?;
    let started = Instant::now();
    let mut cmd = env
        .supervised("timeout")
        .map_err(|e| fail(format!("could not run relay supervise: {e}"), String::new(), false))?;
    // `--foreground` keeps `timeout`, and so the shell and all it starts, in the supervisor's group:
    // without it `timeout` makes a group of its own, which nothing would end with the attempt.
    cmd.args(["--foreground", "-k", "5", &limit.to_string()])
        .arg(program)
        .args(prefix)
        .args(["sh", "-c", &format!("printf '{}'; env -0", MARK.replace('\0', "\\0"))])
        .current_dir(dir)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(err);
    let mut child =
        Supervised::spawn(&mut cmd).map_err(|e| fail(format!("could not run {program}: {e}"), String::new(), false))?;
    // Read beside the wait, never before it: a background child can hold the output open past
    // `timeout`'s exit, and only the group's end, once `timeout` has exited, closes it.
    let out = child.child().stdout.take();
    let reader = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        if let Some(mut out) = out {
            let _ = out.read_to_end(&mut stdout);
        }
        stdout
    });
    let status = match child.finish(|| reader.is_finished(), || None::<std::convert::Infallible>) {
        Ok(Ok(status)) => status,
        Ok(Err(never)) => match never {},
        Err(e) => return Err(fail(format!("waiting for {program}: {e}"), String::new(), false)),
    };
    let stdout = reader.join().unwrap_or_default();
    let tail = std::fs::File::open(stderr).map(|f| tail_of(f, TAIL_BYTES)).unwrap_or_default();
    if timed_out(status, started.elapsed(), limit) {
        return Err(fail(format!("timed out after {limit} s"), tail, true));
    }
    if !status.success() {
        let why = status.code().map_or_else(|| "killed by a signal".to_owned(), |c| format!("exited {c}"));
        return Err(fail(why, tail, false));
    }
    let Some(at) = stdout.windows(MARK.len()).position(|w| w == MARK.as_bytes()) else {
        return Err(fail("printed no environment".into(), tail, false));
    };
    Ok(stdout[at + MARK.len()..]
        .split(|&b| b == 0)
        .filter_map(|kv| {
            // Only what is text: the record is JSON, and no variable a tool needs is otherwise.
            let (k, v) = std::str::from_utf8(kv).ok()?.split_once('=')?;
            carried(k, deny).then(|| (k.to_owned(), v.to_owned()))
        })
        .collect())
}

/// Whether an attempt that ended with `status` after `elapsed` was stopped at its `limit` (seconds):
/// `timeout` exits 124 when the shell ends on its TERM; one that ignores the TERM is killed 5 s
/// later, and `timeout` dies by that SIGKILL itself, seen as the signal or, through a supervisor
/// that reports a signal as 128 + its number, as 137. Only once the limit has passed: the shell can
/// exit 124 itself (a nested `timeout`), or be killed by something else, before it, and that is a
/// failure like any other.
fn timed_out(status: std::process::ExitStatus, elapsed: std::time::Duration, limit: u64) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    let stopped = status.code() == Some(124) || status.signal() == Some(9) || status.code() == Some(137);
    stopped && elapsed >= std::time::Duration::from_secs(limit)
}

fn tail_of(mut f: std::fs::File, max: u64) -> String {
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bus::{Event, Stamped};
    use crate::config::tempdir::Dir;

    /// [`warm`], once no other test's child still holds a copy of this test's lock. Tests run in
    /// threads of one process, and a child another thread is starting holds a copy of every
    /// descriptor between its fork and its exec, the lock's too, for long enough to be seen. A warm-up
    /// of its own never meets one: the warmer starts nothing else meanwhile.
    pub(crate) fn settled(ctx: &Ctx) -> String {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match warm(ctx) {
                Err(e) if e.to_string().contains("already running") && std::time::Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                said => return said.unwrap(),
            }
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A project whose `session.env_shell` is `shell`, its state under the test's directory.
    fn project(d: &Dir, shell: &[&str]) -> Ctx {
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let mut cfg = Config::parse("name = \"t\"\n", &repo).unwrap();
        cfg.session.env_shell = strings(shell);
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        std::fs::create_dir_all(&ctx.bus).unwrap();
        ctx
    }

    /// A shell that counts its entries in `<dir>/entries`, sleeps `secs`, then succeeds or fails.
    fn script(d: &Dir, secs: u32, ok: bool) -> String {
        let path = d.path().join(if ok { "shell-ok" } else { "shell-bad" });
        let entries = d.path().join("entries");
        let end = if ok {
            "export DEV_TOOL=/nix/store/x-tool API_TOKEN=s3cret\nexec \"$@\"\n"
        } else {
            "echo 'error: unable to download nixpkgs: HTTP 503' >&2\nexit 1\n"
        };
        let text =
            format!("#!/bin/sh\necho entered >> '{}'\necho 'warming up' >&2\nsleep {secs}\n{end}", entries.display());
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        path.display().to_string()
    }

    fn entries(d: &Dir) -> usize {
        std::fs::read_to_string(d.path().join("entries")).map_or(0, |s| s.lines().count())
    }

    fn world(ctx: &Ctx) -> World {
        let events: Vec<Stamped> = ctx.events().unwrap();
        World::fold(&events)
    }

    /// Review of #54: a warm-up run by hand (`relay devshell warm` from someone's shell) records what one
    /// dispatch started would, never the caller's own variables: they would pass the deny filter
    /// (`DATABASE_URL`, `GIT_WORK_TREE`) and reach every session.
    #[test]
    fn a_callers_own_variables_are_not_recorded() {
        let d = Dir::new();
        let ctx = project(&d, &[&script(&d, 0, true)]);
        #[allow(clippy::disallowed_methods, reason = "the caller's environment is what must stay out")]
        let callers: Vec<String> = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
        let cmd = ctx.relay_env().command("true");
        let relay: Vec<String> = cmd.get_envs().filter_map(|(k, _)| k.to_str().map(str::to_owned)).collect();
        let theirs: Vec<&String> = callers.iter().filter(|k| !relay.contains(k)).collect();
        assert!(!theirs.is_empty(), "the test process has variables of its own (cargo's, at least)");
        assert_eq!(settled(&ctx), "entered");
        let recorded = recorded(&ctx).unwrap();
        // What the shell sets itself, and the script's own export.
        let own = ["PWD", "OLDPWD", "SHLVL", "_", "DEV_TOOL"];
        let leaked: Vec<&String> =
            recorded.env.iter().map(|(k, _)| k).filter(|k| !relay.contains(k) && !own.contains(&k.as_str())).collect();
        assert!(leaked.is_empty(), "the caller's variables reached the record: {leaked:?}");
        assert!(recorded.env.iter().any(|(k, _)| k == "DEV_TOOL"));
    }

    /// Review of #54: a warmer killed mid-attempt released its own lock while `timeout` and the shell ran
    /// on, so a second warm-up could enter beside them. The attempt's processes hold the lock too (their
    /// stdin): with the warmer's own copy gone, no replacement starts until the attempt has ended.
    #[test]
    fn the_lock_is_held_until_the_attempt_has_ended_even_without_its_warmer() {
        let d = Dir::new();
        let ctx = project(&d, &[&script(&d, 2, true)]);
        let role = Role::try_take(&lock_file(&ctx)).unwrap().unwrap();
        let stdin = role.share().unwrap();
        let (relay, deny) = (ctx.relay_env(), regex::Regex::new("TOKEN").unwrap());
        let (shell, repo, err) = (ctx.cfg.session.env_shell.clone(), ctx.cfg.repo.clone(), stderr_file(&ctx));
        let attempt = std::thread::spawn(move || enter(&relay, stdin, &shell, &deny, &repo, &err, 30));
        std::thread::sleep(std::time::Duration::from_millis(500));
        // The warmer is gone: its own descriptor is closed.
        drop(role);
        assert!(Role::try_take(&lock_file(&ctx)).unwrap().is_none(), "the attempt still holds it");
        let e = warm(&ctx).unwrap_err();
        assert!(e.to_string().contains("already running"), "{e:#}");
        assert_eq!(entries(&d), 1, "no second entry beside the first");
        assert!(attempt.join().unwrap().is_ok());
        assert_eq!(settled(&ctx), "entered", "a replacement may run once the attempt has ended");
    }

    /// Review of #54 (second round): the record does not keep its store paths alive, so after a garbage
    /// collection the same key would admit jobs whose tools are gone. A record whose paths are gone is
    /// not used, and is entered again.
    #[test]
    fn a_recorded_environment_whose_path_directory_vanished_is_entered_again() {
        let d = Dir::new();
        let tools = d.path().join("store/abc-tools/bin");
        let shell = d.path().join("shell");
        let text = format!(
            "#!/bin/sh\necho entered >> '{}'\nmkdir -p '{}'\nexport PATH='{}':\"$PATH\"\nexec \"$@\"\n",
            d.path().join("entries").display(),
            tools.display(),
            tools.display()
        );
        std::fs::write(&shell, text).unwrap();
        std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let ctx = project(&d, &[&shell.display().to_string()]);
        assert_eq!(settled(&ctx), "entered");
        let r = recorded(&ctx).unwrap();
        assert!(r.keeps.contains(&tools.display().to_string()), "{:?}", r.keeps);
        assert!(snapshot(&ctx).is_some());
        // Collected: the key is the same, the tools are not there.
        std::fs::remove_dir_all(d.path().join("store")).unwrap();
        assert!(snapshot(&ctx).is_none(), "no attempt starts on it");
        let gate = Gate::of(&ctx, &world(&ctx)).unwrap();
        let hold = gate.hold(now_micros()).unwrap();
        assert_eq!(hold.why, format!("dev shell needs warming again: {} is gone", tools.display()));
        assert!(gate.due(now_micros()), "a warm-up is started for it");
        assert_eq!(settled(&ctx), "entered", "`warm` enters it again rather than say it is warm");
        assert_eq!(entries(&d), 2);
        assert!(snapshot(&ctx).is_some(), "whole again");
        assert_eq!(Gate::of(&ctx, &world(&ctx)).unwrap().hold(now_micros()), None);
    }

    #[test]
    fn an_environment_stands_on_its_path_and_the_store_paths_it_names_that_exist() {
        let env = [
            ("PATH", "/nix/store/aaa-cargo/bin:/home/u/.local/bin:relative:/gone/bin:/opt/tool chain/bin"),
            ("buildInputs", "/nix/store/bbb-openssl-3 /nix/store/ccc-zlib"),
            ("LIBCLANG_PATH", "/nix/store/ddd-clang/lib"),
            ("out", "/nix/store/eee-shell"),
            ("HOME", "/home/u"),
            ("GUIX_ENVIRONMENT", "/gnu/store/fff-profile"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let there = |p: &Path| !matches!(p.to_str(), Some("/gone/bin" | "/nix/store/eee-shell"));
        assert_eq!(
            keeps(&env, there),
            [
                "/gnu/store/fff-profile",
                "/home/u/.local/bin",
                "/nix/store/aaa-cargo",
                "/nix/store/bbb-openssl-3",
                "/nix/store/ccc-zlib",
                "/nix/store/ddd-clang",
                "/opt/tool chain/bin",
            ],
            "store roots, PATH directories (whole, spaces and all); not what was never there, not plain paths of \
             other variables"
        );
    }

    /// Review of #54 (second round): GNU `timeout` used to put the shell in a group of its own, so a hook's
    /// background child holding stdout outlived it, the read waited for that child, and the lock and every
    /// queued job with it. The attempt ends at its limit whatever the shell leaves behind, and nothing of it
    /// keeps the lock.
    #[test]
    fn a_background_child_holding_stdout_does_not_hold_the_warm_up() {
        let d = Dir::new();
        let ctx = project(&d, &["sh", "-c", "exec \"$@\"", "s"]);
        let deny = regex::Regex::new("TOKEN").unwrap();
        let err = d.path().join("stderr");
        let relay = RelayEnv::of([("PATH", "/usr/bin:/bin")]);
        let started = std::time::Instant::now();
        // A hook that leaves a child behind, then enters as asked: done at once.
        let hook = strings(&["sh", "-c", "sleep 60 & export DEV_TOOL=x; exec \"$@\"", "s"]);
        let role = Role::try_take(&lock_file(&ctx)).unwrap().unwrap();
        let env = enter(&relay, role.share().unwrap(), &hook, &deny, Path::new("/"), &err, 30).unwrap();
        assert!(env.iter().any(|(k, _)| k == "DEV_TOOL"));
        assert!(started.elapsed().as_secs() < 10, "done when the shell is: {:?}", started.elapsed());
        drop(role);
        assert_eq!(settled(&ctx), "entered", "the child it left keeps no lock: a warm-up can run");
        // One that hangs past its limit, with a child holding the output: stopped at the limit.
        let started = std::time::Instant::now();
        let hung = strings(&["sh", "-c", "sleep 60 & sleep 60", "s"]);
        let f = enter(&relay, Stdio::null(), &hung, &deny, Path::new("/"), &err, 1).unwrap_err();
        assert!(f.timed_out, "{f:?}");
        assert!(started.elapsed().as_secs() < 10, "stopped at its limit: {:?}", started.elapsed());
    }

    /// Review of #54 (third round): an early 124 is the shell's own exit, retried after a back-off, not
    /// at once as a warm-up that ran out of time is.
    #[test]
    fn a_shell_that_exits_124_early_has_failed() {
        let d = Dir::new();
        let (deny, err) = (regex::Regex::new("TOKEN").unwrap(), d.path().join("stderr"));
        let relay = RelayEnv::of([("PATH", "/usr/bin:/bin")]);
        let nested = strings(&["sh", "-c", "exit 124", "s"]);
        let f = enter(&relay, Stdio::null(), &nested, &deny, Path::new("/"), &err, 30).unwrap_err();
        assert_eq!((f.why.as_str(), f.timed_out), ("exited 124", false));
    }

    /// Review of #54: a shell that ignores the TERM gets `timeout` killed by SIGKILL, with no exit
    /// code. After the limit that is a timeout (retried at once), not a failure to back off from.
    #[test]
    fn a_kill_after_the_limit_is_a_timeout() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::ExitStatus;
        use std::time::Duration;
        let (code, signal) = (|c: i32| ExitStatus::from_raw(c << 8), ExitStatus::from_raw);
        let (late, early) = (Duration::from_secs(605), Duration::from_secs(3));
        assert!(timed_out(code(124), late, 600), "timeout's own exit, at the limit");
        assert!(
            !timed_out(code(124), early, 600),
            "the shell's own 124 before the limit (a nested timeout) is a failure"
        );
        assert!(timed_out(signal(9), late, 600), "killed by SIGKILL after the limit");
        assert!(timed_out(code(137), late, 600), "the same, reported through a supervisor");
        assert!(!timed_out(signal(9), early, 600), "killed before the limit: something else killed it");
        assert!(!timed_out(code(137), early, 600));
        assert!(!timed_out(signal(15), late, 600), "a TERM is not the kill after it");
        assert!(!timed_out(code(1), late, 600), "a failure is a failure");
        assert!(!timed_out(code(0), late, 600));
    }

    #[test]
    fn the_environment_is_read_after_the_marker_and_filtered() {
        let d = Dir::new();
        // A "shell" that prints a banner, sets variables, then runs the command it is given.
        let shell =
            strings(&["sh", "-c", "echo welcome; export DEV_TOOL=/nix/store/x API_TOKEN=s3cret; exec \"$@\"", "shell"]);
        let deny = regex::Regex::new("TOKEN").unwrap();
        let err = d.path().join("stderr");
        let relay = RelayEnv::of([("PATH", "/usr/bin:/bin")]);
        let env = enter(&relay, Stdio::null(), &shell, &deny, Path::new("/"), &err, 30).unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("DEV_TOOL"), Some("/nix/store/x"));
        assert_eq!(get("API_TOKEN"), None, "secret-named variables stay out");
        assert!(env.iter().all(|(k, _)| !k.contains("welcome")), "the banner is not a variable");
        let f = enter(
            &relay,
            Stdio::null(),
            &strings(&["sh", "-c", "echo broken >&2; exit 3", "s"]),
            &deny,
            Path::new("/"),
            &err,
            30,
        );
        assert_eq!(f, Err(Failure { why: "exited 3".into(), tail: "broken\n".into(), timed_out: false }));
        let slow =
            enter(&relay, Stdio::null(), &strings(&["sh", "-c", "sleep 5", "s"]), &deny, Path::new("/"), &err, 1)
                .unwrap_err();
        assert!(slow.timed_out, "{slow:?}");
        assert_eq!(enter(&relay, Stdio::null(), &[], &deny, Path::new("/"), &err, 30), Ok(Vec::new()));
    }

    /// The incident of 2026-11-12: a shell that takes long to enter. Jobs are held while it warms, at
    /// most one warm-up runs however many ask, and once it is recorded every job starts on it.
    #[test]
    fn a_slow_shell_holds_jobs_and_is_entered_once() {
        let d = Dir::new();
        let ctx = project(&d, &[&script(&d, 2, true)]);
        let gate = Gate::of(&ctx, &world(&ctx)).unwrap();
        assert_eq!(gate.hold(now_micros()).unwrap().why, "dev shell not warmed yet");
        assert!(gate.due(now_micros()));
        let said = std::thread::scope(|s| {
            let first = s.spawn(|| warm(&ctx).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(500));
            // While it warms: held, with how long, and a second warm-up is refused.
            let gate = Gate::of(&ctx, &world(&ctx)).unwrap();
            let why = gate.hold(now_micros()).unwrap().why;
            assert!(why.starts_with("dev shell warming ("), "{why}");
            let second = warm(&ctx).unwrap_err();
            assert!(second.to_string().contains("already running"), "{second:#}");
            first.join().unwrap()
        });
        assert_eq!(said, "entered");
        assert_eq!(entries(&d), 1, "one entry");
        let gate = Gate::of(&ctx, &world(&ctx)).unwrap();
        assert_eq!(gate.hold(now_micros()), None);
        assert!(!gate.due(now_micros()));
        assert!(snapshot(&ctx).is_some());
        assert_eq!(settled(&ctx), "warm", "already recorded: not entered again");
        assert_eq!(entries(&d), 1);
        // Every session reads the record; the secret-looking variable is not in it.
        let env = snapshot(&ctx).unwrap();
        assert!(env.iter().any(|(k, v)| k == "DEV_TOOL" && v.as_deref() == Some("/nix/store/x-tool".as_ref())));
        assert!(env.iter().all(|(k, _)| k != "API_TOKEN"), "filtered");
        let on_disk = std::fs::read_to_string(recorded_file(&ctx)).unwrap();
        assert!(!on_disk.contains("s3cret"), "never stored");
        assert_eq!(entries(&d), 1);
    }

    #[test]
    fn a_failing_shell_holds_jobs_with_its_reason_and_backs_off() {
        let d = Dir::new();
        let ctx = project(&d, &[&script(&d, 0, false)]);
        assert_eq!(settled(&ctx), "failed: exited 1");
        assert!(snapshot(&ctx).is_none());
        let w = world(&ctx);
        let gate = Gate::of(&ctx, &w).unwrap();
        let at = w.devshell.as_ref().unwrap().failed.as_ref().unwrap().at;
        let hold = gate.hold(at).unwrap();
        assert!(hold.why.starts_with("dev shell failed: exited 1 (attempt 1, retry "), "{}", hold.why);
        assert!(hold.why.ends_with(": error: unable to download nixpkgs: HTTP 503"), "{}", hold.why);
        assert_eq!(hold.until, Some(at + FIRST_BACKOFF_US));
        assert!(!gate.due(at + FIRST_BACKOFF_US - 1), "backing off");
        assert!(gate.due(at + FIRST_BACKOFF_US));
        settled(&ctx);
        let w = world(&ctx);
        let gate = Gate::of(&ctx, &w).unwrap();
        let at = w.devshell.as_ref().unwrap().failed.as_ref().unwrap().at;
        assert_eq!(gate.hold(at).unwrap().until, Some(at + 2 * FIRST_BACKOFF_US), "doubled");
        assert_eq!(entries(&d), 2);
        assert!(snapshot(&ctx).is_none(), "nothing recorded");
    }

    /// Review of #54 (third round): devenv's inputs and imports live in devenv.yaml, which no other
    /// hashed file reflects; and each shell the README names has its own files.
    #[test]
    fn each_documented_shells_own_files_are_in_the_key() {
        let d = Dir::new();
        let ctx = project(&d, &["devenv", "shell"]);
        for file in
            ["devenv.yaml", "devenv.local.nix", "devbox.json", "mise.toml", ".tool-versions", ".config/mise.toml"]
        {
            let path = ctx.cfg.repo.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let before = key(&ctx.cfg).unwrap();
            std::fs::write(&path, "inputs:\n  nixpkgs:\n    url: github:cachix/devenv-nixpkgs/rolling\n").unwrap();
            assert_ne!(key(&ctx.cfg).unwrap(), before, "{file} is in the key");
        }
    }

    #[test]
    fn a_changed_key_enters_again() {
        let d = Dir::new();
        let ctx = project(&d, &[&script(&d, 0, true)]);
        let before = key(&ctx.cfg).unwrap();
        assert_eq!(settled(&ctx), "entered");
        assert!(snapshot(&ctx).is_some());
        std::fs::write(ctx.cfg.repo.join("flake.lock"), "{\"nodes\":{}}").unwrap();
        let after = key(&ctx.cfg).unwrap();
        assert_ne!(before, after, "the lock is in the key");
        assert!(snapshot(&ctx).is_none());
        let gate = Gate::of(&ctx, &world(&ctx)).unwrap();
        assert_eq!(
            gate.hold(now_micros()).unwrap().why,
            "dev shell not warmed yet",
            "the old key's record is not this one's"
        );
        assert!(gate.due(now_micros()));
        assert_eq!(settled(&ctx), "entered");
        assert_eq!(entries(&d), 2, "entered again");
        assert!(snapshot(&ctx).is_some());
        // The shell command is in the key too.
        let mut other = ctx.cfg.clone();
        other.session.env_shell.push("--impure".into());
        assert_ne!(key(&other), Some(after));
        let mut none = ctx.cfg.clone();
        none.session.env_shell.clear();
        assert_eq!(key(&none), None);
    }

    /// Review of #54: what the shell is entered from (`session.env`) and what is left out of the record
    /// (`sandbox.env_deny`) change what a warm-up captures, so each is in the key.
    #[test]
    fn the_environment_it_is_entered_from_and_the_filter_are_in_the_key() {
        let d = Dir::new();
        let ctx = project(&d, &["nix", "develop", "-c"]);
        let base = key(&ctx.cfg).unwrap();
        let mut env = ctx.cfg.clone();
        env.session.env.insert("NIX_CONFIG".into(), "substituters = https://cache.example".into());
        let with_env = key(&env).unwrap();
        assert_ne!(with_env, base, "session.env is in the key");
        env.session.env.insert("NIX_CONFIG".into(), "substituters = https://other.example".into());
        assert_ne!(key(&env).unwrap(), with_env, "and its values");
        let mut deny = ctx.cfg.clone();
        deny.sandbox.env_deny.push_str("|^DATABASE_URL$");
        assert_ne!(key(&deny).unwrap(), base, "sandbox.env_deny is in the key");
        assert_eq!(key(&ctx.cfg).unwrap(), base, "the same settings, the same key");
    }

    #[test]
    fn a_timed_out_attempt_goes_on_and_a_warm_up_left_by_a_dead_warmer_is_started_again() {
        let d = Dir::new();
        let ctx = project(&d, &["true"]);
        let k = key(&ctx.cfg).unwrap();
        let st = |t, e| Stamped { t, event: Event::DevShell(e), from: None };
        let mut events = vec![
            st(10, DevShellEvent::Warming { key: k.clone(), shell: "s".into() }),
            st(
                20,
                DevShellEvent::WarmFailed {
                    key: k.clone(),
                    why: "timed out after 600 s".into(),
                    tail: String::new(),
                    timed_out: true,
                },
            ),
            st(30, DevShellEvent::Warming { key: k.clone(), shell: "s".into() }),
        ];
        let gate = Gate::of(&ctx, &World::fold(&events)).unwrap();
        let hold = gate.hold(10 + 15 * 60 * 1_000_000).unwrap();
        assert_eq!(
            hold.why, "dev shell warming (15m); attempt 1 stopped at 600 s, going on",
            "since the first attempt"
        );
        assert!(gate.due(31), "one that died is started again; a live one holds the lock");
        events.push(st(40, DevShellEvent::Warmed { key: k.clone() }));
        let w = World::fold(&events);
        assert_eq!(w.devshell.as_ref().map(|s| (s.ready_at, s.failures, s.warming_since)), Some((Some(40), 0, None)));
        // Ready by the bus but nothing on disk (removed by hand): warmed again, never started without it.
        assert!(Gate::of(&ctx, &w).unwrap().hold(41).is_some());
        // Another key's events say nothing of this one.
        let other = vec![st(50, DevShellEvent::Warmed { key: "old".into() })];
        assert_eq!(Gate::of(&ctx, &World::fold(&other)).unwrap().state, None);
    }
}
