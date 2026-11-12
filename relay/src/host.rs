//! What relay knows of this machine without asking whoever called it: where each harness's
//! program is, and the environment everything relay starts on its own runs in.
//!
//! A caller's shell is not a machine fact. The first shell to wake the dispatcher used to choose
//! for every job after it: its `PATH` decided which harnesses existed, and its variables (a `TZ`,
//! a `GIT_DIR`, a `RELAY_POLICY`) reached the dispatcher and every session. Now:
//!
//! - a harness's program is `[harness.X] program`, else the absolute path recorded the first time
//!   relay found it on a `PATH` ([`resolve`]); a configured harness with neither is
//!   [`Program::Missing`], and shown as such;
//! - what relay starts unattended (the dispatcher, its runners, gc, the quota reader, hooks and
//!   every session) runs in an [`Environ`], built from the config and this machine, never inherited.
//!
//! The caller's environment is read only where its choice is the point (which state root, which
//! project, the record above); `clippy.toml` refuses any other read.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

/// Where each program was found, by name: `programs.json` in relay's state root, which no session sees.
pub fn record_in(state_root: &Path) -> PathBuf {
    state_root.join("programs.json")
}

/// What a lookup may do to the record of where programs were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Record what `PATH` finds that the record lacks: under `programs.lock`, in `programs.json`. What
    /// the dispatcher and everything that starts a session does, so later callers find the same program.
    Record,
    /// The record is read, never locked or written: what `PATH` finds that it lacks is used, and not
    /// recorded. What a view that promises to write nothing does.
    ReadOnly,
}

/// A harness's program, as this machine has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A file this user may run.
    Found(PathBuf),
    /// What was looked for: a configured path, or a name neither recorded nor on `PATH`.
    Missing(OsString),
}

/// Whether `path` is a regular file (links followed) with an execute bit set.
pub fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program `name` names. An absolute path is taken as it is; a relative one (`bin/claude`) is
/// never found, since it would name a different file from every working directory (a config's
/// relative `program` is made absolute against its file's directory when it is loaded). A bare name is the
/// absolute path recorded at `record` while that can still run, else what relay's own `PATH`
/// finds, which is then recorded: a later caller whose `PATH` lacks it finds the same program,
/// and one whose `PATH` has another does not replace it. With no record (`None`), `PATH` alone.
///
/// A program found by name is never a mise shim, recorded or on `PATH`: a shim picks its version
/// from the mise config of whatever directory it runs in, and fails (`No version is set for
/// shim`) where none pins it, such as a session's clone or relay's state directory. The real
/// binary an activated mise put on the caller's `PATH` is found and recorded instead; with only a
/// shim, the program is missing, and shown so. A path the config names is the owner's own
/// choice, and is taken as it is.
pub fn resolve(record: Option<&Path>, name: &OsStr) -> Program {
    resolve_in(Mode::Record, record, name)
}

/// As [`resolve`], recording only as `mode` allows.
pub fn resolve_in(mode: Mode, record: Option<&Path>, name: &OsStr) -> Program {
    let dirs = search_path().unwrap_or_default();
    resolve_as(mode, record, name, &executable, &|p: &Path| executable(p) && !mise_shim(p), &dirs)
}

/// A harness's helper ([`crate::harness::Harness::helpers`]: the interpreter its launcher's
/// `#!/usr/bin/env` line names) as [`resolve`] finds a program, except that a mise shim is never
/// the answer, recorded or on `PATH`: a shim picks its version from the mise config of whatever
/// directory it runs in, and fails (`No version is set for shim`) where none pins it, such as a
/// session's clone or relay's state directory. The real binary an activated mise put on the
/// caller's `PATH` (`~/.local/share/mise/installs/bun/<v>/bin/bun`) is found and recorded instead;
/// with only a shim, the helper is missing, and shown so.
pub fn resolve_helper(record: Option<&Path>, name: &OsStr) -> Program {
    resolve_helper_in(Mode::Record, record, name)
}

/// As [`resolve_helper`], recording only as `mode` allows.
pub fn resolve_helper_in(mode: Mode, record: Option<&Path>, name: &OsStr) -> Program {
    let dirs = search_path().unwrap_or_default();
    resolve_as(mode, record, name, &helper_runs(name), &helper_runs(name), &dirs)
}

/// Whether `p` can serve as the helper `name`: a runnable file of that name, never a mise shim.
/// A launcher finds its helper by name (`#!/usr/bin/env node`) in the recorded file's directory, so
/// a recorded file under another name (`/opt/node/node-v22`) could not serve it: it is passed over.
fn helper_runs(name: &OsStr) -> impl Fn(&Path) -> bool {
    move |p: &Path| executable(p) && !mise_shim(p) && p.file_name() == Some(name)
}

