//! Claude Code, and the CLIs built from it that keep its protocol (puku-cli).
//! One [`ClaudeCode`] serves them all, told apart by the program, the prefix
//! of their homes, the variable that selects one and where the sign-in and
//! the quota come from ([`Account`]).
//!
//! Claude Code's homes are config dirs (`~/.claude`, `~/.claude.<label>`,
//! selected by `CLAUDE_CONFIG_DIR`); quota comes from `claude -p /usage`
//! and from `rate_limit_event`s; transcripts live in the home, under
//! `projects/<cwd key>/<session>.jsonl`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::harness::report::{Report, parse_reset};
use crate::harness::{
    Caps, Harness, Home, Identity, Invocation, Kind, Meter, Meters, Permissions, Reader, Refusal, Session, SignIn,
    Task, read_json,
};

const PREFIX: &str = ".claude";

/// A Claude Code CLI: Claude Code itself, or one built from it.
#[derive(Debug)]
pub struct ClaudeCode {
    pub id: &'static str,
    /// Its command.
    pub program: &'static str,
    /// Homes are `<user home>/<prefix>` (`default`) and `<prefix>.<label>`.
    pub prefix: &'static str,
    /// The variable naming the home a run uses.
    pub config_env: &'static str,
    pub identity: &'static str,
    /// The flag that turns every permission check off.
    pub skip_permissions: &'static str,
    /// The files in a home the host later runs or obeys, besides executable scripts.
    pub config_files: &'static [&'static str],
    /// The directories in a home the host later runs or obeys.
    pub config_dirs: &'static [&'static str],
    pub account: &'static dyn Account,
}

/// Where a Claude Code CLI keeps its sign-in, and how its quota is read.
pub trait Account: Sync + fmt::Debug {
    /// Whether the home's sign-in can renew itself. One that cannot expires
    /// within hours, so it is not offered.
    fn can_refresh(&self, home: &Home) -> bool;
    fn sign_in(&self, home: &Home) -> SignIn;
    fn lapsed(&self, text: &str) -> bool;
    /// Variables a run on `home` needs besides the one naming the home: where its sign-in is, when
    /// that is not in the home. Every run takes them through [`ClaudeCode::run_env`].
    fn env(&self, _home: &Home) -> Vec<(OsString, Option<OsString>)> {
        Vec::new()
    }
    /// Where every home's sign-in lives when that is outside the home.
    fn sign_in_paths(&self, _user_home: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
    /// Where this home's sign-in lives when that is outside the home. A run
    /// writes it when it renews the sign-in.
    fn sign_in_dir(&self, _home: &Home) -> Option<PathBuf> {
        None
    }
    fn quota(&self, cli: &ClaudeCode, home: &Home) -> Invocation;
    fn parse_quota(&self, text: &str, now_s: i64) -> Meters;
    /// Programs its quota reading runs besides the CLI ([`Harness::helpers`]).
    fn helpers(&self) -> &'static [&'static str] {
        &[]
    }
}

#[allow(non_upper_case_globals)]
pub const Claude: ClaudeCode = ClaudeCode {
    id: "claude",
    program: "claude",
    prefix: PREFIX,
    config_env: "CLAUDE_CONFIG_DIR",
    identity: "You are Claude",
    skip_permissions: "--dangerously-skip-permissions",
    config_files: &["settings.json", "settings.local.json", "CLAUDE.md"],
    config_dirs: &["hooks", "plugins", "skills", "commands", "agents", "output-styles"],
    account: &ClaudeAccount,
};

impl ClaudeCode {
    /// What every run on `home` (a session, a quota reading and whatever that starts) is told about
    /// its account: the home, and where the sign-in is ([`Account::env`]). Built here alone, so no run
    /// can look for the account somewhere relay's own reading of it ([`Account::sign_in`]) did not.
    pub fn run_env(&self, home: &Home) -> Vec<(OsString, Option<OsString>)> {
        std::iter::once((self.config_env.into(), Some(home.dir.clone().into()))).chain(self.account.env(home)).collect()
    }
}

