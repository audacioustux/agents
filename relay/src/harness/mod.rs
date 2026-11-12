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
pub mod puku;
pub mod report;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// A signed-in account of one harness: a Claude Code config dir, an omp profile, a puku-cli config dir.
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

/// Who a home is signed in as, in a form safe to print: a short hash of the
/// account's uuid and the plan. Neither an email address nor the uuid is kept.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Identity {
    pub plan: String,
    /// The first six hex characters of the sha256 of the account's uuid.
    pub id: String,
    /// Changes when the home's sign-in does (a new login, a new plan), so a
    /// reading taken under another one is taken again. Not shown.
    #[serde(skip)]
    pub stamp: String,
}

/// What a home's own files say about its sign-in, read with no call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// The harness keeps no identity this crate can read.
    Unknown,
    /// No account is signed in.
    Out,
    As(Identity),
}

/// How long a meter's window is. The boards give each its own column, shortest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Window {
    FiveHours,
    Week,
    Month,
    /// Any other: its length in hours when the harness says one.
    Other(Option<u32>),
}

impl Default for Window {
    fn default() -> Self {
        Self::Other(None)
    }
}

impl Window {
    /// The window `hours` long: a named one when it is one of theirs.
    pub fn of_hours(hours: u32) -> Self {
        match hours {
            5 => Self::FiveHours,
            168 => Self::Week,
            672..=744 => Self::Month,
            h => Self::Other(Some(h)),
        }
    }

    /// Its length in hours, when known (a month as thirty days).
    pub fn hours(self) -> Option<u32> {
        match self {
            Self::FiveHours => Some(5),
            Self::Week => Some(168),
            Self::Month => Some(720),
            Self::Other(h) => h,
        }
    }

    /// As a column's heading: `5 hours`, `7 days`, `month`, `1 day`, `other`.
    pub fn label(self) -> String {
        match self {
            Self::FiveHours => "5 hours".into(),
            Self::Week => "7 days".into(),
            Self::Month => "month".into(),
            Self::Other(None) => "other".into(),
            Self::Other(Some(h)) if h % 24 == 0 => {
                let d = h / 24;
                format!("{d} day{}", if d == 1 { "" } else { "s" })
            }
            Self::Other(Some(h)) => format!("{h} hour{}", if h == 1 { "" } else { "s" }),
        }
    }
}

impl Ord for Window {
    /// Shortest first; one of unknown length last.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let key = |w: &Self| (w.hours().is_none(), w.hours(), matches!(w, Self::Other(_)));
        key(self).cmp(&key(other))
    }
}

impl PartialOrd for Window {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One quota window. `utilization` is a fraction; `resets_at` is unix seconds. What it measures
/// is said by the harness that read it: its window, the provider within the account whose
/// allowance it is, and the model or group within that allowance it counts alone.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meter {
    #[serde(default)]
    pub utilization: f64,
    #[serde(default, rename = "resetsAt", alias = "resets_at")]
    pub resets_at: Option<i64>,
    #[serde(default)]
    pub window: Window,
    /// The provider whose allowance it is, for a harness with one per provider (omp's
    /// `openai-codex`); none where the account has one allowance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model or group it counts alone, within that allowance (claude's `Fable`, antigravity's
    /// `Gemini`); none for the whole allowance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Meter {
    pub fn new(window: Window, utilization: f64, resets_at: Option<i64>) -> Self {
        Self { utilization, resets_at, window, provider: None, scope: None }
    }

    #[must_use]
    pub fn provider(self, provider: impl Into<String>) -> Self {
        Self { provider: Some(provider.into()), ..self }
    }

    #[must_use]
    pub fn scope(self, scope: impl Into<String>) -> Self {
        Self { scope: Some(scope.into()), ..self }
    }

    /// A meter as a reading from before meters said their window names it ([`legacy`]), for tests
    /// that fold such readings.
    #[cfg(test)]
    pub fn named(name: &str, utilization: f64, resets_at: Option<i64>) -> Self {
        Self { utilization, resets_at, ..legacy(name) }
    }

    /// Whether `model` names the meter's scope, so that the meter counts the model's use alone:
    /// `claude-fable-5-2` and `Fable`. A meter with no scope counts every model's.
    pub fn counts(&self, model: Option<&str>) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|s| model.is_some_and(|m| m.to_ascii_lowercase().contains(&s.to_ascii_lowercase())))
    }
}

/// Named meters of one home, such as `seven_day`, or `openai-codex/7 days`. The name is the
/// meter's identity; what it measures is in the meter.
pub type Meters = BTreeMap<String, Meter>;

/// What a meter named `name` measures, for a reading taken before meters said: the one place
/// the names the harnesses used are read. claude's and puku's (`five_hour`, `seven_day`,
/// `seven_day_opus`, `monthly`) and omp's (`provider/label`), each as its harness reads it now.
pub fn legacy(name: &str) -> Meter {
    match name.split_once('/') {
        Some((provider, label)) => omp::window_of(label).provider(provider),
        None => claude::window_of(name),
    }
}