/// Whether `path` is a mise shim: a link to the `mise` executable under another name.
pub fn mise_shim(path: &Path) -> bool {
    let named = |p: &Path| p.file_name().is_some_and(|n| n == "mise");
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
        && !named(path)
        && std::fs::canonicalize(path).is_ok_and(|t| named(&t))
}

/// The program `name` names: `explicit` judges a path the owner named (their own choice), and
/// `discovered` judges what is found by name (the record, then `dirs`), so the two can differ.
fn resolve_as(
    mode: Mode,
    record: Option<&Path>,
    name: &OsStr,
    explicit: &dyn Fn(&Path) -> bool,
    discovered: &dyn Fn(&Path) -> bool,
    dirs: &[PathBuf],
) -> Program {
    let as_path = Path::new(name);
    if name.is_empty() || as_path.components().count() != 1 {
        let found = !name.is_empty() && as_path.is_absolute() && explicit(as_path);
        return if found { Program::Found(as_path.to_path_buf()) } else { Program::Missing(name.to_owned()) };
    }
    let key = name.to_string_lossy().into_owned();
    if let Some(p) = recorded(&record.map(read).unwrap_or_default(), &key, discovered) {
        return Program::Found(p);
    }
    let Some(found) = on_path_in(dirs, name, discovered) else { return Program::Missing(name.to_owned()) };
    if mode == Mode::ReadOnly {
        return Program::Found(found);
    }
    // A record that cannot be had or written is found again next time.
    match record.and_then(Held::take) {
        Some(held) => held.record(key, found, discovered),
        None => Program::Found(found),
    }
}

/// What the record says of `key`, if it is a program that can still run (and that `runs` accepts).
fn recorded(known: &BTreeMap<String, PathBuf>, key: &str, runs: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    known.get(key).filter(|p| p.is_absolute() && runs(p)).cloned()
}

/// The record, read under its lock (`programs.lock` beside it) and held until written: the one way
/// to change it, so two relays that each found a program cannot drop the other's.
#[derive(Debug)]
struct Held {
    _lock: crate::os::Role,
    path: PathBuf,
    known: BTreeMap<String, PathBuf>,
}

impl Held {
    /// The record at `path`, locked once every other holder is done; `None` when its state root is not
    /// there (a view outside every project creates none) or its lock cannot be opened or taken.
    ///
    /// It waits as long as it must: a holder only reads and rewrites this small file, never waiting on
    /// anything while it holds it, so the wait ends; a waiter that gave up instead lost its discovery
    /// whenever writers queued behind a loaded machine longer than it waited.
    fn take(path: &Path) -> Option<Self> {
        if !path.parent().is_some_and(Path::is_dir) {
            return None;
        }
        let role = crate::os::Role::take(&path.with_extension("lock")).ok()?;
        Some(Self { _lock: role, path: path.to_path_buf(), known: read(path) })
    }

    /// Records `found` for `key`, unless another relay recorded a program that can run since this
    /// one looked: then that one stands, and is the answer.
    fn record(mut self, key: String, found: PathBuf, runs: &dyn Fn(&Path) -> bool) -> Program {
        if let Some(p) = recorded(&self.known, &key, runs) {
            return Program::Found(p);
        }
        self.known.insert(key, found.clone());
        let _ = self.write();
        Program::Found(found)
    }

    /// Replaces the record whole (a new file renamed over it), so a reader never sees half of one.
    fn write(&self) -> anyhow::Result<()> {
        let tmp = self.path.with_extension(format!("json.{}", std::process::id()));
        crate::ctx::Ctx::write_private(&tmp, &serde_json::to_vec_pretty(&self.known)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Where `name` is in `dirs`, in order, the first that `runs` accepts.
fn on_path_in(dirs: &[PathBuf], name: &OsStr, runs: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| runs(p))
}

/// The directories relay's own `PATH` names, the one read of the caller's `PATH`. Relative
/// entries are left out (they would name a different file from every other directory).
#[allow(clippy::disallowed_methods, reason = "the one read of a caller's PATH: what it finds is recorded")]
fn search_path() -> Option<Vec<PathBuf>> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).filter(|d| d.is_absolute()).collect())
}