/// `.claude` is `default`; `.claude.work` is `work`.
pub fn label_for(entry: &str, prefix: &str) -> Option<String> {
    let rest = entry.strip_prefix(prefix)?;
    if rest.is_empty() {
        return Some("default".into());
    }
    let label = rest.strip_prefix('.')?;
    (!label.is_empty()).then(|| label.to_owned())
}

pub(crate) fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().concat()
}

/// Claude Code's own sign-in: `.credentials.json` and `.claude.json` in the home.
#[derive(Debug)]
pub struct ClaudeAccount;

impl Account for ClaudeAccount {
    fn can_refresh(&self, home: &Home) -> bool {
        read_json(&home.dir.join(".credentials.json"))
            .and_then(|v| v.pointer("/claudeAiOauth/refreshToken").and_then(Value::as_str).map(|t| !t.is_empty()))
            .unwrap_or(false)
    }

    fn sign_in(&self, home: &Home) -> SignIn {
        let file = read_json(&home.dir.join(".claude.json"));
        let account = file.as_ref().and_then(|v| v.get("oauthAccount"));
        let field = |k: &str| account.and_then(|a| a.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(uuid) = field("accountUuid") else { return SignIn::Out };
        let plan = field("planDisplayName").or(field("billingType")).unwrap_or_default().to_owned();
        let fetched = account.and_then(|a| a.get("profileFetchedAt")).map(Value::to_string).unwrap_or_default();
        let stamp = digest(
            &[uuid, field("organizationUuid").unwrap_or(""), &plan, field("billingType").unwrap_or(""), &fetched]
                .join("\n"),
        );
        SignIn::As(Identity { plan, id: digest(uuid)[..6].to_owned(), stamp: stamp[..12].to_owned() })
    }

    fn lapsed(&self, text: &str) -> bool {
        static WORDS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?i)not logged in|please run /login|(session|token|login|sign-?in|credentials?)[^\n]{0,40}expired|expired[^\n]{0,40}(session|token|login)|unauthori[sz]ed|authentication_error|invalid (api key|authentication)|failed to authenticate",
            )
            .expect("regex")
        });
        WORDS.is_match(&strip_ansi(text))
    }

    fn quota(&self, cli: &ClaudeCode, home: &Home) -> Invocation {
        let mut i = Invocation::new(cli.program);
        i.args(["-p", "/usage", "--strict-mcp-config", "--setting-sources", "project,local"]);
        i.env.extend(cli.run_env(home));
        i
    }

    fn parse_quota(&self, text: &str, now_s: i64) -> Meters {
        static LINE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^Current (week \(all models\)|session):\s+(\d+)% used(?:\s+·\s+resets\s+(.+?))?\s*$")
                .expect("regex")
        });
        let clean = strip_ansi(text);
        let mut out = Meters::new();
        for line in clean.lines() {
            let Some(c) = LINE.captures(line.trim()) else { continue };
            let key = if c[1].starts_with("week") { "seven_day" } else { "five_hour" };
            let used: f64 = c[2].parse().unwrap_or(0.0);
            let resets_at = c.get(3).and_then(|r| parse_reset(&format!("resets {}", r.as_str()), now_s));
            out.insert(key.into(), Meter { utilization: used / 100.0, resets_at });
        }
        out
    }
}

/// Tools that schedule the session's own later turns. Nothing is there to take
/// a later turn of a `-p` run: it only spends tokens, and its closing message
/// replaces the report the session already gave.
const SELF_SCHEDULING: &str = "ScheduleWakeup,CronCreate,CronDelete,RemoteTrigger";