/// The meters `raw` names, each that does not say what it measures named by [`legacy`]: how
/// readings already on the bus fold.
pub fn deserialize_meters<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Meters, D::Error> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        utilization: f64,
        #[serde(default, rename = "resetsAt", alias = "resets_at")]
        resets_at: Option<i64>,
        #[serde(default)]
        window: Option<Window>,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        scope: Option<String>,
    }
    let raw: BTreeMap<String, Wire> = serde::Deserialize::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(name, w)| {
            let m = match w.window {
                Some(window) => Meter { window, provider: w.provider, scope: w.scope, ..Meter::default() },
                None => legacy(&name),
            };
            (name, Meter { utilization: w.utilization, resets_at: w.resets_at, ..m })
        })
        .collect())
}

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
    /// A private directory for this task that the session writes: its session store.
    pub scratch: &'a Path,
    /// Where the host puts the files it hands the session (a prompt file): a [`crate::hostdir::HostDir`],
    /// which the session can at most read.
    pub inbox: &'a Path,
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
    /// Written before the run, each directly inside the task's [`Task::inbox`] (and refused anywhere
    /// else). The prompt goes here for a CLI that reads it from a file.
    pub files: Vec<(PathBuf, String)>,
    /// The argument naming the harness's command, when `program` is a script
    /// that hands over to it rather than the command itself. It is located as
    /// the program otherwise is ([`crate::config::HarnessConf::locate`]).
    pub command_arg: Option<usize>,
    /// Directories put first on the `PATH` it runs with, whatever else names one: its harness's
    /// helpers, as recorded ([`crate::config::Config::locate`]), so a launcher such as
    /// `#!/usr/bin/env node` runs the interpreter relay found, not one an earlier directory holds.
    pub path_first: Vec<PathBuf>,
}

impl Invocation {
    pub(crate) fn new(program: &str) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            stdin: None,
            files: Vec::new(),
            command_arg: None,
            path_first: Vec::new(),
        }
    }

    /// Run by `program` with `args`, the harness's `command` among them.
    pub(crate) fn script(program: &str, args: &[&str], command: &str) -> Self {
        let mut i = Self::new(program);
        i.args(args);
        i.command_arg = Some(i.args.len());
        i.arg(command);
        i
    }

    fn arg(&mut self, a: impl Into<OsString>) -> &mut Self {
        self.args.push(a.into());
        self
    }

    fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(&mut self, a: I) -> &mut Self {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    /// Writes [`Self::files`] into `inbox`, through its handle: a file named anywhere but directly
    /// inside it is refused, so no file of an invocation is ever written through a path a session controls.
    pub fn write_files(&self, inbox: &crate::hostdir::HostDir) -> std::io::Result<()> {
        for (path, text) in &self.files {
            let name = path
                .strip_prefix(inbox.path())
                .ok()
                .and_then(|rest| rest.to_str())
                .filter(|n| !n.is_empty() && !n.contains('/'))
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{} is not in the task's inbox {}", path.display(), inbox.path().display()),
                    )
                })?;
            inbox.write(name, text.as_bytes())?;
        }
        Ok(())
    }
}

/// What a path in a home is: what a placeholder for it is made as when it is not there yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
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
    /// Whether the line belongs in the job's log. A harness that streams
    /// partial updates, which a later event repeats in full, drops them.
    fn keep(&self, _line: &str) -> bool {
        true
    }
}

