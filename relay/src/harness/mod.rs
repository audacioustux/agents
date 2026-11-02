//! Every sibling agent CLI ("harness") behind one interface.
//!
//! A harness is plugged in by implementing [`Harness`] and adding it to
//! [`ALL`]. Callers never branch on which harness they hold: the bridge's
//! one-shot calls and relay's unattended sessions both describe their work
//! as a [`Task`], get an [`Invocation`] back, read the output through the
//! harness's [`Reader`] into a [`report::Report`], and judge it with the
//! same [`report::verdict`]. What a CLI cannot do is declared in [`Caps`],
//! and a task that needs it is refused, never silently weakened.
//!
//! Every adapter is a claim about software this repository does not
//! control. The tests run the installed CLIs where they exist; check again
//! after every upgrade.

pub mod bridge;
pub mod claude;
pub mod omp;
pub mod prompt;
pub mod report;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// A signed-in account of one harness: a Claude Code config dir, an omp profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub harness: &'static str,
    /// Unique within the harness: `default`, `work`.
    pub label: String,
    pub dir: PathBuf,
}

impl Home {
    /// `harness:label`, the name used everywhere outside the adapter.
    pub fn key(&self) -> String {
        format!("{}:{}", self.harness, self.label)
    }
}

/// One quota window. `utilization` is a fraction; `resets_at` is unix seconds.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meter {
    #[serde(default)]
    pub utilization: f64,
    #[serde(default, rename = "resetsAt", alias = "resets_at")]
    pub resets_at: Option<i64>,
}

/// Named meters of one home, such as `seven_day`, or `openai-codex/7 days`.
pub type Meters = BTreeMap<String, Meter>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permissions {
    /// The CLI's own read-only mode, where it has one. The bridge's contract.
    ReadOnly,
    /// May edit files but asks before anything else. For runs with no sandbox.
    EditsOnly,
    /// No permission checks. Only for runs inside a sandbox, whose mounts are the boundary.
    Unrestricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session<'a> {
    /// A new session. A harness that takes a preset id uses this one.
    New(&'a str),
    /// Continue this session, writing to it.
    Resume(&'a str),
    /// Start from this session's history without writing to it.
    Fork(&'a str),
}

/// The work, described the same way for every harness.
#[derive(Debug, Clone)]
pub struct Task<'a> {
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub session: Session<'a>,
    pub permissions: Permissions,
    /// Appended to the system prompt.
    pub briefing: Option<&'a str>,
    /// A spend cap the CLI enforces, where it can. Callers that need one
    /// for sure enforce it themselves from [`report::Report::cost_usd`].
    pub budget_usd: Option<f64>,
    /// A display name for the session.
    pub name: Option<&'a str>,
    /// Machine-readable events on stdout (for a runner), rather than text (for a person).
    pub events: bool,
    /// Leave out MCP servers, plugins, skill listings and user settings, which
    /// cost context on every turn. For unattended tasks that do not use them.
    pub lean: bool,
    /// A private directory for this task: the prompt file, a session store.
    pub scratch: &'a Path,
}

/// A command ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Set (`Some`) or removed (`None`) on top of the caller's environment.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// Written to the child's stdin, then closed.
    pub stdin: Option<String>,
    /// Written before the run. The prompt goes here for a CLI that reads it from a file.
    pub files: Vec<(PathBuf, String)>,
}

impl Invocation {
    fn new(program: &str) -> Self {
        Self { program: program.into(), args: Vec::new(), env: Vec::new(), stdin: None, files: Vec::new() }
    }

    fn arg(&mut self, a: impl Into<OsString>) -> &mut Self {
        self.args.push(a.into());
        self
    }

    fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(&mut self, a: I) -> &mut Self {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    /// Writes [`Self::files`], creating their directories.
    pub fn write_files(&self) -> std::io::Result<()> {
        for (path, text) in &self.files {
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(path, text)?;
        }
        Ok(())
    }
}

/// Why a harness cannot run a task as asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// What a harness can do. Anything `false` here is refused when a task needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Caps {
    /// [`Permissions::ReadOnly`] removes the ability to write, rather than asking first.
    pub read_only_by_capability: bool,
    pub fork: bool,
    /// A new session can be given its id up front.
    pub preset_session_id: bool,
    /// The CLI enforces [`Task::budget_usd`].
    pub budget: bool,
    /// The harness can report its quota without a model call.
    pub quota: bool,
}

/// Turns a harness's output, line by line, into a [`report::Report`].
pub trait Reader: Send {
    /// Feeds one line of stdout. Returns fresh quota meters if the line carried some.
    fn line(&mut self, line: &str) -> Option<Meters>;
    fn report(&self) -> &report::Report;
}

/// One harness. Implement it and add it to [`ALL`] to plug a CLI in.
pub trait Harness: Sync + fmt::Debug {
    /// Also its command name on `PATH`.
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;
    /// How a prompt addresses the model ("You are Claude").
    fn identity(&self) -> &'static str;
    /// Every usable home under `user_home`, in label order.
    fn homes(&self, user_home: &Path) -> Vec<Home>;
    /// Every path that holds any home of this harness, usable or not, so a
    /// sandbox can hide all of them except the session's own.
    fn home_paths(&self, user_home: &Path) -> Vec<PathBuf>;
    /// What a session on `home` must be able to write, besides its checkout.
    fn writable(&self, home: &Home, user_home: &Path) -> Vec<PathBuf>;
    /// What inside those the host later runs or obeys (settings, hooks,
    /// plugins, scripts), so it is mounted back read-only: a session must not
    /// be able to change what runs outside the sandbox.
    fn protected(&self, _home: &Home) -> Vec<PathBuf> {
        Vec::new()
    }
    /// Who the home is signed in as, to merge homes that share one quota. Never printed.
    fn identity_of(&self, _home: &Home) -> Option<String> {
        None
    }
    /// The command for `task` on `home`.
    fn invocation(&self, home: &Home, task: &Task<'_>) -> Result<Invocation, Refusal>;
    fn reader(&self) -> Box<dyn Reader>;
    /// A command printing the home's quota without a model call.
    fn quota(&self, _home: &Home) -> Option<Invocation> {
        None
    }
    fn parse_quota(&self, _text: &str, _now_s: i64) -> Meters {
        Meters::new()
    }
    /// The meters that govern `model`. An error means they cannot be told,
    /// and a start on this home would be blind.
    fn meters_for<'m>(&self, model: Option<&str>, meters: &'m Meters) -> Result<Vec<&'m Meter>, Refusal>;
    /// Makes `session`, last written on `from`, resumable on `to`. Returns
    /// whether it can be resumed there; if not, the caller starts afresh.
    fn carry(&self, session: &str, from: Option<&Home>, to: &Home, scratch: &Path) -> bool;
}

/// Every harness this crate knows, in a fixed order.
pub static ALL: [&dyn Harness; 2] = [&claude::Claude, &omp::Omp];

pub fn by_id(id: &str) -> Result<&'static dyn Harness, Refusal> {
    ALL.iter().copied().find(|h| h.id() == id).ok_or_else(|| {
        let names: Vec<_> = ALL.iter().map(|h| h.id()).collect();
        Refusal(format!("unknown agent: {id}. Supported: {}", names.join(", ")))
    })
}