fn read(record: &Path) -> BTreeMap<String, PathBuf> {
    std::fs::read(record).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The variables that say which relay a process is: whose home, which state and data roots, which
/// policy, which project. relay's own children must keep the ones of the relay that starts them,
/// whatever `session.env` says; a session may be given others.
pub const SELECTORS: [&str; 5] = ["HOME", "XDG_STATE_HOME", "XDG_DATA_HOME", "RELAY_POLICY", "RELAY_CONFIG"];

/// What relay sets in every environment it builds, whoever woke it: the user and home this process
/// runs as, the state and data roots it serves, the policy its config was merged with, a `PATH`
/// from this machine ([`default_path`]) unless `session.env` names one, and a UTF-8 locale. Nothing
/// of the caller's: no `TZ`, no `GIT_DIR`, no tokens.
fn base(cfg: &Config, state_root: &Path, data_root: &Path) -> BTreeMap<OsString, OsString> {
    let home = crate::accounts::home();
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();
    let mut set = |k: &str, v: &OsStr| env.insert(k.into(), v.to_owned());
    set("HOME", home.as_os_str());
    if let Some((user, shell)) = account(rustix::process::getuid().as_raw()) {
        set("USER", user.as_ref());
        set("LOGNAME", user.as_ref());
        set("SHELL", shell.as_ref());
    }
    set("LANG", "C.UTF-8".as_ref());
    for (var, root) in [("XDG_STATE_HOME", state_root), ("XDG_DATA_HOME", data_root)] {
        if let Some(dir) = root.parent() {
            set(var, dir.as_os_str());
        }
    }
    // Named only when it is not where every process would look anyway, so a policy file
    // deleted later stays optional.
    if let Some(p) = cfg.policy_path.as_ref().filter(|p| **p != crate::config::Policy::default_in(&home)) {
        set("RELAY_POLICY", p.as_os_str());
    }
    if !cfg.session.env.contains_key("PATH") {
        set("PATH", &default_path(cfg, &home));
    }
    env
}

/// A `session.env` value with relay's two directory names filled in: `${RELAY_STATE}` is this project's
/// state directory (where a check's checkout is made) and `${RELAY_CLONES}` its clones directory, so
/// a variable can name them without the home path. Nothing else is expanded: relay does not read the
/// shell's `${...}`, and the caller's variables are not its to give.
pub fn expand(value: &str, cfg: &Config, state_root: &Path, data_root: &Path) -> OsString {
    let (state, clones) = (state_root.join(&cfg.name), data_root.join(&cfg.name).join("clones"));
    let mut out = OsString::new();
    let mut rest = value;
    while let Some(at) = rest.find("${RELAY_") {
        out.push(&rest[..at]);
        rest = &rest[at..];
        let (name, dir) = if rest.starts_with("${RELAY_STATE}") {
            ("${RELAY_STATE}", &state)
        } else if rest.starts_with("${RELAY_CLONES}") {
            ("${RELAY_CLONES}", &clones)
        } else {
            out.push("${RELAY_");
            rest = &rest["${RELAY_".len()..];
            continue;
        };
        out.push(dir);
        rest = &rest[name.len()..];
    }
    out.push(rest);
    out
}

fn without(env: &BTreeMap<OsString, OsString>, deny: &regex::Regex) -> BTreeMap<OsString, OsString> {
    env.iter().filter(|(k, _)| !deny.is_match(&k.to_string_lossy())).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The environment of relay's own children (the dispatcher, its runners, gc, the quota reader, and
/// `command:` hooks, which may call relay back): [`base`], then `session.env` except the
/// [`SELECTORS`], which stay this relay's, so a child always serves the queue its parent wrote to.
/// The only environment [`RelayEnv::command`] starts anything in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnv(BTreeMap<OsString, OsString>);

/// Which relay a process is: the project config it serves and its state and data roots. Every way
/// relay starts relay takes them from one of these ([`crate::ctx::Ctx::selectors`]): `--config`, and
/// `RELAY_CONFIG`, `XDG_STATE_HOME` and `XDG_DATA_HOME` in [`RelayEnv`], so a child, or a hook that
/// calls relay back without `--config`, reaches the same project and queue.
#[derive(Debug, Clone, Copy)]
pub struct Selectors<'a> {
    pub config: &'a Path,
    pub state_root: &'a Path,
    pub data_root: &'a Path,
}

impl RelayEnv {
    /// [`base`] for `cfg`, `session.env` over it except the [`SELECTORS`], then `RELAY_CONFIG` as
    /// `which` names it.
    pub fn new(cfg: &Config, which: Selectors<'_>) -> Self {
        let mut env = base(cfg, which.state_root, which.data_root);
        for (k, v) in cfg.session.env.iter().filter(|(k, _)| !SELECTORS.contains(&k.as_str())) {
            env.insert(k.into(), expand(v, cfg, which.state_root, which.data_root));
        }
        env.insert("RELAY_CONFIG".into(), which.config.into());
        Self(env)
    }

    /// Exactly these variables, for a test.
    #[cfg(test)]
    pub fn of<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    #[cfg(test)]
    pub fn get(&self, k: &str) -> Option<&OsString> {
        self.0.get(OsStr::new(k))
    }

    /// The same, less every variable whose name `deny` matches.
    pub fn without(&self, deny: &regex::Regex) -> Self {
        Self(without(&self.0, deny))
    }

    /// `program`, to run with this environment and nothing of the caller's (found on this `PATH`).
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear().envs(&self.0);
        c
    }

    /// The same, run under `relay supervise` ([`crate::supervised::command`]), which passes this
    /// environment on to `program`.
    pub fn supervised(&self, program: impl AsRef<OsStr>) -> std::io::Result<Command> {
        let mut c = crate::supervised::command(program)?;
        c.env_clear().envs(&self.0);
        Ok(c)
    }
}