/// One harness. Implement it and add it to [`ALL`] to plug a CLI in.
pub trait Harness: Sync + fmt::Debug {
    fn id(&self) -> &'static str;
    /// Its program's name, as [`crate::config::Config::harness_programs`] looks it up: its id
    /// unless the CLI's command differs (puku's is `puku-cli`).
    fn program(&self) -> &'static str {
        self.id()
    }
    /// Other programs its runs need, by name: an interpreter a quota script runs on. Looked up and
    /// recorded with its program ([`crate::config::Config::harness_programs`]), so they are found
    /// wherever relay runs, and the only programs an [`Invocation::script`] may run.
    fn helpers(&self) -> &'static [&'static str] {
        &[]
    }
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
    /// be able to change what runs outside the sandbox. Each with what it is,
    /// since one the home does not have yet is made empty first, as that kind.
    fn protected_kinds(&self, _home: &Home) -> Vec<(PathBuf, Kind)> {
        Vec::new()
    }
    /// The paths of [`Self::protected_kinds`], for a test that asks only what is protected.
    #[cfg(test)]
    fn protected(&self, home: &Home) -> Vec<PathBuf> {
        self.protected_kinds(home).into_iter().map(|(p, _)| p).collect()
    }
    /// Who the home is signed in as, so homes that share one account share its quota.
    fn sign_in(&self, _home: &Home) -> SignIn {
        SignIn::Unknown
    }
    /// The address the home is signed in as, for a view to show its owner beside the account's name.
    /// Read from the home's files when the view is drawn and never kept: not in [`SignIn`], the
    /// state, the events or anything a run is given. None when the harness keeps none this crate
    /// reads, or the home is signed out.
    fn email(&self, _home: &Home) -> Option<String> {
        None
    }
    /// Whether the quota command's output says the session is expired,
    /// unauthorised or not signed in.
    fn lapsed(&self, _text: &str) -> bool {
        false
    }
    /// The command for `task` on `home`.
    fn invocation(&self, home: &Home, task: &Task<'_>) -> Result<Invocation, Refusal>;
    fn reader(&self) -> Box<dyn Reader>;
    /// A command printing the home's quota without a model call.
    fn quota(&self, _home: &Home) -> Option<Invocation> {
        None
    }
    /// `local` is the zone the quota command ran in, for a time that names none.
    fn parse_quota(&self, _text: &str, _now_s: i64, _local: &jiff::tz::TimeZone) -> Meters {
        Meters::new()
    }
    /// The meters that govern `model`. An error means they cannot be told,
    /// and a start on this home would be blind.
    fn meters_for<'m>(&self, model: Option<&str>, meters: &'m Meters) -> Result<Vec<&'m Meter>, Refusal>;
    /// Makes `session`, last written on `from`, resumable on `to`. Returns
    /// whether it can be resumed there; if not, the caller starts afresh.
    fn carry(&self, session: &str, from: Option<&Home>, to: &Home, scratch: &Path) -> bool;
    /// Makes `session`, last written on `from`, forkable on `to` by a session
    /// working in `cwd`. Returns whether it can be forked there.
    fn carry_fork(&self, _session: &str, _from: Option<&Home>, _to: &Home, _cwd: &Path) -> bool {
        false
    }
}

/// Every harness this crate knows, in a fixed order.
pub static ALL: [&dyn Harness; 3] = [&claude::Claude, &omp::Omp, &puku::Puku];

pub fn by_id(id: &str) -> Result<&'static dyn Harness, Refusal> {
    ALL.iter().copied().find(|h| h.id() == id).ok_or_else(|| {
        let names: Vec<_> = ALL.iter().map(|h| h.id()).collect();
        Refusal(format!("unknown agent: {id}. Supported: {}", names.join(", ")))
    })
}

/// Control characters (C0, DEL and C1, such as U+009B, a one-character CSI) replaced, so a value cannot forge
/// log lines or terminal escapes.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
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
            inbox: Path::new("/inbox"),
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

    /// Readings on the bus from before meters said what they measure fold as their harness reads
    /// them now; a reading that says keeps what it says.
    #[test]
    fn an_old_readings_meters_are_named_once_as_they_fold() {
        #[derive(serde::Deserialize)]
        struct Reading {
            #[serde(deserialize_with = "deserialize_meters")]
            meters: Meters,
        }
        let old = r#"{"account":"x","meters":{"seven_day":{"utilization":0.3},"five_hour":{"utilization":0.1},"monthly":{"utilization":0.0},"openai-codex/7 days":{"utilization":0.2},"google-antigravity/Gemini (Weekly)":{"utilization":0.1},"requests":{"utilization":0.5}}}"#;
        let m = serde_json::from_str::<Reading>(old).unwrap().meters;
        let said = |n: &str| (m[n].window, m[n].provider.as_deref(), m[n].scope.as_deref());
        assert_eq!(said("seven_day"), (Window::Week, None, None));
        assert_eq!(said("five_hour"), (Window::FiveHours, None, None));
        assert_eq!(said("monthly"), (Window::Month, None, None));
        assert_eq!(said("requests"), (Window::Other(None), None, None));
        assert_eq!(said("openai-codex/7 days"), (Window::Week, Some("openai-codex"), None));
        assert_eq!(
            said("google-antigravity/Gemini (Weekly)"),
            (Window::Week, Some("google-antigravity"), Some("Gemini"))
        );
        assert!((m["seven_day"].utilization - 0.3).abs() < 1e-9);
        // A new reading says what it measures, and that is kept, whatever its name.
        let new = Meters::from([("odd".to_owned(), Meter::new(Window::Month, 0.4, Some(9)).scope("Fable"))]);
        let text = serde_json::json!({ "meters": new }).to_string();
        assert_eq!(serde_json::from_str::<Reading>(&text).unwrap().meters, new, "{text}");
        assert!(text.contains(r#""window":"month""#), "{text}");
    }

    #[test]
    fn windows_are_ordered_shortest_first() {
        let mut w = vec![Window::Other(None), Window::Month, Window::of_hours(24), Window::Week, Window::FiveHours];
        w.sort();
        assert_eq!(w, [Window::FiveHours, Window::Other(Some(24)), Window::Week, Window::Month, Window::Other(None)]);
        assert_eq!(w.iter().map(|w| w.label()).collect::<Vec<_>>(), ["5 hours", "1 day", "7 days", "month", "other"]);
    }

    #[test]
    fn registry() {
        assert_eq!(by_id("omp").unwrap().id(), "omp");
        assert!(by_id("codex").unwrap_err().0.contains("Supported: claude, omp, puku"));
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
