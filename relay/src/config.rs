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
    /// What `relay land gc` removes, and when. On by default: it is a safety feature.
    #[serde(default)]
    pub gc: Gc,
    /// Named job profiles, merged from the owner's policy file and this project's config.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Who decides at each decision point, in order.
    #[serde(default)]
    pub hooks: Hooks,
    /// The policy file this was merged with, when one was read.
    #[serde(skip)]
    pub policy_path: Option<PathBuf>,
    /// This machine's record of where each harness's program is ([`crate::host::resolve`]), set
    /// where a config is loaded for real; none in a test, which then finds programs on `PATH` and
    /// records nothing.
    #[serde(skip)]
    pub programs: Option<PathBuf>,
    /// Where each merged setting came from: `profiles.NAME.FIELD` and
    /// `session.banned_models.REGEX`, each `policy` or `project`.
    #[serde(skip)]
    pub origins: BTreeMap<String, Origin>,
}

/// Which file a merged setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Policy,
    Project,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Project => "project",
        }
    }
}

/// A named kind of job: what `queue add --profile` fills in when the command line says nothing.
/// Choosing the profile is the orchestrator's judgment; this only maps it to settings.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Profile {
    /// The one harness the job runs on.
    pub harness: Option<String>,
    /// The model, for `harness` (or for the single harness that applies).
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Spend cap per attempt, in USD.
    pub budget: Option<f64>,
    pub readonly: Option<bool>,
    /// What the profile is for, shown by `relay profiles`.
    pub note: Option<String>,
    /// No more than this many jobs of the profile run at once.
    pub max_running: Option<usize>,
    /// No more than this many sessions of the profile start in a rolling 24 hours.
    pub max_per_day: Option<usize>,
    /// The acceptance check a build job of the profile gets when its line names none.
    pub check: Option<String>,
    /// Whether that check must also fail at the job's base commit (the revert control).
    pub check_red_first: Option<bool>,
    /// `"family"`: a review of a job of this profile may not run on the model family that built it.
    pub independent: Option<String>,
    /// The profile a job is requeued under once `escalate_after` of its ends have failed.
    pub escalate_to: Option<String>,
    /// How many failed ends of one job reach `escalate_to`.
    pub escalate_after: Option<u32>,
}

impl Profile {
    /// The fields of the profile, as `(name, value)` for those that are set.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut put = |k, v: Option<String>| out.extend(v.map(|v| (k, v)));
        put("harness", self.harness.clone());
        put("model", self.model.clone());
        put("effort", self.effort.clone());
        put("budget", self.budget.map(|b| b.to_string()));
        put("readonly", self.readonly.map(|b| b.to_string()));
        put("max_running", self.max_running.map(|n| n.to_string()));
        put("max_per_day", self.max_per_day.map(|n| n.to_string()));
        put("check", self.check.clone());
        put("check_red_first", self.check_red_first.map(|b| b.to_string()));
        put("independent", self.independent.clone());
        put("escalate_to", self.escalate_to.clone());
        put("escalate_after", self.escalate_after.map(|n| n.to_string()));
        put("note", self.note.clone());
        out
    }

    /// `self`, with every field it leaves unset taken from `base`.
    fn over(&self, base: &Self) -> Self {
        Self {
            harness: self.harness.clone().or_else(|| base.harness.clone()),
            model: self.model.clone().or_else(|| base.model.clone()),
            effort: self.effort.clone().or_else(|| base.effort.clone()),
            budget: self.budget.or(base.budget),
            readonly: self.readonly.or(base.readonly),
            note: self.note.clone().or_else(|| base.note.clone()),
            max_running: self.max_running.or(base.max_running),
            max_per_day: self.max_per_day.or(base.max_per_day),
            check: self.check.clone().or_else(|| base.check.clone()),
            check_red_first: self.check_red_first.or(base.check_red_first),
            independent: self.independent.clone().or_else(|| base.independent.clone()),
            escalate_to: self.escalate_to.clone().or_else(|| base.escalate_to.clone()),
            escalate_after: self.escalate_after.or(base.escalate_after),
        }
    }

    fn validate(&self, name: &str) -> Result<()> {
        ensure!(valid_name(name), "profile names are lowercase letters, digits and dashes, not {name:?}");
        if let Some(h) = &self.harness {
            crate::harness::by_id(h).map_err(|e| anyhow::anyhow!("profiles.{name}.harness: {e}"))?;
        }
        ensure!(self.budget.is_none_or(positive), "profiles.{name}.budget must be a positive number");
        ensure!(self.max_running.is_none_or(|n| n >= 1), "profiles.{name}.max_running must be at least 1");
        ensure!(self.max_per_day.is_none_or(|n| n >= 1), "profiles.{name}.max_per_day must be at least 1");
        ensure!(self.check.as_deref().is_none_or(|c| !c.trim().is_empty()), "profiles.{name}.check is empty");
        ensure!(
            self.check_red_first != Some(true) || self.check.is_some(),
            "profiles.{name}.check_red_first needs a check"
        );
        ensure!(
            self.independent.as_deref().is_none_or(|i| i == "family"),
            "profiles.{name}.independent must be \"family\""
        );
        ensure!(
            self.escalate_to.is_some() == self.escalate_after.is_some(),
            "profiles.{name}: escalate_to and escalate_after go together"
        );
        ensure!(self.escalate_after.is_none_or(|n| n >= 1), "profiles.{name}.escalate_after must be at least 1");
        Ok(())
    }
}