fn session_args(i: &mut Invocation, task: &Task<'_>, skip_permissions: &str) {
    i.arg("-p");
    if task.events {
        // stream-json keeps every result, so a later wake-up cannot overwrite
        // the report, and it needs --verbose in -p mode.
        i.args(["--output-format", "stream-json", "--verbose"]);
    }
    if task.lean {
        i.args(["--strict-mcp-config", "--setting-sources", "project,local", "--disable-slash-commands"]);
    }
    match task.permissions {
        Permissions::Unrestricted => i.arg(skip_permissions),
        Permissions::ReadOnly => i.args(["--permission-mode", "plan"]),
        Permissions::EditsOnly => i.args(["--permission-mode", "acceptEdits"]),
    };
    i.args(["--disallowedTools", SELF_SCHEDULING]);
    if let Some(b) = task.budget_usd {
        i.args(["--max-budget-usd".to_owned(), format!("{b}")]);
    }
    if let Some(b) = task.briefing {
        i.args(["--append-system-prompt", b]);
    }
    // Model and effort go on resumes too, or a resume runs on the home's default model.
    if let Some(m) = task.model {
        i.args(["--model", m]);
    }
    if let Some(e) = task.effort {
        i.args(["--effort", e]);
    }
    if let Some(n) = task.name {
        i.args(["--name", n]);
    }
    // --continue is never built: it picks the latest session anywhere.
    match task.session {
        Session::New(id) => i.args(["--session-id", id]),
        Session::Resume(id) => i.args(["--resume", id]),
        Session::Fork(id) => i.args(["--resume", id, "--fork-session"]),
    };
    // `-p` with no prompt argument reads it from stdin: never argv, which is
    // public in /proc and limited in size.
    i.stdin = Some(task.prompt.to_owned());
}

impl Harness for ClaudeCode {
    fn id(&self) -> &'static str {
        self.id
    }

    fn program(&self) -> &'static str {
        self.program
    }

    fn helpers(&self) -> &'static [&'static str] {
        self.account.helpers()
    }

    fn caps(&self) -> Caps {
        Caps { read_only_by_capability: true, fork: true, preset_session_id: true, budget: true, quota: true }
    }

    fn identity(&self) -> &'static str {
        self.identity
    }

    fn homes(&self, user_home: &Path) -> Vec<Home> {
        let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(user_home)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| label_for(&name, self.prefix).map(|l| (l, user_home.join(&name))))
            .filter(|(_, d)| d.is_dir())
            .collect();
        entries.sort();
        entries
            .into_iter()
            .map(|(label, dir)| Home { harness: self.id, label, dir })
            .filter(|h| self.account.can_refresh(h))
            .collect()
    }

    fn home_paths(&self, user_home: &Path) -> Vec<PathBuf> {
        // Also ~/.claude.json, the default home's account file.
        std::fs::read_dir(user_home)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(self.prefix))
            .map(|e| e.path())
            .chain(self.account.sign_in_paths(user_home))
            .collect()
    }

    fn writable(&self, home: &Home, _user_home: &Path) -> Vec<PathBuf> {
        std::iter::once(home.dir.clone()).chain(self.account.sign_in_dir(home)).collect()
    }

    fn protected_kinds(&self, home: &Home) -> Vec<(PathBuf, Kind)> {
        // `.claude.json` (which can name MCP servers) stays writable: claude
        // replaces it by rename, which a read-only mount would break.
        let scripts = std::fs::read_dir(&home.dir).into_iter().flatten().flatten().filter(|e| {
            use std::os::unix::fs::PermissionsExt;
            let p = e.path();
            p.is_file()
                && (p.extension().is_some_and(|x| x == "sh")
                    || e.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
        });
        let files = self.config_files.iter().map(|c| (home.dir.join(c), Kind::File));
        let dirs = self.config_dirs.iter().map(|c| (home.dir.join(c), Kind::Dir));
        files.chain(dirs).chain(scripts.map(|e| (e.path(), Kind::File))).collect()
    }

    fn sign_in(&self, home: &Home) -> SignIn {
        self.account.sign_in(home)
    }

    fn lapsed(&self, text: &str) -> bool {
        self.account.lapsed(text)
    }

    fn invocation(&self, home: &Home, task: &Task<'_>) -> Result<Invocation, Refusal> {
        let mut i = Invocation::new(self.program);
        session_args(&mut i, task, self.skip_permissions);
        i.env.extend(self.run_env(home));
        i.env.push(("DISABLE_AUTOUPDATER".into(), Some("1".into())));
        Ok(i)
    }

    fn reader(&self) -> Box<dyn Reader> {
        Box::new(StreamReader::default())
    }

    fn quota(&self, home: &Home) -> Option<Invocation> {
        Some(self.account.quota(self, home))
    }

    fn parse_quota(&self, text: &str, now_s: i64) -> Meters {
        self.account.parse_quota(text, now_s)
    }

    fn meters_for<'m>(&self, _model: Option<&str>, meters: &'m Meters) -> Result<Vec<&'m Meter>, Refusal> {
        // One allowance per account, whatever the model.
        Ok(meters.values().collect())
    }

    fn carry(&self, session: &str, from: Option<&Home>, to: &Home, _scratch: &Path) -> bool {
        carry_transcript(session, from.map(|h| h.dir.as_path()), &to.dir).unwrap_or(false)
    }

    fn carry_fork(&self, session: &str, from: Option<&Home>, to: &Home, cwd: &Path) -> bool {
        from.is_some_and(|f| fork_transcript(session, &f.dir, &to.dir, cwd).unwrap_or(false))
    }
}

