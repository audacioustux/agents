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
    /// The address the home is signed in as, read for a view alone ([`Harness::email`]).
    fn email(&self, _home: &Home) -> Option<String> {
        None
    }
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

    /// `oauthAccount.emailAddress`, when the home is signed in ([`Account::sign_in`] reads an account).
    fn email(&self, home: &Home) -> Option<String> {
        let file = read_json(&home.dir.join(".claude.json"))?;
        let account = file.get("oauthAccount")?;
        let field = |k: &str| account.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        field("accountUuid")?;
        field("emailAddress").map(crate::harness::printable)
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

    fn email(&self, home: &Home) -> Option<String> {
        self.account.email(home)
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
    // The home that wrote last holds the newest copy.
    let Some((key, src)) = from.filter(|f| *f != to).and_then(|f| nofollow::find(f, session)) else {
        return Ok(nofollow::find(to, session).is_some());
    };
    let dest = nofollow::make_dir(&nofollow::projects(to)?, key.as_c_str())?;
    nofollow::copy_transcript(&src, &dest, session)?;
    Ok(true)
}

/// The transcript, copied to where a session working in `cwd` finds it on
/// `to` (claude keys a project by its path, every other character a dash).
fn fork_transcript(session: &str, from: &Path, to: &Path, cwd: &Path) -> std::io::Result<bool> {
    let Some((src_key, src)) = nofollow::find(from, session) else {
        return Ok(false);
    };
    let key: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    if from == to && src_key.as_bytes() == key.as_bytes() {
        return Ok(true);
    }
    let dest = nofollow::make_dir(&nofollow::projects(to)?, key.as_str())?;
    nofollow::copy_transcript(&src, &dest, session)?;
    Ok(true)
}

/// Copying a transcript between homes without following a symlink (audit finding 3). A session
/// writes its own home, so any entry under `projects/` may be a link it planted: in the source, to
/// another account's credentials, which the host would copy where the session reads them; in the
/// destination, to a file the host would write through. So every directory is a handle opened with
/// `O_NOFOLLOW | O_DIRECTORY`, only regular files and directories are copied (anything else is
/// skipped), and each file at the destination is unlinked and made afresh with
/// `O_CREAT | O_EXCL | O_NOFOLLOW`.
mod nofollow {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
    use rustix::io::Errno;
    use rustix::path::Arg;

    fn dir_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    /// What `name` in `dir` is, without following a link.
    fn kind<P: Arg>(dir: impl AsFd, name: P) -> Option<FileType> {
        rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok().map(|st| FileType::from_raw_mode(st.st_mode))
    }

    /// The directory `name` in `dir`, refusing a symlink.
    fn open_dir<P: Arg>(dir: impl AsFd, name: P) -> io::Result<OwnedFd> {
        Ok(rustix::fs::openat(dir, name, dir_flags(), Mode::empty())?)
    }

    /// The directory `name` in `dir`: a directory already there is kept; anything else there (a
    /// file, a link) is removed and a directory made in its place.
    pub(super) fn make_dir<P: Arg + Copy>(dir: impl AsFd, name: P) -> io::Result<OwnedFd> {
        let dir = dir.as_fd();
        match kind(dir, name) {
            Some(FileType::Directory) | None => {}
            Some(_) => rustix::fs::unlinkat(dir, name, AtFlags::empty())?,
        }
        match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => open_dir(dir, name),
            Err(e) => Err(e.into()),
        }
    }

    /// `<home>/projects`, made when missing. The home is relay's own; `projects` may not be a link.
    pub(super) fn projects(home: &Path) -> io::Result<OwnedFd> {
        std::fs::create_dir_all(home)?;
        let home = rustix::fs::open(home, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())?;
        make_dir(&home, "projects")
    }

    /// `session` names one entry in a directory: not empty, no `/`, not `.` or `..`.
    fn bare(session: &str) -> bool {
        !(session.is_empty() || session == "." || session == ".." || session.contains(['/', '\0']))
    }

    /// The `projects/<key>` directory in `home` holding `<session>.jsonl` as a regular file: its
    /// key and a handle on it.
    pub(super) fn find(home: &Path, session: &str) -> Option<(CString, OwnedFd)> {
        if !bare(session) {
            return None;
        }
        let projects = rustix::fs::open(home.join("projects"), dir_flags(), Mode::empty()).ok()?;
        let file = format!("{session}.jsonl");
        Dir::read_from(&projects).ok()?.flatten().find_map(|e| {
            let key = e.file_name();
            if [&b"."[..], b".."].contains(&key.to_bytes()) {
                return None;
            }
            let dir = open_dir(&projects, key).ok()?;
            (kind(&dir, file.as_str()) == Some(FileType::RegularFile)).then(|| (key.to_owned(), dir))
        })
    }

    /// `<session>.jsonl` and its `<session>/` sidecar, from one key directory to another.
    /// The transcript itself must be copied: one swapped for a link or anything but a regular file
    /// since it was found is an error, never a transfer reported done. Sidecar entries may be skipped.
    pub(super) fn copy_transcript(from: &OwnedFd, to: &OwnedFd, session: &str) -> io::Result<()> {
        if !copy_file(from, to, format!("{session}.jsonl").as_str())? {
            return Err(io::Error::other(format!("{session}.jsonl is no longer a regular file")));
        }
        if kind(from, session) == Some(FileType::Directory) {
            copy_tree(&open_dir(from, session)?, &make_dir(to, session)?)?;
        }
        Ok(())
    }

    /// Copies the regular file `name`, with its permission bits; anything else there is skipped.
    /// Whether it was copied.
    fn copy_file<P: Arg + Copy>(from: &OwnedFd, to: &OwnedFd, name: P) -> io::Result<bool> {
        // `NONBLOCK` so a fifo swapped in is not waited on; it is skipped below.
        let read = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let src = match rustix::fs::openat(from, name, read, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::LOOP) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let st = rustix::fs::fstat(&src)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            return Ok(false);
        }
        match rustix::fs::unlinkat(to, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        let write = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = Mode::from_raw_mode(st.st_mode & 0o777);
        let dest = rustix::fs::openat(to, name, write, mode)?;
        // The umask filtered the mode `openat` was given; set it on the descriptor, never by name.
        rustix::fs::fchmod(&dest, mode)?;
        io::copy(&mut File::from(src), &mut File::from(dest))?;
        Ok(true)
    }

    fn copy_tree(from: &OwnedFd, to: &OwnedFd) -> io::Result<()> {
        for e in Dir::read_from(from)? {
            let e = e?;
            let name = e.file_name();
            if [&b"."[..], b".."].contains(&name.to_bytes()) {
                continue;
            }
            match kind(from, name) {
                Some(FileType::Directory) => copy_tree(&open_dir(from, name)?, &make_dir(to, name)?)?,
                Some(FileType::RegularFile) => _ = copy_file(from, to, name)?,
                _ => {}
            }
        }
        Ok(())
    }
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
    fn the_email_is_read_for_a_view_only_when_the_home_is_signed_in() {
        let d = std::env::temp_dir().join(format!("hb-email-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let h = Home { dir: d.clone(), ..home("claude", "w") };
        assert_eq!(Claude.email(&h), None, "no .claude.json");
        std::fs::write(d.join(".claude.json"), "not json").unwrap();
        assert_eq!(Claude.email(&h), None, "an unreadable file is no error");
        std::fs::write(d.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"x@y.invalid"}}"#).unwrap();
        assert_eq!(Claude.email(&h), None, "signed out: no accountUuid");
        std::fs::write(d.join(".claude.json"), r#"{"oauthAccount":{"accountUuid":"u-1","emailAddress":""}}"#).unwrap();
        assert_eq!(Claude.email(&h), None, "no address");
        let file = r#"{"oauthAccount":{"accountUuid":"u-1","emailAddress":"x@y.invalid\u001b[2J"}}"#;
        std::fs::write(d.join(".claude.json"), file).unwrap();
        assert_eq!(Claude.email(&h).as_deref(), Some("x@y.invalid [2J"), "no terminal escape gets through");
        let file = r#"{"oauthAccount":{"accountUuid":"u-1","emailAddress":"x@y.invalid\u009b2J"}}"#;
        std::fs::write(d.join(".claude.json"), file).unwrap();
        assert_eq!(Claude.email(&h).as_deref(), Some("x@y.invalid 2J"), "nor a one-character CSI (C1)");
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

    /// Two homes `a` and `b`, with `s1` in `a/projects/-repo`, and a secret outside both.
    fn homes_with_a_transcript(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let (a, b, secret) = (d.join("a"), d.join("b"), d.join("secret"));
        std::fs::create_dir_all(a.join("projects/-repo/s1")).unwrap();
        std::fs::write(a.join("projects/-repo/s1.jsonl"), "x").unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(&secret, "secret").unwrap();
        (a, b, secret)
    }

    /// Audit finding 3: a symlink in the sidecar is not copied as what it points at.
    fn a_sidecar_link_is_not_followed(copy: impl Fn(&Path, &Path) -> std::io::Result<bool>) {
        let d = crate::config::tempdir::Dir::new();
        let (a, b, secret) = homes_with_a_transcript(d.path());
        std::os::unix::fs::symlink(&secret, a.join("projects/-repo/s1/leak")).unwrap();
        assert!(copy(&a, &b).unwrap(), "the transcript is copied");
        assert_eq!(std::fs::read_to_string(b.join("projects/-repo/s1.jsonl")).unwrap(), "x", "with its data");
        let leak = b.join("projects/-repo/s1/leak");
        assert!(std::fs::read_to_string(&leak).map_or(true, |s| s != "secret"), "the secret was copied");
        assert!(std::fs::symlink_metadata(&leak).is_err(), "a link was copied");
    }

    /// Audit finding 3: a symlink planted at the destination is replaced, never written through.
    fn a_planted_destination_link_is_not_written_through(copy: impl Fn(&Path, &Path) -> std::io::Result<bool>) {
        let d = crate::config::tempdir::Dir::new();
        let (a, b, victim) = homes_with_a_transcript(d.path());
        std::fs::create_dir_all(b.join("projects/-repo")).unwrap();
        std::os::unix::fs::symlink(&victim, b.join("projects/-repo/s1.jsonl")).unwrap();
        assert!(copy(&a, &b).unwrap());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "secret");
        assert_eq!(std::fs::read_to_string(b.join("projects/-repo/s1.jsonl")).unwrap(), "x");
    }

    /// Review of #50: a transcript that is no longer a regular file when it is copied (a link
    /// swapped in after `find`) fails the copy rather than reporting a transfer done.
    #[test]
    fn a_transcript_that_is_not_a_regular_file_fails_the_copy() {
        use std::os::fd::OwnedFd;
        let d = crate::config::tempdir::Dir::new();
        let (a, b, secret) = homes_with_a_transcript(d.path());
        let key = a.join("projects/-repo");
        std::fs::remove_file(key.join("s1.jsonl")).unwrap();
        std::os::unix::fs::symlink(&secret, key.join("s1.jsonl")).unwrap();
        std::fs::create_dir_all(b.join("projects/-repo")).unwrap();
        let open = |p: &Path| -> OwnedFd { std::fs::File::open(p).unwrap().into() };
        assert!(nofollow::copy_transcript(&open(&key), &open(&b.join("projects/-repo")), "s1").is_err());
        assert!(std::fs::symlink_metadata(b.join("projects/-repo/s1.jsonl")).is_err(), "nothing copied");
    }

    /// Review of #50: a copied file keeps its source's permission bits whatever the umask.
    #[test]
    fn a_copied_transcript_keeps_its_mode_past_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let d = crate::config::tempdir::Dir::new();
        let (a, b, _) = homes_with_a_transcript(d.path());
        let src = a.join("projects/-repo/s1.jsonl");
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(carry_transcript("s1", Some(&a), &b).unwrap());
        let mode = std::fs::metadata(b.join("projects/-repo/s1.jsonl")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o666, "{mode:o}");
    }

    #[test]
    fn a_carry_does_not_copy_a_sidecar_link() {
        a_sidecar_link_is_not_followed(|a, b| carry_transcript("s1", Some(a), b));
    }

    #[test]
    fn a_carry_does_not_write_through_a_planted_link() {
        a_planted_destination_link_is_not_written_through(|a, b| carry_transcript("s1", Some(a), b));
    }

    #[test]
    fn a_fork_does_not_copy_a_sidecar_link() {
        a_sidecar_link_is_not_followed(|a, b| fork_transcript("s1", a, b, Path::new("/repo")));
    }

    #[test]
    fn a_fork_does_not_write_through_a_planted_link() {
        a_planted_destination_link_is_not_written_through(|a, b| fork_transcript("s1", a, b, Path::new("/repo")));
    }
}