/// The owner's policy file: what every project on the machine starts from.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Policy {
    session: PolicySession,
    profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct PolicySession {
    banned_models: BTreeMap<String, String>,
}

/// The policy file's default place, under `$HOME`.
const POLICY_FILE: &str = ".config/relay/policy.toml";

impl Policy {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Where the policy is read from, and whether `$RELAY_POLICY` names it
    /// (then it must exist): `$RELAY_POLICY`, else `~/.config/relay/policy.toml`.
    #[allow(
        clippy::disallowed_methods,
        reason = "which policy is the caller's choice; children get it from the config"
    )]
    pub fn path() -> Option<(PathBuf, bool)> {
        match std::env::var_os("RELAY_POLICY").filter(|p| !p.is_empty()) {
            Some(p) => Some((PathBuf::from(p), true)),
            None => std::env::var_os("HOME").map(|h| (Self::default_in(Path::new(&h)), false)),
        }
    }

    /// Where the policy is when `$RELAY_POLICY` names none, for a user whose home is `home`.
    pub fn default_in(home: &Path) -> PathBuf {
        home.join(POLICY_FILE)
    }

    /// The policy file, optional unless `$RELAY_POLICY` names it.
    pub fn load() -> Result<Option<(Self, PathBuf)>> {
        let Some((path, named)) = Self::path() else { return Ok(None) };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !named => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading policy {}", path.display())),
        };
        let policy = Self::parse(&text).with_context(|| format!("in policy {}", path.display()))?;
        Ok(Some((policy, path)))
    }
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
    /// A meter that resets within `within_hours` has this floor instead of
    /// `floor`: quota still unused at a reset is lost. Absent, `floor` holds everywhere.
    pub floor_near_reset: Option<NearReset>,
    /// Default spend cap per session, passed as `--max-budget-usd`.
    pub budget_usd: f64,
    /// A session that prints nothing for this long is ended.
    pub silent_minutes: u64,
    /// No new session starts while the 1-minute load average is above this.
    pub max_load: Option<f64>,
    /// No new build session (each makes a clone) starts below this much free
    /// disk; a gc pass runs at once below it, and the largest running clone's
    /// job reaches the `disk` decision point.
    #[serde(alias = "min_free_gb")]
    pub min_free_disk_gb: f64,
    /// A running build job whose clone passes this many GB reaches the `disk` decision point.
    pub job_disk_gb: f64,
    /// How often a running job's clone is measured, in minutes.
    pub disk_sample_minutes: f64,
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

/// The handler chains at relay's decision points, and their bounds. A chain
/// left unset keeps relay's fixed behaviour at that point.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Hooks {
    /// At a spend cap. Unset: `["rule", "agent:<limits.checker>"]` when the budget may grow.
    pub cap: Option<Vec<String>>,
    /// When a run ends done or failed.
    pub ended: Option<Vec<String>>,
    /// When a queued job has been held by a limit for `held_after_minutes`.
    pub held: Option<Vec<String>>,
    /// When a running job's clone is over `limits.job_disk_gb`, or the disk is nearly full.
    pub disk: Option<Vec<String>>,
    pub held_after_minutes: f64,
    /// How long an `orchestrator` handler waits for `relay decide answer` before the next handler.
    pub orchestrator_timeout_minutes: f64,
    /// How long an `agent:` handler waits for an account to take its session (a free seat, a usable
    /// reading) before it defers to the next handler. A cap checkpoint that defers stops the job.
    pub seat_wait_minutes: f64,
    /// How long a `command:` handler may run.
    pub command_timeout_seconds: u64,
    /// Whether a `held` decision may `force` a job past its profile's limits.
    pub allow_force: bool,
    /// The most decisions that may act on one job (one incarnation) once any chain
    /// is set; past it every action is refused and the point's fallback stands.
    pub max_decisions: u32,
}

impl Hooks {
    /// Whether any chain is set.
    pub fn any(&self) -> bool {
        self.cap.is_some() || self.ended.is_some() || self.held.is_some()
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            cap: None,
            ended: None,
            held: None,
            disk: None,
            held_after_minutes: 60.0,
            orchestrator_timeout_minutes: 20.0,
            seat_wait_minutes: 30.0,
            command_timeout_seconds: 120,
            allow_force: false,
            max_decisions: 10,
        }
    }
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

/// The looser floor for a meter about to reset.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearReset {
    pub within_hours: f64,
    pub floor: f64,
}

