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
pub fn resolve(record: Option<&Path>, name: &OsStr) -> Program {
    let as_path = Path::new(name);
    if name.is_empty() || as_path.components().count() != 1 {
        let found = !name.is_empty() && as_path.is_absolute() && executable(as_path);
        return if found { Program::Found(as_path.to_path_buf()) } else { Program::Missing(name.to_owned()) };
    }
    let key = name.to_string_lossy().into_owned();
    if let Some(p) = recorded(&record.map(read).unwrap_or_default(), &key) {
        return Program::Found(p);
    }
    let Some(found) = on_path(name) else { return Program::Missing(name.to_owned()) };
    // A record that cannot be had or written is found again next time.
    match record.and_then(Held::take) {
        Some(held) => held.record(key, found),
        None => Program::Found(found),
    }
}

/// What the record says of `key`, if it is a program that can still run.
fn recorded(known: &BTreeMap<String, PathBuf>, key: &str) -> Option<PathBuf> {
    known.get(key).filter(|p| p.is_absolute() && executable(p)).cloned()
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
    /// How long to wait for another relay's write, before leaving the record as it is.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

    /// The record at `path`, locked; `None` when its state root is not there (a view outside every
    /// project creates none) or another relay holds it past [`Held::PATIENCE`].
    fn take(path: &Path) -> Option<Self> {
        let lock = path.with_extension("lock");
        if !path.parent().is_some_and(Path::is_dir) {
            return None;
        }
        let started = std::time::Instant::now();
        loop {
            match crate::os::Role::try_take(&lock) {
                Ok(Some(role)) => return Some(Self { _lock: role, path: path.to_path_buf(), known: read(path) }),
                Ok(None) if started.elapsed() < Self::PATIENCE => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                _ => return None,
            }
        }
    }

    /// Records `found` for `key`, unless another relay recorded a program that can run since this
    /// one looked: then that one stands, and is the answer.
    fn record(mut self, key: String, found: PathBuf) -> Program {
        if let Some(p) = recorded(&self.known, &key) {
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

/// Where `name` is on relay's own `PATH`, skipping relative entries (they would name a
/// different file from every other directory).
#[allow(clippy::disallowed_methods, reason = "the one read of a caller's PATH: what it finds is recorded")]
fn on_path(name: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| executable(p))
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
            env.insert(k.into(), v.into());
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
            env.insert(k.into(), v.into());
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

    #[test]
    fn concurrent_discoveries_all_stay_in_the_record() {
        let common =
            ["sh", "cat", "ls", "env", "true", "false", "echo", "mkdir", "rm", "cp", "mv", "ln", "sort", "head"];
        let names: Vec<&str> = common.into_iter().filter(|n| on_path(OsStr::new(n)).is_some()).collect();
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
        if let (_, Program::Missing(what)) = &without.helpers[0] {
            assert_eq!(cfg.missing_harnesses(), [("puku", what.to_string_lossy().into_owned())]);
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
}
