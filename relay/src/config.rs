//! `relay.toml`: everything project-specific lives here, so the binary works
//! for any repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

pub const FILE_NAME: &str = "relay.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Names this project's queue. State lives under `$XDG_STATE_HOME/relay/<name>`.
    pub name: String,
    /// The repository sessions work on, relative to the config file.
    #[serde(default = "dot")]
    pub repo: PathBuf,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub accounts: Accounts,
    /// Per-harness settings, keyed by harness id (`claude`, `omp`).
    #[serde(default, rename = "harness")]
    pub harnesses: BTreeMap<String, HarnessConf>,
    #[serde(default)]
    pub session: Session,
    #[serde(default)]
    pub sandbox: Sandbox,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Sessions running at once, over all accounts.
    pub max_running: usize,
    /// Sessions running at once on one account.
    pub per_account: usize,
    /// An account takes no new session once any quota window reaches this utilization.
    pub floor: f64,
    /// Default spend cap per session, passed as `--max-budget-usd`.
    pub budget_usd: f64,
    /// A session that prints nothing for this long is ended.
    pub silent_minutes: u64,
    /// No new session starts while the 1-minute load average is above this.
    pub max_load: Option<f64>,
    /// No new build session (each makes a clone) starts below this much free disk.
    pub min_free_disk_gb: f64,
    /// Automatic retries of one job for empty or silent deaths, before it is left failed.
    pub max_retries: u32,
    /// At a spend cap, the next run's cap is the last one times this, after a
    /// progress check. `1.0` (the default) never resumes on its own.
    pub budget_growth: f64,
    /// The most a job may spend over all its runs and checks. Absent: no bound but the checks.
    pub job_ceiling_usd: Option<f64>,
    /// What judges progress at a spend cap.
    pub checker: Checker,
}

/// The progress check's harness, model and spend cap.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Checker {
    pub harness: String,
    /// Default: the harness's configured model.
    pub model: Option<String>,
    pub budget_usd: f64,
}

impl Default for Checker {
    fn default() -> Self {
        Self { harness: "claude".into(), model: None, budget_usd: 1.0 }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_running: 2,
            per_account: 1,
            floor: 0.9,
            budget_usd: 10.0,
            silent_minutes: 30,
            max_load: None,
            min_free_disk_gb: 10.0,
            max_retries: 1,
            budget_growth: 1.0,
            job_ceiling_usd: None,
            checker: Checker::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Accounts {
    /// Accounts never used, as `harness:label` (`claude:work`, `omp:default`).
    pub exclude: Vec<String>,
}

/// One harness's settings. Every harness takes the same keys.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HarnessConf {
    /// Default: on when the harness is installed (or `program` names it).
    pub enabled: Option<bool>,
    /// The command to run instead of the harness's own name. Tests use a stub.
    pub program: Option<PathBuf>,
    /// The model for jobs that name none for this harness.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Extra arguments for every session of this harness.
    pub extra_args: Vec<String>,
}

impl HarnessConf {
    /// The invocation, run by the configured program when one is set, else by
    /// the harness as relay's own `PATH` finds it. A project's `env_shell` must
    /// not pick the harness: TheGrid's devshell once put a stale `nixpkgs` claude
    /// first, on which `opus` meant an older model and current ids were refused.
    pub fn locate(&self, mut inv: crate::harness::Invocation) -> crate::harness::Invocation {
        if let Some(p) = &self.program {
            inv.program = p.clone().into();
        } else if let Some(p) = crate::os::on_path(&inv.program) {
            inv.program = p.into();
        }
        inv
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Session {
    /// Appended to the system prompt. `{branch}` and `{workspace}` are filled in.
    pub briefing: String,
    /// Build jobs work on `<branch_prefix><job>` in a private clone.
    pub branch_prefix: String,
    /// A result without this text gets one automatic follow-up asking for it.
    /// Empty turns the check off.
    pub final_marker: String,
    /// Model-name regex -> reason. Matching models are refused at queue and at start.
    pub banned_models: std::collections::BTreeMap<String, String>,
    /// Hard limit on one attempt's wall-clock time.
    pub max_hours: f64,
    /// CPU niceness for sessions, so interactive work stays responsive.
    pub nice: i32,
    /// A command prefix that enters the project's development environment,
    /// such as `["nix", "develop", "-c"]`. Such a command usually cannot run
    /// inside the sandbox (single-user Nix locks its store), so relay runs
    /// `env` through it on the host before each attempt and passes the result
    /// in, as direnv would. Empty: the caller's environment as it is.
    pub env_shell: Vec<String>,
}

impl Session {
    pub fn banned(&self, model: &str) -> Option<&str> {
        self.banned_models
            .iter()
            .find(|(re, _)| regex::Regex::new(re).is_ok_and(|r| r.is_match(model)))
            .map(|(_, why)| why.as_str())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self {
            briefing: DEFAULT_BRIEFING.into(),
            branch_prefix: "relay/".into(),
            final_marker: "FINAL".into(),
            env_shell: Vec::new(),
            banned_models: std::collections::BTreeMap::new(),
            max_hours: 4.0,
            nice: 10,
        }
    }
}

const DEFAULT_BRIEFING: &str = "You are an unattended session started by relay. Nobody is watching, so \
do not ask questions: take the safe, reversible default and record it under decided. {workspace} \
Other workspaces, other accounts and credentials are not mounted; do not look for them. Read only what \
the task needs, starting from the files the brief names; do not survey the repository. End with a short \
section, at most about 40 lines, that never pastes command output, in this shape:

FINAL
status: done | partial | blocked
commits: <short sha> <subject> (one per line, or none)
checked: <command> -> pass | fail (one per line, no output)
decided: <choice made without asking> (one per line, or none)
found: <file:line> <problem> (one per line, or none)
notes: <at most 5 lines, only what the reader must know>";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Sandbox {
    /// Run sessions under bubblewrap. Without it a session can read and write
    /// anything you can, so it is refused unless this is explicitly false.
    pub enabled: bool,
    /// Directories shown empty inside the sandbox. The repository is mounted
    /// back read-only on top. Defaults to the repository's parent directory.
    pub hide: Option<Vec<PathBuf>>,
    /// Paths under `$HOME` hidden from sessions.
    pub hide_home: Vec<PathBuf>,
    /// Paths under `$HOME` sessions get as writable caches. Each is relay's own
    /// directory for the project, mounted there, never the host's: a session
    /// that could write the host's `~/.cargo` could replace what the host runs.
    pub caches: Vec<PathBuf>,
    /// Environment variables whose names match this regex are not passed in.
    pub env_deny: String,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            enabled: true,
            hide: None,
            hide_home: [
                ".ssh",
                ".gnupg",
                ".config/gh",
                ".git-credentials",
                ".netrc",
                ".aws",
                ".config/gcloud",
                ".docker",
                ".npmrc",
                ".cargo/credentials",
                ".cargo/credentials.toml",
                // Not all of ~/.local/state: it holds the Nix profile and mise's trusted
                // configs, which a project's toolchain needs. relay's own state there is
                // hidden by its own layer.
                ".local/state/gh",
                ".local/state/claude",
                ".omp/ssh-control",
            ]
            .map(PathBuf::from)
            .to_vec(),
            caches: [".cache", ".cargo/registry", ".cargo/git"].map(PathBuf::from).to_vec(),
            env_deny: DEFAULT_ENV_DENY.into(),
        }
    }
}