pub(crate) fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").expect("regex"));
    ANSI.replace_all(text, "")
}

/// A transcript is `<home>/projects/<cwd key>/<session>.jsonl`, plus a
/// `<session>/` sidecar dir when present. A sandbox keeps the cwd at the same
/// path, so the key is the same, and copying the files moves the session.
fn carry_transcript(session: &str, from: Option<&Path>, to: &Path) -> std::io::Result<bool> {
    let find = |dir: &Path| -> Option<(PathBuf, PathBuf)> {
        std::fs::read_dir(dir.join("projects")).ok()?.flatten().find_map(|e| {
            let file = e.path().join(format!("{session}.jsonl"));
            file.is_file().then(|| (e.path(), file))
        })
    };
    // The home that wrote last holds the newest copy.
    let Some((key_dir, file)) = from.filter(|f| *f != to).and_then(find) else { return Ok(find(to).is_some()) };
    let dest = to.join("projects").join(key_dir.file_name().unwrap_or_default());
    std::fs::create_dir_all(&dest)?;
    std::fs::copy(&file, dest.join(format!("{session}.jsonl")))?;
    let sidecar = key_dir.join(session);
    if sidecar.is_dir() {
        copy_dir(&sidecar, &dest.join(session))?;
    }
    Ok(true)
}

/// The transcript, copied to where a session working in `cwd` finds it on
/// `to` (claude keys a project by its path, every other character a dash).
fn fork_transcript(session: &str, from: &Path, to: &Path, cwd: &Path) -> std::io::Result<bool> {
    let Some(src) = std::fs::read_dir(from.join("projects"))?
        .flatten()
        .map(|e| e.path())
        .find(|d| d.join(format!("{session}.jsonl")).is_file())
    else {
        return Ok(false);
    };
    let key: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let dest = to.join("projects").join(key);
    if dest == src {
        return Ok(true);
    }
    std::fs::create_dir_all(&dest)?;
    std::fs::copy(src.join(format!("{session}.jsonl")), dest.join(format!("{session}.jsonl")))?;
    if src.join(session).is_dir() {
        copy_dir(&src.join(session), &dest.join(session))?;
    }
    Ok(true)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// `--output-format stream-json --verbose`, and the single result object
/// of `--output-format json`.
#[derive(Debug, Default)]
pub(crate) struct StreamReader {
    report: Report,
    last_text: Option<String>,
}

impl StreamReader {
    fn result(&mut self, v: &Value) {
        let r = &mut self.report;
        let text = v.get("result").and_then(Value::as_str).unwrap_or("");
        let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
        r.ended = true;
        r.session_id = v.get("session_id").and_then(Value::as_str).map(str::to_owned).or(r.session_id.take());
        r.cost_usd = v.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(r.cost_usd);
        r.turns = v.get("num_turns").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(r.turns);
        // `is_error` can be true with subtype "success" (an auth failure is reported that way).
        r.is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false) || !matches!(subtype, "" | "success");
        r.budget_hit = subtype == "error_max_budget_usd";
        r.api_status = v.get("api_error_status").and_then(Value::as_u64);
        let api_error = v.get("api_error").and_then(Value::as_str).unwrap_or("");
        r.error_text = if r.is_error { format!("{text} {api_error}").trim().to_owned() } else { String::new() };
        // A wake-up (a background task finishing after the report) adds a later
        // result; the report is the last one that is not a wake-up.
        if v.pointer("/origin/kind").and_then(Value::as_str) != Some("task-notification") {
            r.answer =
                if text.trim().is_empty() { self.last_text.clone().unwrap_or_default() } else { text.to_owned() };
        }
    }
}