impl Limits {
    /// The floor for a meter that resets at `resets_at` (unix seconds), at
    /// `now_s`, and whether that is the near-reset one.
    pub fn floor_for(&self, resets_at: Option<i64>, now_s: i64) -> (f64, bool) {
        #[allow(clippy::cast_precision_loss)]
        match (self.floor_near_reset, resets_at) {
            (Some(n), Some(r)) if r > now_s && (r - now_s) as f64 <= n.within_hours * 3600.0 => (n.floor, true),
            _ => (self.floor, false),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_running: 2,
            per_account: 1,
            floor: 0.9,
            floor_near_reset: None,
            budget_usd: 10.0,
            silent_minutes: 30,
            max_load: None,
            min_free_disk_gb: 10.0,
            job_disk_gb: 40.0,
            disk_sample_minutes: 3.0,
            max_retries: 1,
            budget_growth: 1.0,
            job_ceiling_usd: None,
            checker: Checker::default(),
        }
    }
}

/// What `relay land gc` removes. Every default is on: nothing here needs
/// switching on to keep the disk from filling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Gc {
    /// Paths in a clone, relative to it, removed from the clone of an ended job
    /// that nothing waits for, once it has been ended for `outputs_after_hours`.
    pub build_outputs: Vec<String>,
    pub outputs_after_hours: f64,
    /// A `done` job's whole clone goes after this many days, its commits kept in the repository.
    pub clones_after_days: f64,
    /// A failed or stopped job's clone is kept this long, in case it is resumed.
    pub waiting_retain_days: f64,
    /// A job's log (`<job>.log.ndjson`) is gzipped after this many days.
    pub logs_gzip_after_days: f64,
    /// The dispatcher runs a pass at most this often while it is awake.
    pub interval_minutes: f64,
    /// A pass stops after this long, and the next one goes on.
    pub pass_seconds: u64,
}

impl Default for Gc {
    fn default() -> Self {
        Self {
            build_outputs: ["target", "node_modules", ".venv"].map(String::from).to_vec(),
            outputs_after_hours: 6.0,
            clones_after_days: 7.0,
            waiting_retain_days: 7.0,
            logs_gzip_after_days: 3.0,
            interval_minutes: 60.0,
            pass_seconds: 300,
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
    /// Default: on when its program is found ([`Config::harness_programs`]).
    pub enabled: Option<bool>,
    /// The command to run instead of the harness's own name: a path (a relative one is the config
    /// file's directory's), or a name looked up as the harness's own is. Tests use a stub.
    pub program: Option<PathBuf>,
    /// The model for jobs that name none for this harness.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Extra arguments for every session of this harness.
    pub extra_args: Vec<String>,
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
    /// Variables set in every job's environment, over the caller's and the
    /// development environment's, under the harness's own (the account).
    /// For Rust projects `CARGO_INCREMENTAL = "0"` keeps `target/` small.
    pub env: BTreeMap<String, String>,
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
            env: BTreeMap::new(),
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
    /// `/dev/shm`: a RAM tmpfs of at most this many MiB per session, for what wants one (POSIX
    /// shared memory, a test that keeps fsync cheap). A write past it fails with ENOSPC instead of
    /// filling RAM. 0 makes it empty and read-only.
    pub shm_mb: u64,
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
            shm_mb: 256,
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
    /// The project's config alone, as if there were no policy file.
    #[cfg(test)]
    pub fn parse(text: &str, dir: &Path) -> Result<Self> {
        Self::parse_with(text, dir, None)
    }

    /// The project's config over the owner's policy: a profile field the project
    /// sets replaces the policy's, one it leaves out stays; banned models add up.
    pub fn parse_with(text: &str, dir: &Path, policy: Option<(&Policy, &Path)>) -> Result<Self> {
        let mut cfg: Self = toml::from_str(text)?;
        cfg.repo = dir.join(&cfg.repo).canonicalize().with_context(|| format!("repo {}", cfg.repo.display()))?;
        // A relative `program` path is the config file's, as `repo` is, never the working
        // directory's: from here on a program is an absolute path or a bare name.
        for conf in cfg.harnesses.values_mut() {
            if let Some(p) = conf.program.as_mut().filter(|p| p.is_relative() && p.components().count() > 1) {
                *p = dir.join(&*p);
            }
        }
        cfg.merge(policy);
        cfg.validate()?;
        Ok(cfg)
    }

    fn merge(&mut self, policy: Option<(&Policy, &Path)>) {
        let mut origins = BTreeMap::new();
        for re in self.session.banned_models.keys() {
            origins.insert(format!("session.banned_models.{re}"), Origin::Project);
        }
        for (name, p) in &self.profiles {
            for (field, _) in p.fields() {
                origins.insert(format!("profiles.{name}.{field}"), Origin::Project);
            }
        }
        if let Some((policy, path)) = policy {
            for (re, why) in &policy.session.banned_models {
                self.session.banned_models.insert(re.clone(), why.clone());
                origins.insert(format!("session.banned_models.{re}"), Origin::Policy);
            }
            let mut merged = policy.profiles.clone();
            for (name, p) in &self.profiles {
                let base = policy.profiles.get(name).cloned().unwrap_or_default();
                merged.insert(name.clone(), p.over(&base));
            }
            for (name, p) in &policy.profiles {
                for (field, _) in p.fields() {
                    origins.entry(format!("profiles.{name}.{field}")).or_insert(Origin::Policy);
                }
            }
            self.profiles = merged;
            self.policy_path = Some(path.to_path_buf());
        }
        self.origins = origins;
    }

    /// The model `job` last ran on (as its dispatch recorded it), or would run on, when that is known.
    pub fn model_of(&self, job: &crate::world::Job) -> Option<String> {
        if let Some(model) = &job.model {
            return Some(model.clone());
        }
        let account = job.transcript_account.as_deref().or(job.account.as_deref());
        let harness = account.and_then(|a| a.split(':').next()).or(match job.harnesses.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        })?;
        job.models.get(harness).cloned().or_else(|| self.harness(harness).model)
    }

    /// The profile called `name`. The one place a profile is looked up: an unknown name
    /// is refused (not found) with the names there are.
    pub fn profile(&self, name: &str) -> Result<&Profile> {
        let Some(p) = self.profiles.get(name) else {
            let names: Vec<&str> = self.profiles.keys().map(String::as_str).collect();
            let known =
                if names.is_empty() { "there are none".to_owned() } else { format!("profiles: {}", names.join(", ")) };
            fail!(NotFound, "no profile named {name} ({known})");
        };
        Ok(p)
    }

    /// Where a merged setting came from, `project` when unrecorded.
    pub fn origin(&self, key: &str) -> Origin {
        self.origins.get(key).copied().unwrap_or(Origin::Project)
    }

    /// `--config`, else `$RELAY_CONFIG`, else the nearest `relay.toml` at or above the
    /// current directory; `None` when there is none of these.
    #[allow(clippy::disallowed_methods, reason = "which project is the caller's choice")]
    pub fn find(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
        match explicit.map(Path::to_path_buf).or_else(|| std::env::var_os("RELAY_CONFIG").map(PathBuf::from)) {
            Some(p) => Ok(Some(p)),
            None => Ok(find_upwards(&std::env::current_dir()?)),
        }
    }

    /// The project config [`Config::find`] finds, merged with the owner's policy.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = Self::find(explicit)?.with_context(|| format!("no {FILE_NAME} here or above; pass --config"))?;
        Self::load_at(&path)
    }

