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

/// The program `name` names. A path (anything with a `/`) is taken as it is. A bare name is the
/// absolute path recorded at `record` while that can still run, else what relay's own `PATH`
/// finds, which is then recorded: a later caller whose `PATH` lacks it finds the same program,
/// and one whose `PATH` has another does not replace it. With no record (`None`), `PATH` alone.
pub fn resolve(record: Option<&Path>, name: &OsStr) -> Program {
    let as_path = Path::new(name);
    if name.is_empty() || as_path.components().count() != 1 {
        let found = !name.is_empty() && executable(as_path);
        return if found { Program::Found(as_path.to_path_buf()) } else { Program::Missing(name.to_owned()) };
    }
    let key = name.to_string_lossy().into_owned();
    let mut known = record.map(read).unwrap_or_default();
    if let Some(p) = known.get(&key).filter(|p| p.is_absolute() && executable(p)) {
        return Program::Found(p.clone());
    }
    let Some(found) = on_path(name) else { return Program::Missing(name.to_owned()) };
    if let Some(record) = record {
        known.insert(key, found.clone());
        // A record that cannot be written is found again next time.
        let _ = write(record, &known);
    }
    Program::Found(found)
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

/// Replaces the record whole (a new file renamed over it), so a reader never sees half of one. Only
/// into a state root that is there: a view outside every project creates none.
fn write(record: &Path, known: &BTreeMap<String, PathBuf>) -> anyhow::Result<()> {
    anyhow::ensure!(record.parent().is_some_and(Path::is_dir), "no state root to record in");
    let tmp = record.with_extension(format!("json.{}", std::process::id()));
    crate::ctx::Ctx::write_private(&tmp, &serde_json::to_vec_pretty(known)?)?;
    std::fs::rename(&tmp, record)?;
    Ok(())
}

/// The environment relay gives what it starts on its own, whoever woke it: the user and home this
/// process runs as, the state and data roots it serves, the policy its config was merged with, a
/// `PATH` from this machine ([`default_path`]), a UTF-8 locale, and `session.env` over all of
/// them. Nothing else of the caller's: no `TZ`, no `GIT_DIR`, no tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environ(BTreeMap<OsString, OsString>);

impl Environ {
    pub fn new(cfg: &Config, state_root: &Path, data_root: &Path) -> Self {
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
        for (k, v) in &cfg.session.env {
            set(k, v.as_ref());
        }
        Self(env)
    }

    /// Exactly these variables, for a test.
    #[cfg(test)]
    pub fn of<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    pub fn vars(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.0.iter()
    }

    /// The same, less every variable whose name `deny` matches.
    pub fn without(&self, deny: &regex::Regex) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(k, _)| !deny.is_match(&k.to_string_lossy()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }

    /// `program`, to run with this environment and nothing of the caller's (found on this `PATH`).
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear().envs(&self.0);
        c
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
/// (`~/.local/bin`, `~/bin`, `~/.cargo/bin`, mise's shims, the Nix profile), each found harness
/// program's directory (for its interpreter), then the system's. Only those that exist, in that order.
pub fn default_path(cfg: &Config, home: &Path) -> OsString {
    let own = crate::os::own_binary().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let user = [".local/bin", "bin", ".cargo/bin", ".local/share/mise/shims", ".nix-profile/bin"].map(|d| home.join(d));
    let harnesses = cfg.harness_programs().into_iter().filter_map(|(_, p)| match p {
        Program::Found(p) => p.parent().map(Path::to_path_buf),
        Program::Missing(_) => None,
    });
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
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in own.into_iter().chain(user).chain(harnesses).chain(system) {
        if d.is_absolute() && d.is_dir() && !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
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
    fn a_path_is_found_only_when_it_can_be_run() {
        let d = crate::config::tempdir::Dir::new();
        let run = program(d.path(), "run", 0o755);
        let plain = program(d.path(), "plain", 0o644);
        assert_eq!(resolve(None, run.as_os_str()), Program::Found(run));
        assert_eq!(resolve(None, plain.as_os_str()), Program::Missing(plain.clone().into()));
        assert_eq!(resolve(None, d.path().as_os_str()), Program::Missing(d.path().into()), "a directory");
        assert!(!executable(&plain));
    }

    #[test]
    fn a_recorded_program_stands_until_it_can_no_longer_run() {
        let d = crate::config::tempdir::Dir::new();
        let record = d.path().join("programs.json");
        let first = program(d.path(), "first", 0o755);
        write(&record, &BTreeMap::from([("omp-xyz".to_owned(), first.clone())])).unwrap();
        // Not on anyone's PATH: the record alone finds it.
        assert_eq!(resolve(Some(&record), OsStr::new("omp-xyz")), Program::Found(first.clone()));
        // Once it cannot run, it is not found (nor on PATH), and says what was looked for.
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(resolve(Some(&record), OsStr::new("omp-xyz")), Program::Missing("omp-xyz".into()));
        // A relative record (written by hand) is not trusted either.
        write(&record, &BTreeMap::from([("omp-xyz".to_owned(), PathBuf::from("first"))])).unwrap();
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
    fn the_environment_is_built_not_inherited() {
        let d = crate::config::tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"x\"\n[session.env]\nFROM_CONFIG = \"yes\"\n", d.path()).unwrap();
        let env = Environ::new(&cfg, Path::new("/s/relay"), Path::new("/d/relay"));
        let get = |k: &str| env.0.get(OsStr::new(k)).map(|v| v.to_string_lossy().into_owned());
        assert_eq!(get("FROM_CONFIG").as_deref(), Some("yes"));
        assert_eq!((get("XDG_STATE_HOME").as_deref(), get("XDG_DATA_HOME").as_deref()), (Some("/s"), Some("/d")));
        assert_eq!(get("HOME"), Some(crate::accounts::home().display().to_string()));
        let path = get("PATH").unwrap();
        assert!(path.split(':').any(|d| d == "/usr/bin"), "{path}");
        let names: Vec<String> = env.0.keys().map(|k| k.to_string_lossy().into_owned()).collect();
        let expected =
            ["FROM_CONFIG", "HOME", "LANG", "LOGNAME", "PATH", "SHELL", "USER", "XDG_DATA_HOME", "XDG_STATE_HOME"];
        assert!(names.iter().all(|n| expected.contains(&n.as_str())), "only what relay sets: {names:?}");
        // session.env wins, PATH included.
        let cfg =
            Config::parse("name = \"x\"\n[session.env]\nPATH = \"/only\"\nLANG = \"en_GB.UTF-8\"\n", d.path()).unwrap();
        let env = Environ::new(&cfg, Path::new("/s/relay"), Path::new("/d/relay"));
        assert_eq!(env.0.get(OsStr::new("PATH")), Some(&OsString::from("/only")));
        assert_eq!(env.0.get(OsStr::new("LANG")), Some(&OsString::from("en_GB.UTF-8")));
        let deny = regex::Regex::new("^PATH$").unwrap();
        assert!(!env.without(&deny).0.contains_key(OsStr::new("PATH")));
    }
}