/// A finite amount above zero, such as a spend cap.
pub fn positive(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// `AUTH` only as a whole word, so `GH_AUTH` and `SSH_AUTH_SOCK` go but `GIT_AUTHOR_NAME` stays.
const DEFAULT_ENV_DENY: &str =
    "(?i)TOKEN|SECRET|PASSW|CREDENTIAL|API_?KEY|_KEY$|(^|_)AUTH($|_)|COOKIE|SESSION|^CLAUDE|^ANTHROPIC";

#[cfg(test)]
#[test]
fn default_env_deny() {
    let re = regex::Regex::new(DEFAULT_ENV_DENY).unwrap();
    for gone in ["GITHUB_TOKEN", "GH_AUTH", "SSH_AUTH_SOCK", "AWS_SECRET_ACCESS_KEY", "OPENAI_API_KEY", "CLAUDECODE"] {
        assert!(re.is_match(gone), "{gone}");
    }
    for kept in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "PATH", "HOME", "LANG", "KEYBOARD"] {
        assert!(!re.is_match(kept), "{kept}");
    }
}

fn dot() -> PathBuf {
    PathBuf::from(".")
}

impl Config {
    pub fn parse(text: &str, dir: &Path) -> Result<Self> {
        let mut cfg: Self = toml::from_str(text)?;
        cfg.repo = dir.join(&cfg.repo).canonicalize().with_context(|| format!("repo {}", cfg.repo.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// `--config`, else `$RELAY_CONFIG`, else the nearest `relay.toml` at or above the current directory.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match explicit.map(Path::to_path_buf).or_else(|| std::env::var_os("RELAY_CONFIG").map(PathBuf::from))
        {
            Some(p) => p,
            None => find_upwards(&std::env::current_dir()?)
                .with_context(|| format!("no {FILE_NAME} here or above; pass --config"))?,
        };
        let path = path.canonicalize().with_context(|| format!("config {}", path.display()))?;
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg = Self::parse(&text, path.parent().unwrap_or(Path::new("/")))
            .with_context(|| format!("in {}", path.display()))?;
        Ok((cfg, path))
    }

    fn validate(&self) -> Result<()> {
        ensure!(valid_name(&self.name), "name must be lowercase letters, digits and dashes");
        ensure!(self.repo.join(".git").exists(), "repo {} is not a git repository", self.repo.display());
        ensure!(self.limits.max_running >= 1 && self.limits.per_account >= 1, "limits must be at least 1");
        ensure!((0.0..=1.0).contains(&self.limits.floor), "limits.floor is a fraction between 0 and 1");
        ensure!(positive(self.limits.budget_usd), "limits.budget_usd must be a positive number");
        ensure!(
            self.limits.budget_growth.is_finite() && self.limits.budget_growth >= 1.0,
            "limits.budget_growth must be at least 1"
        );
        ensure!(self.limits.job_ceiling_usd.is_none_or(positive), "limits.job_ceiling_usd must be a positive number");
        ensure!(positive(self.limits.checker.budget_usd), "limits.checker.budget_usd must be a positive number");
        crate::harness::by_id(&self.limits.checker.harness)
            .map_err(|e| anyhow::anyhow!("limits.checker.harness: {e}"))?;
        for id in self.harnesses.keys() {
            crate::harness::by_id(id).map_err(|e| anyhow::anyhow!("[harness.{id}]: {e}"))?;
        }
        for key in &self.accounts.exclude {
            ensure!(key.contains(':'), "accounts.exclude entries are harness:label, not {key:?}");
        }
        ensure!(self.session.max_hours > 0.0, "session.max_hours must be positive");
        for re in self.session.banned_models.keys() {
            regex::Regex::new(re).with_context(|| format!("session.banned_models key {re:?}"))?;
        }
        regex::Regex::new(&self.sandbox.env_deny).context("sandbox.env_deny")?;
        for p in self.sandbox.hide_home.iter().chain(&self.sandbox.caches) {
            if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("{} must be a path inside $HOME, written relative to it", p.display());
            }
        }
        Ok(())
    }

    /// The harnesses relay may use, in registry order.
    pub fn harnesses(&self) -> Vec<&'static dyn crate::harness::Harness> {
        crate::harness::ALL
            .iter()
            .copied()
            .filter(|h| {
                let conf = self.harnesses.get(h.id());
                conf.and_then(|c| c.enabled)
                    .unwrap_or_else(|| conf.and_then(|c| c.program.as_ref()).is_some() || crate::harness::installed(*h))
            })
            .collect()
    }