    /// The project config at `path`, merged with the owner's policy.
    pub fn load_at(path: &Path) -> Result<(Self, PathBuf)> {
        let path = path.canonicalize().with_context(|| format!("config {}", path.display()))?;
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let policy = Policy::load()?;
        let cfg = Self::parse_with(
            &text,
            path.parent().unwrap_or(Path::new("/")),
            policy.as_ref().map(|(p, from)| (p, from.as_path())),
        )
        .with_context(|| format!("in {}", path.display()))?;
        let cfg = Self { programs: Some(crate::host::record_in(&crate::ctx::state_root())), ..cfg };
        Ok((cfg, path))
    }

    /// The owner's policy alone, for the read-only views outside any project. Its bans and
    /// profiles are checked as a project's would be; it names no project or repository, so
    /// it must never reach `Ctx::new` (an empty name would put job state at relay's roots).
    pub fn policy_only() -> Result<Self> {
        let policy = Policy::load()?;
        let mut cfg = Self {
            name: String::new(),
            repo: PathBuf::new(),
            limits: Limits::default(),
            accounts: Accounts::default(),
            harnesses: BTreeMap::new(),
            session: Session::default(),
            sandbox: Sandbox::default(),
            gc: Gc::default(),
            profiles: BTreeMap::new(),
            hooks: Hooks::default(),
            policy_path: None,
            programs: Some(crate::host::record_in(&crate::ctx::state_root())),
            origins: BTreeMap::new(),
        };
        cfg.merge(policy.as_ref().map(|(p, from)| (p, from.as_path())));
        if let Some((_, from)) = &policy {
            cfg.validate_policy().with_context(|| format!("in policy {}", from.display()))?;
        }
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        ensure!(valid_name(&self.name), "name must be lowercase letters, digits and dashes");
        ensure!(self.repo.join(".git").exists(), "repo {} is not a git repository", self.repo.display());
        ensure!(self.limits.max_running >= 1 && self.limits.per_account >= 1, "limits must be at least 1");
        ensure!((0.0..=1.0).contains(&self.limits.floor), "limits.floor is a fraction between 0 and 1");
        if let Some(n) = self.limits.floor_near_reset {
            ensure!(positive(n.within_hours), "limits.floor_near_reset.within_hours must be a positive number");
            ensure!(
                n.floor > self.limits.floor && n.floor <= 1.0,
                "limits.floor_near_reset.floor must be above limits.floor and at most 1"
            );
        }
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
        self.validate_policy()?;
        let h = &self.hooks;
        for (point, chain) in [("cap", &h.cap), ("ended", &h.ended), ("held", &h.held), ("disk", &h.disk)] {
            for handler in chain.iter().flatten() {
                crate::systems::decide::Handler::parse(handler, self)
                    .map_err(|e| anyhow::anyhow!("hooks.{point}: {e}"))?;
            }
        }
        ensure!(h.held_after_minutes >= 0.0, "hooks.held_after_minutes must not be negative");
        ensure!(
            !h.any() || self.limits.job_ceiling_usd.is_some(),
            "[hooks] needs limits.job_ceiling_usd: a decision may spend more only up to it"
        );
        ensure!(h.orchestrator_timeout_minutes >= 0.0, "hooks.orchestrator_timeout_minutes must not be negative");
        ensure!(
            h.seat_wait_minutes.is_finite() && h.seat_wait_minutes >= 0.0,
            "hooks.seat_wait_minutes must not be negative"
        );
        self.validate_disk()?;
        regex::Regex::new(&self.sandbox.env_deny).context("sandbox.env_deny")?;
        for p in self.sandbox.hide_home.iter().chain(&self.sandbox.caches) {
            if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("{} must be a path inside $HOME, written relative to it", p.display());
            }
        }
        Ok(())
    }

    /// What the policy can set, the bans and the profiles, checked alike inside a project
    /// and outside one ([`Config::policy_only`]).
    fn validate_policy(&self) -> Result<()> {
        for re in self.session.banned_models.keys() {
            regex::Regex::new(re).with_context(|| format!("session.banned_models key {re:?}"))?;
        }
        for (name, p) in &self.profiles {
            p.validate(name)?;
        }
        self.validate_escalation()
    }

    /// Every `escalate_to` names a profile, and following them never comes back round.
    fn validate_escalation(&self) -> Result<()> {
        for (name, p) in &self.profiles {
            let mut seen = vec![name.as_str()];
            let mut at = p;
            while let Some(to) = at.escalate_to.as_deref() {
                let Some(next) = self.profiles.get(to) else {
                    bail!("profiles.{}.escalate_to names {to}, which is not a profile", seen[seen.len() - 1]);
                };
                ensure!(!seen.contains(&to), "profiles.{name}.escalate_to leads back to {to}: an escalation cycle");
                seen.push(to);
                at = next;
            }
        }
        Ok(())
    }

    /// The disk limits, `[gc]` and `[session] env`. A `gc.build_outputs` entry
    /// names a place inside a clone, so it can never reach outside one.
    fn validate_disk(&self) -> Result<()> {
        let (l, g) = (&self.limits, &self.gc);
        ensure!(l.job_disk_gb.is_finite() && l.job_disk_gb >= 0.0, "limits.job_disk_gb must not be negative");
        ensure!(positive(l.disk_sample_minutes), "limits.disk_sample_minutes must be a positive number");
        for (key, v) in [
            ("outputs_after_hours", g.outputs_after_hours),
            ("clones_after_days", g.clones_after_days),
            ("waiting_retain_days", g.waiting_retain_days),
            ("logs_gzip_after_days", g.logs_gzip_after_days),
        ] {
            ensure!(v.is_finite() && v >= 0.0, "gc.{key} must not be negative");
        }
        ensure!(positive(g.interval_minutes), "gc.interval_minutes must be a positive number");
        ensure!(g.pass_seconds >= 1, "gc.pass_seconds must be at least 1");
        for path in &g.build_outputs {
            let p = Path::new(path);
            let plain = p.components().all(|c| matches!(c, std::path::Component::Normal(_)));
            ensure!(
                !path.is_empty() && plain && !p.is_absolute(),
                "gc.build_outputs entry {path:?} must be a path inside the clone, such as \"target\""
            );
        }
        for k in self.session.env.keys() {
            ensure!(valid_env_name(k), "session.env name {k:?} is not a variable name");
        }
        Ok(())
    }

    /// Every harness this config expects, with its program as this machine has it, in registry
    /// order: one not turned off whose program is found, or one `[harness.X]` names (so a missing
    /// program is shown, never silently dropped). The one place a harness's program is looked up.
    pub fn harness_programs(&self) -> Vec<(&'static dyn crate::harness::Harness, crate::host::Program)> {
        crate::harness::ALL
            .iter()
            .copied()
            .filter_map(|h| {
                let conf = self.harnesses.get(h.id());
                if conf.and_then(|c| c.enabled) == Some(false) {
                    return None;
                }
                let name = conf.and_then(|c| c.program.clone()).unwrap_or_else(|| PathBuf::from(h.id()));
                let program = crate::host::resolve(self.programs.as_deref(), name.as_os_str());
                (conf.is_some() || matches!(program, crate::host::Program::Found(_))).then_some((h, program))
            })
            .collect()
    }

    /// The harnesses relay may use: those [`Config::harness_programs`] found, in registry order.
    pub fn harnesses(&self) -> Vec<&'static dyn crate::harness::Harness> {
        let found = self.harness_programs().into_iter();
        found.filter(|(_, p)| matches!(p, crate::host::Program::Found(_))).map(|(h, _)| h).collect()
    }

    /// The harnesses this config expects whose program is not found, each with what was looked for.
    pub fn missing_harnesses(&self) -> Vec<(&'static str, String)> {
        let all = self.harness_programs().into_iter();
        all.filter_map(|(h, p)| match p {
            crate::host::Program::Missing(what) => Some((h.id(), what.to_string_lossy().into_owned())),
            crate::host::Program::Found(_) => None,
        })
        .collect()
    }

    /// `inv` run by its harness's program as this machine has it ([`Config::harness_programs`]):
    /// never by whatever a caller's or a project shell's `PATH` would find first (one project's
    /// devshell once put a stale `nixpkgs` claude there, on which `opus` meant an older model).
    pub fn locate(
        &self,
        harness: &str,
        mut inv: crate::harness::Invocation,
    ) -> Result<crate::harness::Invocation, crate::harness::Refusal> {
        let name = self.harness(harness).program.map_or_else(|| inv.program.clone(), PathBuf::into_os_string);
        match crate::host::resolve(self.programs.as_deref(), &name) {
            crate::host::Program::Found(p) => {
                inv.program = p.into_os_string();
                Ok(inv)
            }
            crate::host::Program::Missing(what) => Err(crate::harness::Refusal(format!(
                "harness {harness}: program not found: {}",
                what.to_string_lossy()
            ))),
        }
    }

    pub fn harness(&self, id: &str) -> HarnessConf {
        self.harnesses.get(id).cloned().unwrap_or_default()
    }

    /// Directories hidden inside the sandbox.
    pub fn hidden_dirs(&self) -> Vec<PathBuf> {
        self.sandbox.hide.clone().unwrap_or_else(|| self.repo.parent().map(Path::to_path_buf).into_iter().collect())
    }
}