impl Reader for StreamReader {
    fn line(&mut self, line: &str) -> Option<Meters> {
        let v: Value = serde_json::from_str(line).ok()?;
        self.report.lines += 1;
        match v.get("type").and_then(Value::as_str) {
            Some("system") => {
                self.report.session_id = v.get("session_id").and_then(Value::as_str).map(str::to_owned);
            }
            Some("rate_limit_event") => {
                let m: Meters = serde_json::from_value(v.pointer("/rate_limit_info/unifiedWindows")?.clone()).ok()?;
                self.report.meters = Some(m.clone());
                return Some(m);
            }
            // The main agent's words, not a subagent's.
            Some("assistant") if v.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                let text: Vec<&str> = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                if !text.is_empty() {
                    self.last_text = Some(text.join("\n"));
                }
            }
            Some("result") => self.result(&v),
            _ => {}
        }
        None
    }

    fn report(&self) -> &Report {
        &self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{argv, home, task};

    fn read(lines: &[&str]) -> Report {
        let mut r = Claude.reader();
        for l in lines {
            r.line(l);
        }
        r.report().clone()
    }

    #[test]
    fn argv_shapes() {
        let s = Path::new("/s");
        let h = home("claude", "w");
        let fresh = Claude.invocation(&h, &task("p", Session::New("id"), Permissions::Unrestricted, s)).unwrap();
        let a = argv(&fresh);
        assert!(a.contains("--session-id id") && a.contains("--dangerously-skip-permissions"), "{a}");
        assert!(a.contains("--output-format stream-json --verbose") && a.contains("--max-budget-usd 2.5"));
        assert!(a.contains("--model m") && a.contains("--effort high") && a.contains("--strict-mcp-config"));
        assert!(a.contains("--disallowedTools ScheduleWakeup,CronCreate"), "no self-scheduled turns: {a}");
        assert_eq!(fresh.stdin.as_deref(), Some("p"));
        assert!(fresh.env.contains(&("CLAUDE_CONFIG_DIR".into(), Some("/h/w".into()))));
        let fork = argv(&Claude.invocation(&h, &task("p", Session::Fork("o"), Permissions::ReadOnly, s)).unwrap());
        assert!(fork.contains("--resume o --fork-session") && fork.contains("--permission-mode plan"));
        assert!(!fork.split(' ').any(|x| x == "--continue" || x == "-c"));
    }

    #[test]
    fn stream_reading() {
        let r = read(&[
            r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"seven_day":{"utilization":0.23,"resetsAt":1794150000}}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"FINAL main"}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"t","message":{"content":[{"type":"text","text":"subagent"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"","num_turns":3,"total_cost_usd":0.5,"session_id":"s1"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","origin":{"kind":"task-notification"}}"#,
        ]);
        assert_eq!(r.answer, "FINAL main");
        assert_eq!(r.session_id.as_deref(), Some("s1"));
        assert!((r.meters.unwrap()["seven_day"].utilization - 0.23).abs() < 1e-9);
        let auth =
            read(&[r#"{"type":"result","subtype":"success","is_error":true,"result":"Failed to authenticate"}"#]);
        assert!(auth.is_error && auth.error_text.contains("authenticate"));
        let budget = read(&[r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":""}"#]);
        assert!(budget.budget_hit);
    }

    #[test]
    fn usage() {
        let text = "\x1b[1mCurrent week (all models): 24% used · resets Nov 8, 3pm (UTC)\x1b[0m\nCurrent week (Fable): 0% used\nCurrent session: 61% used · resets 7pm (UTC)\n";
        let m = Claude.parse_quota(text, 1_793_631_600);
        assert_eq!(m.len(), 2);
        assert!((m["seven_day"].utilization - 0.24).abs() < 1e-9);
        assert_eq!(m["five_hour"].resets_at, Some(1_793_631_600 + 4 * 3600));
    }

    #[test]
    fn sign_in_is_read_from_the_home_and_never_carries_the_email_or_uuid() {
        let d = std::env::temp_dir().join(format!("hb-signin-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let h = Home { dir: d.clone(), ..home("claude", "w") };
        assert_eq!(Claude.sign_in(&h), SignIn::Out, "no .claude.json");
        std::fs::write(d.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"x@y.invalid"}}"#).unwrap();
        assert_eq!(Claude.sign_in(&h), SignIn::Out, "no accountUuid");
        let file = |uuid: &str, plan: &str| {
            format!(
                r#"{{"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"x@y.invalid","planDisplayName":"{plan}","profileFetchedAt":7}}}}"#
            )
        };
        std::fs::write(d.join(".claude.json"), file("76de9d00-aaaa-4bbb-8ccc-000000000001", "Ultra")).unwrap();
        let SignIn::As(id) = Claude.sign_in(&h) else { panic!("signed in") };
        assert_eq!((id.plan.as_str(), id.id.len()), ("Ultra", 6));
        assert!(id.id.chars().all(|c| c.is_ascii_hexdigit()));
        let shown = format!("{id:?} {}", serde_json::to_string(&id).unwrap());
        assert!(!shown.contains("x@y.invalid") && !shown.contains("76de9d00-aaaa"), "{shown}");
        std::fs::write(d.join(".claude.json"), file("76de9d00-aaaa-4bbb-8ccc-000000000001", "Max")).unwrap();
        let SignIn::As(later) = Claude.sign_in(&h) else { panic!("signed in") };
        assert_eq!(later.id, id.id);
        assert_ne!(later.stamp, id.stamp, "a changed plan is a changed sign-in");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn expired_sessions_are_recognised_in_a_usage_output() {
        for text in [
            "Not logged in · Please run /login",
            "\x1b[31mYour session has expired. Please run /login\x1b[0m",
            "OAuth token has expired",
            "API Error: 401 {\"type\":\"authentication_error\"}",
            "Failed to authenticate",
        ] {
            assert!(Claude.lapsed(text), "{text}");
        }
        for text in ["Current week (all models): 24% used · resets Nov 8, 3pm (UTC)\n", "Total cost: $0.0000\n", ""] {
            assert!(!Claude.lapsed(text), "{text}");
        }
    }

    #[test]
    fn labels() {
        assert_eq!(label_for(".claude", PREFIX).as_deref(), Some("default"));
        assert_eq!(label_for(".claude.work", PREFIX).as_deref(), Some("work"));
        assert_eq!(label_for(".claudex", PREFIX), None);
    }

    #[test]
    fn transcript_moves_with_its_sidecar() {
        let d = std::env::temp_dir().join(format!("hb-carry-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        let key = a.join("projects/-repo");
        std::fs::create_dir_all(key.join("s1/sub")).unwrap();
        std::fs::write(key.join("s1.jsonl"), "x").unwrap();
        std::fs::write(key.join("s1/sub/f"), "y").unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (ha, hb) = (Home { dir: a, ..home("claude", "a") }, Home { dir: b.clone(), ..home("claude", "b") });
        assert!(!Claude.carry("s2", Some(&ha), &hb, &d));
        assert!(Claude.carry("s1", Some(&ha), &hb, &d));
        assert_eq!(std::fs::read_to_string(b.join("projects/-repo/s1/sub/f")).unwrap(), "y");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