    pub fn harness(&self, id: &str) -> HarnessConf {
        self.harnesses.get(id).cloned().unwrap_or_default()
    }

    /// Directories hidden inside the sandbox.
    pub fn hidden_dirs(&self) -> Vec<PathBuf> {
        self.sandbox.hide.clone().unwrap_or_else(|| self.repo.parent().map(Path::to_path_buf).into_iter().collect())
    }
}

pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn find_upwards(start: &Path) -> Option<PathBuf> {
    start.ancestors().map(|d| d.join(FILE_NAME)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_runs_as_relay_finds_it_not_as_a_project_shell_would() {
        let conf = HarnessConf::default();
        let inv = conf.locate(crate::harness::Invocation::new("sh"));
        assert!(Path::new(&inv.program).is_absolute(), "{:?}", inv.program);
        let pinned = HarnessConf { program: Some("/opt/x/claude".into()), ..HarnessConf::default() };
        assert_eq!(pinned.locate(crate::harness::Invocation::new("claude")).program, "/opt/x/claude");
        let missing = conf.locate(crate::harness::Invocation::new("no-such-harness-xyz"));
        assert_eq!(missing.program, "no-such-harness-xyz", "left to fail as it would have");
    }

    fn repo() -> tempdir::Dir {
        let d = tempdir::Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        d
    }

    #[test]
    fn minimal_config_takes_defaults() {
        let d = repo();
        let cfg = Config::parse("name = \"demo\"", d.path()).unwrap();
        assert_eq!(cfg.limits.max_running, 2);
        assert!(cfg.sandbox.enabled);
        assert_eq!(cfg.hidden_dirs(), vec![cfg.repo.parent().unwrap().to_path_buf()]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let d = repo();
        assert!(Config::parse("name = \"demo\"\n[limits]\nmax_runing = 3", d.path()).is_err());
    }

    #[test]
    fn bad_values_are_rejected() {
        let d = repo();
        assert!(Config::parse("name = \"Demo\"", d.path()).is_err());
        assert!(Config::parse("name = \"demo\"\n[limits]\nfloor = 1.5", d.path()).is_err());
        assert!(Config::parse("name = \"demo\"\n[sandbox]\nhide_home = [\"../x\"]", d.path()).is_err());
    }

    #[test]
    fn the_example_config_parses_and_matches_the_defaults() {
        let d = repo();
        let cfg = Config::parse(include_str!("../relay.example.toml"), d.path()).unwrap();
        let defaults = Config::parse("name = \"myproject\"", d.path()).unwrap();
        assert_eq!(format!("{:?}", cfg.limits), format!("{:?}", defaults.limits));
        assert_eq!(cfg.sandbox.hide_home, defaults.sandbox.hide_home);
        assert_eq!(cfg.sandbox.caches, defaults.sandbox.caches);
        assert_eq!(cfg.session.max_hours, defaults.session.max_hours);
    }

    #[test]
    fn names() {
        assert!(valid_name("fix-parser-2"));
        assert!(!valid_name("-x"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }
}

/// A self-deleting temporary directory for tests, without a dependency.
#[cfg(test)]
pub mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "relay-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