/// Whether the harness's command is on `PATH`.
pub fn installed(h: &dyn Harness) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(h.id()).is_file()))
}

/// Control characters replaced, so a value cannot forge log lines or terminal escapes.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_control() { ' ' } else { c }).collect()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn task<'a>(prompt: &'a str, session: Session<'a>, permissions: Permissions, scratch: &'a Path) -> Task<'a> {
        Task {
            prompt,
            model: Some("m"),
            effort: Some("high"),
            session,
            permissions,
            briefing: Some("b"),
            budget_usd: Some(2.5),
            name: Some("n"),
            events: true,
            lean: true,
            scratch,
        }
    }

    pub fn home(harness: &'static str, label: &str) -> Home {
        Home { harness, label: label.into(), dir: PathBuf::from(format!("/h/{label}")) }
    }

    pub fn argv(i: &Invocation) -> String {
        i.args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        assert_eq!(by_id("omp").unwrap().id(), "omp");
        assert!(by_id("codex").unwrap_err().0.contains("Supported: claude, omp"));
        let ids: Vec<_> = ALL.iter().map(|h| h.id()).collect();
        let mut sorted = ids.clone();
        sorted.dedup();
        assert_eq!(ids, sorted, "ids are unique");
    }

    #[test]
    fn every_harness_keeps_the_prompt_out_of_argv() {
        let big = "p".repeat(300_000);
        let d = Path::new("/scratch");
        for h in ALL {
            let home = testing::home(h.id(), "default");
            let i =
                h.invocation(&home, &testing::task(&big, Session::New("id"), Permissions::Unrestricted, d)).unwrap();
            assert!(i.args.iter().all(|a| a.len() < 10_000), "{}", h.id());
            assert!(i.stdin.as_deref() == Some(big.as_str()) || i.files.iter().any(|(_, t)| *t == big), "{}", h.id());
        }
    }

    #[test]
    fn what_a_harness_lacks_is_refused_not_weakened() {
        let d = Path::new("/scratch");
        for h in ALL {
            let home = testing::home(h.id(), "default");
            let fork = h.invocation(&home, &testing::task("p", Session::Fork("old"), Permissions::ReadOnly, d));
            assert_eq!(fork.is_ok(), h.caps().fork, "{} fork", h.id());
        }
    }
}