/// The environment a session starts from (and a check, and a harness's quota reading): [`base`],
/// then every `session.env` entry, selectors included, since the job's own tools may want them.
/// It cannot start relay: it has no `command`, only variables for a sandbox or a harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEnv(BTreeMap<OsString, OsString>);

impl JobEnv {
    pub fn new(cfg: &Config, state_root: &Path, data_root: &Path) -> Self {
        let mut env = base(cfg, state_root, data_root);
        for (k, v) in &cfg.session.env {
            env.insert(k.into(), expand(v, cfg, state_root, data_root));
        }
        Self(env)
    }

    #[cfg(test)]
    pub fn get(&self, k: &str) -> Option<&OsString> {
        self.0.get(OsStr::new(k))
    }

    pub fn vars(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.0.iter()
    }

    /// The same, less every variable whose name `deny` matches.
    pub fn without(&self, deny: &regex::Regex) -> Self {
        Self(without(&self.0, deny))
    }
}

/// This user's name and login shell, from `/etc/passwd`.
fn account(uid: u32) -> Option<(String, String)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[2].parse() == Ok(uid)).then(|| (f[0].to_owned(), f[6].to_owned()))
    })
}

/// The `PATH` when `session.env` names none: relay's own directory, the user's tool directories
/// (`~/.local/bin`, `~/bin`, `~/.cargo/bin`, mise's shims, the Nix profile), the directory of each
/// found harness program and helper (for its interpreter: puku-cli runs on `node`), then the system's. Only those that exist, in that order.
pub fn default_path(cfg: &Config, home: &Path) -> OsString {
    let own = crate::os::own_binary().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let user = [".local/bin", "bin", ".cargo/bin", ".local/share/mise/shims", ".nix-profile/bin"].map(|d| home.join(d));
    let programs = cfg.harness_programs();
    let found: Vec<PathBuf> = programs
        .iter()
        .flat_map(crate::config::HarnessPrograms::all)
        .filter_map(|p| match p {
            Program::Found(p) => p.parent().map(Path::to_path_buf),
            Program::Missing(_) => None,
        })
        .collect();
    let harnesses = found.into_iter();
    let system = [
        "/nix/var/nix/profiles/default/bin",
        "/run/current-system/sw/bin",
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ]
    .map(PathBuf::from);
    path_of(own.into_iter().chain(user).chain(harnesses).chain(system).filter(|d| d.is_absolute() && d.is_dir()))
}