/// A shell-style variable name: letters, digits and underscores, not starting with a digit.
fn valid_env_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    #[test]
    fn a_model_belongs_to_the_family_its_provider_names() {
        let f = super::model_family;
        assert_eq!(f("claude-opus-5-5"), "anthropic");
        assert_eq!(f("sonnet"), "anthropic");
        assert_eq!(f("openai-codex/gpt-6.1-sol"), "openai");
        assert_eq!(f("o3-mini"), "openai");
        assert_eq!(f("google-antigravity/gemini-3-pro"), "google");
        assert_eq!(f("minimax-code/MiniMax-M3.1-Flash"), "minimax");
        assert_eq!(f("stub/m1"), "stub", "unknown: its provider");
        assert_eq!(f("llama-9"), "llama", "unknown and bare: its first word");
        assert_eq!(f("opus"), f("claude-haiku-5-5"), "tiers of one provider are one family");
    }

    #[test]
    fn an_escalation_must_name_a_profile_and_never_loop() {
        let mut cfg: Config = toml::from_str("name = \"x\"").unwrap();
        let profile = |to: Option<&str>| Profile {
            escalate_to: to.map(str::to_owned),
            escalate_after: to.map(|_| 2),
            ..Profile::default()
        };
        cfg.profiles.insert("a".into(), profile(Some("b")));
        assert!(cfg.validate_escalation().unwrap_err().to_string().contains("not a profile"));
        cfg.profiles.insert("b".into(), profile(Some("a")));
        assert!(cfg.validate_escalation().unwrap_err().to_string().contains("escalation cycle"));
        cfg.profiles.insert("b".into(), profile(None));
        cfg.validate_escalation().unwrap();
        assert!(profile(Some("b")).validate("a").is_ok());
        let half = Profile { escalate_to: Some("b".into()), ..Profile::default() };
        assert!(half.validate("a").is_err(), "escalate_to and escalate_after go together");
        let odd = Profile { independent: Some("model".into()), ..Profile::default() };
        assert!(odd.validate("a").is_err());
    }
    use super::*;

    #[test]
    fn a_harness_runs_as_relay_finds_it_not_as_a_project_shell_would() {
        use std::os::unix::fs::PermissionsExt;
        let d = repo();
        let cfg = Config::parse("name = \"x\"", d.path()).unwrap();
        let inv = cfg.locate("claude", crate::harness::Invocation::new("sh")).unwrap();
        assert!(Path::new(&inv.program).is_absolute(), "{:?}", inv.program);
        let pinned = d.path().join("claude");
        std::fs::write(&pinned, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o755)).unwrap();
        let conf = format!("name = \"x\"\n[harness.claude]\nprogram = \"{}\"\n", pinned.display());
        let cfg = Config::parse(&conf, d.path()).unwrap();
        assert_eq!(cfg.locate("claude", crate::harness::Invocation::new("claude")).unwrap().program, pinned);
        // A program that is not there is refused by name, never left for the shell to fail on.
        let missing = cfg.locate("omp", crate::harness::Invocation::new("no-such-harness-xyz")).unwrap_err();
        assert_eq!(missing.0, "harness omp: program not found: no-such-harness-xyz");
        let gone = Config::parse("name = \"x\"\n[harness.omp]\nprogram = \"/opt/x/omp\"\n", d.path()).unwrap();
        assert!(gone.locate("omp", crate::harness::Invocation::new("omp")).unwrap_err().0.contains("/opt/x/omp"));
        assert!(!gone.harnesses().iter().any(|h| h.id() == "omp"), "not usable");
        assert_eq!(gone.missing_harnesses(), [("omp", "/opt/x/omp".to_owned())], "but shown");
        // A relative path is the config directory's (`pinned` is there), wherever relay runs from; a bare
        // name stays a name.
        let rel = Config::parse(
            "name = \"x\"\n[harness.claude]\nprogram = \"./claude\"\n[harness.omp]\nprogram = \"omp-beta\"\n",
            d.path(),
        )
        .unwrap();
        assert_eq!(rel.harness("claude").program, Some(d.path().join("./claude")));
        assert_eq!(
            rel.locate("claude", crate::harness::Invocation::new("claude")).unwrap().program,
            d.path().join("claude")
        );
        assert_eq!(rel.harness("omp").program, Some(PathBuf::from("omp-beta")));
        let off = Config::parse("name = \"x\"\n[harness.omp]\nenabled = false\nprogram = \"/opt/x/omp\"\n", d.path());
        assert!(off.unwrap().missing_harnesses().is_empty(), "a harness turned off is not missing");
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
    fn the_near_reset_floor_is_above_the_floor_and_at_most_one() {
        let d = repo();
        let parse = |n: &str| Config::parse(&format!("name = \"demo\"\n[limits]\nfloor_near_reset = {n}"), d.path());
        let cfg = parse("{ within_hours = 12, floor = 0.98 }").unwrap();
        assert_eq!(cfg.limits.floor_for(Some(100 + 11 * 3600), 100), (0.98, true));
        assert_eq!(cfg.limits.floor_for(Some(100 + 13 * 3600), 100), (0.9, false), "too far off");
        assert_eq!(cfg.limits.floor_for(None, 100), (0.9, false), "an unknown reset is not near");
        assert_eq!(cfg.limits.floor_for(Some(50), 100), (0.9, false), "a reset past is no meter");
        assert!(parse("{ within_hours = 12, floor = 1.0 }").is_ok());
        for bad in [
            "{ within_hours = 12, floor = 0.9 }",
            "{ within_hours = 12, floor = 1.1 }",
            "{ within_hours = 0, floor = 0.95 }",
            "{ floor = 0.95 }",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Config::parse("name = \"demo\"", d.path()).unwrap().limits.floor_near_reset, None);
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
        let hooked = "name = \"demo\"\n[hooks]\nended = [\"rule\"]\n";
        assert!(Config::parse(hooked, d.path()).unwrap_err().to_string().contains("job_ceiling_usd"));
        let bounded = "name = \"demo\"\n[limits]\njob_ceiling_usd = 5.0\n[hooks]\nended = [\"rule\"]\n";
        assert!(Config::parse(bounded, d.path()).is_ok());
    }

    #[test]
    fn gc_and_the_disk_ceiling_are_on_by_default_and_validated() {
        let d = repo();
        let cfg = Config::parse("name = \"demo\"", d.path()).unwrap();
        assert_eq!(cfg.gc.build_outputs, ["target", "node_modules", ".venv"]);
        assert_eq!((cfg.gc.outputs_after_hours, cfg.gc.clones_after_days, cfg.gc.waiting_retain_days), (6.0, 7.0, 7.0));
        assert_eq!((cfg.gc.logs_gzip_after_days, cfg.gc.interval_minutes), (3.0, 60.0));
        assert_eq!((cfg.limits.job_disk_gb, cfg.limits.min_free_disk_gb), (40.0, 10.0));
        assert!(cfg.session.env.is_empty(), "no variable is hard-coded");
        let cfg = Config::parse(
            "name = \"d\"\n[limits]\nmin_free_gb = 20\n[session.env]\nCARGO_INCREMENTAL = \"0\"",
            d.path(),
        )
        .unwrap();
        assert_eq!(cfg.limits.min_free_disk_gb, 20.0, "min_free_gb is the same key");
        assert_eq!(cfg.session.env["CARGO_INCREMENTAL"], "0");
        for bad in [
            "[gc]\nbuild_outputs = [\"../x\"]",
            "[gc]\nbuild_outputs = [\"/usr\"]",
            "[gc]\nbuild_outputs = [\".\"]",
            "[gc]\nbuild_outputs = [\"\"]",
            "[gc]\nclones_after_days = -1",
            "[gc]\npass_seconds = 0",
            "[limits]\njob_disk_gb = -5",
            "[session.env]\n\"A-B\" = \"1\"",
        ] {
            assert!(Config::parse(&format!("name = \"demo\"\n{bad}"), d.path()).is_err(), "{bad}");
        }
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
        assert_eq!(format!("{:?}", cfg.hooks), format!("{:?}", defaults.hooks));
        assert_eq!(format!("{:?}", cfg.gc), format!("{:?}", defaults.gc));
    }

    const POLICY: &str = r#"