/// A `PATH` of `dirs` in order, without repeats. A directory whose name holds `:` cannot be written in a
/// Unix `PATH`, so it is left out, never the whole list with it.
pub(crate) fn path_of(dirs: impl IntoIterator<Item = PathBuf>) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    let mut kept: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !d.as_os_str().as_bytes().contains(&b':') && !kept.contains(&d) {
            kept.push(d);
        }
    }
    std::env::join_paths(kept).expect("no directory left in holds a ':'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn program(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn a_directory_a_path_cannot_hold_is_left_out_not_the_whole_path() {
        let dirs = ["/opt/omp:beta/bin", "/usr/bin", "/bin", "/usr/bin"].map(PathBuf::from);
        assert_eq!(path_of(dirs), OsString::from("/usr/bin:/bin"));
    }

    #[test]
    fn a_path_is_found_only_when_it_can_be_run() {
        let d = crate::config::tempdir::Dir::new();
        let run = program(d.path(), "run", 0o755);
        let plain = program(d.path(), "plain", 0o644);
        assert_eq!(resolve(None, run.as_os_str()), Program::Found(run.clone()));
        assert_eq!(resolve(None, plain.as_os_str()), Program::Missing(plain.clone().into()));
        assert_eq!(resolve(None, d.path().as_os_str()), Program::Missing(d.path().into()), "a directory");
        // A relative path names a different file from each working directory: never found.
        let rel = PathBuf::from(".").join(run.file_name().unwrap());
        assert_eq!(resolve(None, rel.as_os_str()), Program::Missing(rel.clone().into()));
        assert!(!executable(&plain));
    }

    #[test]
    fn a_recorded_program_stands_until_it_can_no_longer_run() {
        let d = crate::config::tempdir::Dir::new();
        let record = d.path().join("programs.json");
        let first = program(d.path(), "first", 0o755);
        let write = |path: &Path, known: BTreeMap<String, PathBuf>| Held { known, ..Held::take(path).unwrap() }.write();
        write(&record, BTreeMap::from([("omp-xyz".to_owned(), first.clone())])).unwrap();
        // Not on anyone's PATH: the record alone finds it.
        assert_eq!(resolve(Some(&record), OsStr::new("omp-xyz")), Program::Found(first.clone()));
        // Once it cannot run, it is not found (nor on PATH), and says what was looked for.
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(resolve(Some(&record), OsStr::new("omp-xyz")), Program::Missing("omp-xyz".into()));
        // A relative record (written by hand) is not trusted either.
        write(&record, BTreeMap::from([("omp-xyz".to_owned(), PathBuf::from("first"))])).unwrap();
        assert_eq!(resolve(Some(&record), OsStr::new("omp-xyz")), Program::Missing("omp-xyz".into()));
    }

    #[test]
    fn a_name_found_on_path_is_recorded_absolute() {
        let d = crate::config::tempdir::Dir::new();
        let record = d.path().join("programs.json");
        // `sh` is on every PATH a test runs under.
        let Program::Found(sh) = resolve(Some(&record), OsStr::new("sh")) else { panic!("no sh") };
        assert!(sh.is_absolute());
        assert_eq!(read(&record).get("sh"), Some(&sh));
        // Never into a state root that is not there.
        let nowhere = d.path().join("none/programs.json");
        assert!(matches!(resolve(Some(&nowhere), OsStr::new("sh")), Program::Found(_)));
        assert!(!nowhere.parent().unwrap().exists());
        assert_eq!(resolve(None, OsStr::new("no-such-program-xyz")), Program::Missing("no-such-program-xyz".into()));
    }

    /// A mise shim (a link to `mise` under the tool's name) is never a program and never a helper: it
    /// fails wherever no mise config pins the tool, which is where sessions run.
    #[test]
    fn a_helper_is_never_a_mise_shim() {
        let d = crate::config::tempdir::Dir::new();
        let mise = program(d.path(), "mise", 0o755);
        std::fs::create_dir(d.path().join("shims")).unwrap();
        let shim = d.path().join("shims/bun");
        std::os::unix::fs::symlink(&mise, &shim).unwrap();
        let real = program(d.path(), "bun", 0o755);
        std::fs::create_dir(d.path().join("linked")).unwrap();
        let linked = d.path().join("linked/bun");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert!(mise_shim(&shim) && !mise_shim(&mise) && !mise_shim(&real) && !mise_shim(&linked));
        let record = d.path().join("programs.json");
        std::fs::write(&record, serde_json::json!({ "bun": shim }).to_string()).unwrap();
        assert_ne!(resolve(Some(&record), OsStr::new("bun")), Program::Found(shim.clone()), "a program not");
        assert_ne!(resolve_helper(Some(&record), OsStr::new("bun")), Program::Found(shim.clone()), "a helper not");
        std::fs::write(&record, serde_json::json!({ "bun": linked }).to_string()).unwrap();
        assert_eq!(resolve_helper(Some(&record), OsStr::new("bun")), Program::Found(linked), "a link to a real one is");
    }

    /// A read-only lookup reads the record and finds on `PATH` as a recording one does, but never
    /// takes `programs.lock` or writes `programs.json`.
    #[test]
    fn a_read_only_lookup_uses_the_record_and_path_and_writes_nothing() {
        let d = crate::config::tempdir::Dir::new();
        let (bin, name) = (d.path().join("bin"), OsStr::new("relay-readonly-program-xyz"));
        std::fs::create_dir(&bin).unwrap();
        let on_path = program(&bin, name.to_str().unwrap(), 0o755);
        let record = d.path().join("programs.json");
        let dirs = [bin.clone()];
        let look = |mode| resolve_as(mode, Some(&record), name, &executable, &executable, &dirs);
        assert_eq!(look(Mode::ReadOnly), Program::Found(on_path.clone()), "found on PATH");
        assert!(!record.exists() && !record.with_extension("lock").exists(), "nothing written or locked");
        let recorded = program(d.path(), "recorded", 0o755);
        std::fs::write(&record, serde_json::json!({ name.to_str().unwrap(): recorded }).to_string()).unwrap();
        let before = std::fs::read(&record).unwrap();
        assert_eq!(look(Mode::ReadOnly), Program::Found(recorded), "the record first");
        assert_eq!(std::fs::read(&record).unwrap(), before);
        assert!(!record.with_extension("lock").exists());
        assert_eq!(look(Mode::Record), look(Mode::ReadOnly), "and a recording lookup agrees");
    }

    /// A mise shim is never a harness's program, recorded or on `PATH`: a shim fails wherever no
    /// mise config pins the tool, which is where sessions run. The real binary an activated mise
    /// put on `PATH` is found and recorded instead; with only a shim there, the program is missing.
    #[test]
    fn a_program_is_never_a_mise_shim() {
        let d = crate::config::tempdir::Dir::new();
        let mise = program(d.path(), "mise", 0o755);
        let (shims, real_dir) = (d.path().join("shims"), d.path().join("installs/bin"));
        std::fs::create_dir(&shims).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        // A name no `PATH` holds, so only what this test puts there is found.
        let name = OsStr::new("relay-shim-program-xyz");
        let key = name.to_str().unwrap();
        let shim = shims.join(name);
        std::os::unix::fs::symlink(&mise, &shim).unwrap();
        let real = program(&real_dir, key, 0o755);
        assert!(mise_shim(&shim) && !mise_shim(&real));
        let runs = |p: &Path| executable(p) && !mise_shim(p);
        let record = d.path().join("programs.json");
        // A shim in the record is passed over, and the record replaced by the real binary found.
        std::fs::write(&record, serde_json::json!({ key: shim }).to_string()).unwrap();
        let dirs = [shims.clone(), real_dir.clone()];
        let found = resolve_as(Mode::Record, Some(&record), name, &executable, &runs, &dirs);
        assert_ne!(found, Program::Found(shim.clone()), "a shim in the record not");
        assert_eq!(found, Program::Found(real.clone()), "the real one instead");
        assert_eq!(read(&record).get(key), Some(&real), "and the record holds it");
        // A shim on `PATH` is passed over too: the real binary behind it is found.
        assert_eq!(on_path_in(&dirs, name, &runs), Some(real.clone()), "the real one, past the shim");
        // With only a shim there, the program is missing, and nothing is recorded.
        let only = [shims.clone()];
        assert_eq!(on_path_in(&only, name, &runs), None, "a shim is not a program");
        let bare = d.path().join("bare.json");
        assert_eq!(
            resolve_as(Mode::Record, Some(&bare), name, &executable, &runs, &only),
            Program::Missing(name.into()),
            "with only a shim, the program is missing"
        );
        assert!(!bare.exists(), "and nothing is recorded");
        // A path the owner names is their own choice: a shim there is taken as it is.
        assert_eq!(
            resolve_as(Mode::Record, None, shim.as_os_str(), &executable, &runs, &only),
            Program::Found(shim.clone())
        );
    }

    /// Review of #40: a launcher finds its helper by name in the directory relay puts on its PATH, so
    /// a record of `node` as `/opt/node/node-v22` would pass discovery and still leave `#!/usr/bin/env
    /// node` without it. Such a record is passed over, never returned.
    #[test]
    fn a_helper_recorded_under_another_name_is_passed_over() {
        let d = crate::config::tempdir::Dir::new();
        let misnamed = program(d.path(), "node-v22", 0o755);
        let record = d.path().join("programs.json");
        std::fs::write(&record, serde_json::json!({ "node": misnamed }).to_string()).unwrap();
        assert_eq!(
            resolve(Some(&record), OsStr::new("node")),
            Program::Found(misnamed.clone()),
            "a program is run by path"
        );
        assert_ne!(resolve_helper(Some(&record), OsStr::new("node")), Program::Found(misnamed), "a helper not");
        std::fs::create_dir(d.path().join("named")).unwrap();
        let named = program(&d.path().join("named"), "node", 0o755);
        std::fs::write(&record, serde_json::json!({ "node": named }).to_string()).unwrap();
        assert_eq!(resolve_helper(Some(&record), OsStr::new("node")), Program::Found(named));
    }

    #[test]
    fn concurrent_discoveries_all_stay_in_the_record() {
        let common =
            ["sh", "cat", "ls", "env", "true", "false", "echo", "mkdir", "rm", "cp", "mv", "ln", "sort", "head"];
        let dirs = search_path().unwrap_or_default();
        let names: Vec<&str> =
            common.into_iter().filter(|n| on_path_in(&dirs, OsStr::new(n), &executable).is_some()).collect();
        assert!(names.len() >= 6, "too few programs on PATH to race: {names:?}");
        let d = crate::config::tempdir::Dir::new();
        for round in 0..20 {
            let record = d.path().join(format!("programs-{round}.json"));
            std::thread::scope(|s| {
                for n in &names {
                    let record = &record;
                    s.spawn(move || resolve(Some(record), OsStr::new(n)));
                }
            });
            let known = read(&record);
            let lost: Vec<&&str> = names.iter().filter(|n| !known.contains_key(**n)).collect();
            assert!(lost.is_empty(), "round {round}: a concurrent writer dropped {lost:?}");
        }
    }

    /// A discovery made while another relay holds the record (here past the 2 s a waiter used to give up
    /// after, as 13 writers each slowed by a loaded machine did) is recorded once it is free, never dropped.
    #[test]
    fn a_discovery_waits_for_the_record_however_long_it_is_held() {
        let d = crate::config::tempdir::Dir::new();
        let record = d.path().join("programs.json");
        let other = crate::os::Role::try_take(&record.with_extension("lock")).unwrap().unwrap();
        let resolving = std::thread::spawn({
            let record = record.clone();
            move || resolve(Some(&record), OsStr::new("sh"))
        });
        std::thread::sleep(std::time::Duration::from_millis(2500));
        assert!(!resolving.is_finished(), "a discovery gave up on a held record");
        drop(other);
        let Program::Found(sh) = resolving.join().unwrap() else { panic!("no sh") };
        assert_eq!(read(&record).get("sh"), Some(&sh));
    }

    /// Review of #40: a helper a harness runs (puku's `node`) in a directory of its own is recorded with
    /// the harness and its directory is on the `PATH` children get, so a sanitized child still runs it.
    #[test]
    fn a_harness_helper_is_recorded_and_its_directory_is_on_the_default_path() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let (cli_dir, node_dir) = (d.path().join("puku/bin"), d.path().join("opt node/bin"));
        std::fs::create_dir_all(&cli_dir).unwrap();
        std::fs::create_dir_all(&node_dir).unwrap();
        let cli = program(&cli_dir, "puku-cli", 0o755);
        let node = program(&node_dir, "node", 0o755);
        let record = d.path().join("programs.json");
        std::fs::write(&record, serde_json::json!({ "node": node }).to_string()).unwrap();
        let conf = format!("name = \"x\"\n[harness.puku]\nprogram = \"{}\"\n", cli.display());
        let mut cfg = Config::parse(&conf, d.path()).unwrap();
        cfg.programs = Some(record.clone());
        let puku = cfg.harness_programs().into_iter().find(|p| p.harness.id() == "puku").unwrap();
        assert_eq!(puku.helpers, [("node", Program::Found(node.clone()))]);
        assert!(puku.missing().is_none() && cfg.harnesses().iter().any(|h| h.id() == "puku"));
        let path = default_path(&cfg, d.path());
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert!(dirs.contains(&node_dir) && dirs.contains(&cli_dir), "{path:?}");
        // A helper that cannot be found leaves the harness shown as missing it, and unused.
        std::fs::remove_file(&node).unwrap();
        std::fs::write(&record, serde_json::json!({ "node": node }).to_string()).unwrap();
        let without = cfg.harness_programs().into_iter().find(|p| p.harness.id() == "puku").unwrap();
        if let (_, Program::Missing(_)) = &without.helpers[0] {
            let rows: Vec<_> = cfg.missing_harnesses().into_iter().filter(|(h, _)| *h == "puku").collect();
            assert_eq!(rows, [("puku", crate::config::Missing::Helper { name: "node" })]);
            assert!(!cfg.harnesses().iter().any(|h| h.id() == "puku"));
        }
    }

    #[test]
    fn the_environment_is_built_not_inherited() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"x\"\n[session.env]\nFROM_CONFIG = \"yes\"\n", d.path()).unwrap();
        let env = JobEnv::new(&cfg, Path::new("/s/relay"), Path::new("/d/relay"));
        let get = |k: &str| env.get(k).map(|v| v.to_string_lossy().into_owned());
        assert_eq!(get("FROM_CONFIG").as_deref(), Some("yes"));
        assert_eq!((get("XDG_STATE_HOME").as_deref(), get("XDG_DATA_HOME").as_deref()), (Some("/s"), Some("/d")));
        assert_eq!(get("HOME"), Some(crate::accounts::home().display().to_string()));
        let path = get("PATH").unwrap();
        assert!(path.split(':').any(|d| d == "/usr/bin"), "{path}");
        let names: Vec<String> = env.vars().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        let expected =
            ["FROM_CONFIG", "HOME", "LANG", "LOGNAME", "PATH", "SHELL", "USER", "XDG_DATA_HOME", "XDG_STATE_HOME"];
        assert!(names.iter().all(|n| expected.contains(&n.as_str())), "only what relay sets: {names:?}");
        let which = Selectors {
            config: Path::new("/p/relay.toml"),
            state_root: Path::new("/s/relay"),
            data_root: Path::new("/d/relay"),
        };
        let mut ours = RelayEnv::new(&cfg, which).0;
        assert_eq!(
            ours.remove(OsStr::new("RELAY_CONFIG")),
            Some("/p/relay.toml".into()),
            "relay's own know their project"
        );
        assert_eq!(ours, env.0, "and otherwise the same, here");
        // session.env wins, PATH included.
        let cfg =
            Config::parse("name = \"x\"\n[session.env]\nPATH = \"/only\"\nLANG = \"en_GB.UTF-8\"\n", d.path()).unwrap();
        let env = JobEnv::new(&cfg, Path::new("/s/relay"), Path::new("/d/relay"));
        assert_eq!(env.get("PATH"), Some(&OsString::from("/only")));
        assert_eq!(env.get("LANG"), Some(&OsString::from("en_GB.UTF-8")));
        let deny = regex::Regex::new("^PATH$").unwrap();
        assert!(env.without(&deny).get("PATH").is_none());
    }

    #[test]
    fn relays_own_children_keep_its_selectors_whatever_session_env_says() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = "name = \"x\"\n[session.env]\nHOME = \"/job\"\nXDG_STATE_HOME = \"/job-state\"\n\
                    XDG_DATA_HOME = \"/job-data\"\nRELAY_POLICY = \"/job.toml\"\nRELAY_CONFIG = \"/job/relay.toml\"\n\
                    PATH = \"/job/bin\"\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        let (state, data) = (Path::new("/s/relay"), Path::new("/d/relay"));
        let ours =
            RelayEnv::new(&cfg, Selectors { config: Path::new("/p/relay.toml"), state_root: state, data_root: data });
        let job = JobEnv::new(&cfg, state, data);
        let home = crate::accounts::home();
        assert_eq!(ours.get("HOME"), Some(&home.into_os_string()));
        assert_eq!((ours.get("XDG_STATE_HOME"), ours.get("XDG_DATA_HOME")), (Some(&"/s".into()), Some(&"/d".into())));
        assert_eq!((ours.get("RELAY_POLICY"), ours.get("RELAY_CONFIG")), (None, Some(&"/p/relay.toml".into())));
        assert_eq!(ours.get("PATH"), Some(&"/job/bin".into()), "not a selector: the config's PATH runs relay too");
        for k in SELECTORS {
            assert!(job.get(k).is_some_and(|v| v.to_string_lossy().starts_with("/job")), "the job gets its {k}");
        }
    }

    /// Tickify: mise's shims refuse a clone's `mise.toml` unless `MISE_TRUSTED_CONFIG_PATHS` names it, and
    /// `[session] env` cannot name the project's own directories without hardcoding the home path.
    #[test]
    fn session_env_can_name_the_projects_own_state_and_clone_directories() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let text = "name = \"x\"\n[session.env]\nMISE_TRUSTED_CONFIG_PATHS = \"${RELAY_STATE}:${RELAY_CLONES}\"\n\
                    OTHER = \"${HOME}/${RELAY_STATE}x\"\n";
        let cfg = Config::parse(text, d.path()).unwrap();
        let (state, data) = (Path::new("/s/relay"), Path::new("/d/relay"));
        let job = JobEnv::new(&cfg, state, data);
        let ours =
            RelayEnv::new(&cfg, Selectors { config: Path::new("/p/relay.toml"), state_root: state, data_root: data });
        let want = OsString::from("/s/relay/x:/d/relay/x/clones");
        assert_eq!(job.get("MISE_TRUSTED_CONFIG_PATHS"), Some(&want));
        assert_eq!(ours.get("MISE_TRUSTED_CONFIG_PATHS"), Some(&want));
        assert_eq!(
            job.get("OTHER"),
            Some(&OsString::from("${HOME}//s/relay/xx")),
            "only relay's two names are expanded"
        );
    }
}