[session]
banned_models = { "^minimax-code/" = "MiniMax is banned", "^x/" = "policy says no" }

[profiles.bug-hunt]
harness = "omp"
model = "openai-codex/gpt-6.1-sol"
effort = "high"
readonly = true
max_per_day = 3
note = "costly"

[profiles.mechanical]
harness = "claude"
budget = 3
"#;

    fn merged(project: &str) -> Config {
        merged_with(POLICY, project)
    }

    fn merged_with(policy: &str, project: &str) -> Config {
        let d = repo();
        let policy = Policy::parse(policy).unwrap();
        Config::parse_with(&format!("name = \"demo\"\n{project}"), d.path(), Some((&policy, Path::new("/p.toml"))))
            .unwrap()
    }

    #[test]
    fn a_project_profile_overrides_the_policy_field_by_field() {
        let cfg = merged("[profiles.bug-hunt]\neffort = \"low\"\nmax_running = 1\n");
        let p = cfg.profile("bug-hunt").unwrap();
        assert_eq!(p.effort.as_deref(), Some("low"), "the project's own field wins");
        assert_eq!(p.model.as_deref(), Some("openai-codex/gpt-6.1-sol"), "an unset field stays the policy's");
        assert_eq!((p.max_per_day, p.max_running, p.readonly), (Some(3), Some(1), Some(true)));
        assert_eq!(cfg.origin("profiles.bug-hunt.effort"), Origin::Project);
        assert_eq!(cfg.origin("profiles.bug-hunt.model"), Origin::Policy);
        assert_eq!(cfg.origin("profiles.bug-hunt.max_running"), Origin::Project);
        assert_eq!(cfg.profile("mechanical").unwrap().budget, Some(3.0), "a policy-only profile is there");
        assert_eq!(cfg.policy_path.as_deref(), Some(Path::new("/p.toml")));
    }

    #[test]
    fn a_project_adds_its_own_profiles_and_bans_to_the_policy() {
        let cfg =
            merged("[session]\nbanned_models = { \"^y/\" = \"project says no\" }\n[profiles.mine]\nmodel = \"m\"\n");
        assert!(cfg.session.banned("minimax-code/M3").is_some(), "the policy's ban stays");
        assert_eq!(cfg.session.banned("y/z"), Some("project says no"));
        assert!(cfg.profile("mine").is_ok() && cfg.profile("bug-hunt").is_ok());
        assert_eq!(cfg.origin("session.banned_models.^y/"), Origin::Project);
        assert_eq!(cfg.origin("session.banned_models.^x/"), Origin::Policy);
    }

    #[test]
    fn a_project_cannot_lift_a_policy_ban() {
        let cfg = merged("[session]\nbanned_models = { \"^x/\" = \"fine really\" }\n");
        assert_eq!(cfg.session.banned("x/y"), Some("policy says no"));
    }

    #[test]
    fn the_readme_example_policy_parses() {
        let readme = include_str!("../README.md");
        let from = readme.split("## Policy and profiles").nth(1).unwrap();
        let block = from.split("```toml\n").nth(1).unwrap().split("```").next().unwrap();
        let cfg = merged_with(block, "");
        assert!(cfg.session.banned("minimax-code/M3").is_some() && cfg.session.banned("sonnet").is_some());
        assert_eq!(cfg.profile("bug-hunt").unwrap().max_per_day, Some(3));
        assert_eq!(cfg.profile("mechanical").unwrap().budget, Some(3.0));
    }

    #[test]
    fn without_a_policy_nothing_changes() {
        let d = repo();
        let cfg = Config::parse("name = \"demo\"", d.path()).unwrap();
        assert!(cfg.profiles.is_empty() && cfg.session.banned_models.is_empty() && cfg.policy_path.is_none());
        let msg = cfg.profile("x").unwrap_err();
        assert_eq!(msg.downcast_ref::<crate::reply::Fail>().map(|f| f.kind), Some(crate::reply::Kind::NotFound));
    }

    #[test]
    fn an_unknown_profile_lists_the_known_ones() {
        let msg = merged("").profile("nope").unwrap_err().to_string();
        assert!(msg.contains("bug-hunt, mechanical"), "{msg}");
    }

    #[test]
    fn bad_profiles_are_rejected() {
        let d = repo();
        for bad in ["harness = \"nope\"", "budget = 0", "max_running = 0", "max_per_day = 0", "colour = 1"] {
            let text = format!("name = \"demo\"\n[profiles.p]\n{bad}\n");
            assert!(Config::parse(&text, d.path()).is_err(), "{bad}");
        }
        assert!(Config::parse("name = \"demo\"\n[profiles.Bad]\nnote = \"x\"", d.path()).is_err());
        assert!(Policy::parse("[limits]\nfloor = 0.5").is_err(), "the policy holds only bans and profiles");
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

/// The family of a model id (`openai-codex/gpt-6.1-sol` is `openai`, `claude-opus-5-5` is `anthropic`),
/// by the names the providers use; a model it does not know is its provider prefix, else its first word.
pub fn model_family(model: &str) -> String {
    let m = model.to_ascii_lowercase();
    let name = m.rsplit('/').next().unwrap_or(&m);
    let has = |keys: &[&str]| keys.iter().any(|k| name.contains(k));
    let family = if has(&["claude", "haiku", "sonnet", "opus", "fable"]) {
        "anthropic"
    } else if has(&["gpt", "codex", "openai"])
        || (name.starts_with('o') && name[1..].starts_with(|c: char| c.is_ascii_digit()))
    {
        "openai"
    } else if has(&["gemini", "gemma", "google"]) {
        "google"
    } else if has(&["minimax"]) {
        "minimax"
    } else if has(&["deepseek"]) {
        "deepseek"
    } else if has(&["qwen"]) {
        "qwen"
    } else if has(&["kimi", "moonshot"]) {
        "moonshot"
    } else if has(&["glm", "zhipu"]) {
        "zhipu"
    } else if has(&["grok"]) {
        "xai"
    } else if has(&["mistral", "mixtral", "codestral"]) {
        "mistral"
    } else {
        return m.split_once('/').map_or_else(|| name.split('-').next().unwrap_or(name), |(p, _)| p).to_owned();
    };
    family.to_owned()
}
