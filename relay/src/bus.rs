//! The event bus. Each system owns its output port: an append-only file,
//! `bus/<system>.ndjson`, holding only that system's event type. A session
//! runner is a system of its own for one attempt at one job, so it writes
//! `bus/session/<job>.<created>.<attempt>.ndjson`, and nothing else writes there.
//!
//! Ownership is enforced three times. A [`Port`] is built only from its
//! system's [`Owned::Source`], a capability whose constructor is private to
//! that system's module, so a second writer cannot be built (`TheGrid`'s
//! linear source capability). A line in a file that does not parse as that
//! file's event type is dropped on read. And the fold applies a session
//! event only from the job's current attempt: a runner that outlived its
//! attempt writes to a file nobody listens to, which is the fence.
//!
//! The files are the lossless record. Readers learn that something changed
//! from an inotify watch on the directory, which is only a doorbell.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use crate::harness::Meters;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QueueEvent {
    Queued {
        job: String,
        task: String,
        readonly: bool,
        /// Harness ids the job may run on. Empty: any enabled harness.
        #[serde(default)]
        harnesses: Vec<String>,
        /// Model per harness id; a harness not named here uses its configured default.
        #[serde(default)]
        models: BTreeMap<String, String>,
        effort: Option<String>,
        budget_usd: f64,
        /// An account (`harness:label`) the owner chose. A pinned job never moves.
        pinned: Option<String>,
        /// A read-only job's question at a clean commit; equal keys get the same kind of answer.
        #[serde(default)]
        reuse_key: Option<String>,
        /// Jobs that must be done before this one starts.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        after: Vec<String>,
        /// The profile the job was queued under; it stays with the job across resumes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    /// The job no longer waits for these jobs (empty: for any of them).
    Released {
        job: String,
        #[serde(default)]
        from: Vec<String>,
    },
    Resumed {
        job: String,
        message: String,
        budget_usd: Option<f64>,
        /// Models that replace the job's own, by harness, from this run on.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        models: std::collections::BTreeMap<String, String>,
    },
    Dequeued {
        job: String,
    },
    StopRequested {
        job: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DispatchEvent {
    Dispatched {
        job: String,
        account: String,
        runner_pid: u32,
        runner_started: u64,
    },
    /// The runner is gone without an `ended` event.
    Reaped {
        job: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Done,
    Failed,
    Stopped,
    /// Back in the queue, for another account or a later attempt.
    Retry,
    /// Back in the queue after a death with no output; counted against `limits.max_retries`.
    Rerun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionEvent {
    Cloned {
        job: String,
        workdir: PathBuf,
        branch: String,
    },
    /// An attempt began, on this account, in this session (new or resumed).
    Started {
        job: String,
        account: String,
        session_id: String,
        /// For a read-only job, the clean commit it reads; absent when the tree is dirty.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
    /// The harness named the session itself (omp chooses its own ids).
    Identified {
        job: String,
        session_id: String,
    },
    Rate {
        account: String,
        #[serde(alias = "windows")]
        meters: Meters,
    },
    Limited {
        account: String,
        until: i64,
    },
    SignedOut {
        account: String,
    },
    /// The session needs one more turn on its own account, such as a missing final report.
    FollowUp {
        job: String,
        message: String,
    },
    /// The run reached its cap and its progress is being checked before more is spent.
    Checking {
        job: String,
        checkpoint: u32,
    },
    /// A checkpoint's judgement, from the progress check or the zero-progress rule.
    Checked {
        job: String,
        check: Check,
    },
    Ended {
        job: String,
        outcome: Outcome,
        cost_usd: f64,
        turns: u32,
        reason: Option<String>,
    },
}

/// What a progress check decided at a spend cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckVerdict {
    /// On track: resume with a larger cap.
    Continue,
    /// Useful but off course: resume with a larger cap and the check's correction.
    Redirect,
    /// Not worth more: wait for a human.
    Stop,
}

impl CheckVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Redirect => "redirect",
            Self::Stop => "stop",
        }
    }
}

/// One checkpoint of a job: where its branch stood, and what was decided there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Counts from 1 over the job's whole life.
    pub checkpoint: u32,
    pub verdict: CheckVerdict,
    pub why: String,
    /// What the resumed session is told; empty on a stop.
    #[serde(default)]
    pub message: String,
    /// The next run's cap, when the job goes on.
    pub next_cap_usd: Option<f64>,
    /// The branch's commit at this checkpoint.
    pub head: Option<String>,
    /// The working tree at this checkpoint, committed or not, as a git tree id.
    pub tree: Option<String>,
    /// The account that judged; absent when no model was asked.
    pub checker: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Check {
    /// One line for the board: `checkpoint 2: continue → $30.00`.
    pub fn summary(&self) -> String {
        match self.next_cap_usd {
            Some(cap) => format!("checkpoint {}: {} → ${cap:.2}", self.checkpoint, self.verdict.as_str()),
            None => format!("checkpoint {}: {} — {}", self.checkpoint, self.verdict.as_str(), self.why),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QuotaEvent {
    AccountEnabled {
        account: String,
        enabled: bool,
    },
    /// A reading from the harness's quota command, which costs no model call.
    Reading {
        account: String,
        #[serde(alias = "windows")]
        meters: Meters,
        /// The home's sign-in (`Identity::stamp`) when it was read, so a new one is read again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sign_in: Option<String>,
    },
    /// The quota command's output says the session is expired, unauthorised or
    /// not signed in. A later reading that succeeds, or enabling the account, clears it.
    SignedOut {
        account: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sign_in: Option<String>,
    },
    /// The quota command gave nothing usable; the account stays unread, and
    /// is not asked again until a reading would be stale.
    Unreadable {
        account: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum LandEvent {
    Forgotten { job: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Queue(QueueEvent),
    Dispatch(DispatchEvent),
    Session(SessionEvent),
    Quota(QuotaEvent),
    Land(LandEvent),
}

/// Ties an event enum to the system that owns it and its file.
pub trait Owned: Serialize + DeserializeOwned {
    const SYSTEM: &'static str;
    /// The capability to write this port, constructible only by its system.
    type Source: Source;
    fn wrap(self) -> Event;
}

/// A port's capability names the file it writes, relative to the bus.
pub trait Source {
    fn file(&self) -> PathBuf;
}

macro_rules! owned {
    ($ty:ty, $system:literal, $variant:ident, $source:ty) => {
        impl Owned for $ty {
            const SYSTEM: &'static str = $system;
            type Source = $source;
            fn wrap(self) -> Event {
                Event::$variant(self)
            }
        }
    };
}
owned!(QueueEvent, "queue", Queue, crate::systems::queue::Source);
owned!(DispatchEvent, "dispatch", Dispatch, crate::systems::dispatch::Source);
owned!(SessionEvent, "session", Session, crate::systems::session::Source);
owned!(QuotaEvent, "quota", Quota, crate::systems::quota::Source);
owned!(LandEvent, "land", Land, crate::systems::land::Source);

/// The one file a whole-system port writes.
pub fn system_file(system: &str) -> PathBuf {
    PathBuf::from(format!("{system}.ndjson"))
}

/// Which session file an event came from: one attempt at one incarnation of
/// a job (a name cleaned and queued again is a new incarnation, told apart by
/// its `created_at`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub job: String,
    /// Absent in files named before incarnations were.
    pub created: Option<u64>,
    pub attempt: u32,
}

/// One attempt's session file. Job names have no dots, so the name parses back.
pub fn session_file(job: &str, created: u64, attempt: u32) -> PathBuf {
    PathBuf::from(format!("session/{job}.{created}.{attempt}.ndjson"))
}

fn parse_session_file(name: &str) -> Option<Origin> {
    let parts: Vec<&str> = name.strip_suffix(".ndjson")?.split('.').collect();
    let (job, created, attempt) = match parts[..] {
        [job, attempt] => (job, None, attempt),
        [job, created, attempt] => (job, Some(created.parse().ok()?), attempt),
        _ => return None,
    };
    Some(Origin { job: job.to_owned(), created, attempt: attempt.parse().ok()? })
}

impl SessionEvent {
    /// The job the event is about, when it names one.
    pub fn job(&self) -> Option<&str> {
        match self {
            Self::Cloned { job, .. }
            | Self::Started { job, .. }
            | Self::Identified { job, .. }
            | Self::FollowUp { job, .. }
            | Self::Checking { job, .. }
            | Self::Checked { job, .. }
            | Self::Ended { job, .. } => Some(job),
            Self::Rate { .. } | Self::Limited { .. } | Self::SignedOut { .. } => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Line<E> {
    /// Unix microseconds, so events from different systems merge in order.
    t: u64,
    #[serde(flatten)]
    event: E,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stamped {
    pub t: u64,
    pub event: Event,
    /// For a session event, the file it came from.
    pub from: Option<Origin>,
}

impl Stamped {
    /// The event as its bus line, with the system that owns it.
    pub fn to_json(&self) -> serde_json::Value {
        fn line<E: Owned>(t: u64, e: &E) -> serde_json::Value {
            let mut v = serde_json::to_value(e).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("t".into(), t.into());
                o.insert("system".into(), E::SYSTEM.into());
            }
            v
        }
        match &self.event {
            Event::Queue(e) => line(self.t, e),
            Event::Dispatch(e) => line(self.t, e),
            Event::Session(e) => line(self.t, e),
            Event::Quota(e) => line(self.t, e),
            Event::Land(e) => line(self.t, e),
        }
    }
}

pub fn now_micros() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros()).unwrap_or(u64::MAX)
}

/// Unix seconds, from bus time (microseconds), as meters name their resets.
pub fn secs(us: u64) -> i64 {
    i64::try_from(us / 1_000_000).unwrap_or(i64::MAX)
}

/// A system's output port, built only from its [`Owned::Source`].
#[derive(Debug)]
pub struct Port<E> {
    file: PathBuf,
    _event: PhantomData<E>,
}

impl<E: Owned> Port<E> {
    pub fn new(bus: &Path, source: &E::Source) -> Self {
        Self { file: bus.join(source.file()), _event: PhantomData }
    }

    pub fn emit(&self, event: E) -> Result<()> {
        let mut line = serde_json::to_vec(&Line { t: now_micros(), event })?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.file)
            .with_context(|| format!("opening {}", self.file.display()))?;
        // One write of a short line with O_APPEND: concurrent writers never interleave.
        let written = f.write(&line)?;
        anyhow::ensure!(written == line.len(), "short write to {}", self.file.display());
        Ok(())
    }
}

/// For each bus file (relative to the bus), how many bytes of it have been read.
pub type Offsets = BTreeMap<PathBuf, Mark>;

/// How far a file has been folded, and which file that was: a file replaced
/// under the same name (a job cleaned and queued again) has a new inode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub ino: u64,
    pub len: u64,
}

/// Reads `file`'s complete lines past `offset` into `out`, and returns the
/// mark just past the last `\n`: a torn last line (a writer mid-append) is
/// left for the next read. `None` when the file is not the one read before
/// (shorter, or another inode), so what was read no longer describes it.
/// A missing file reads as empty. A line that is not `E` is skipped.
fn read_file<E: Owned>(file: &Path, from: Option<&Origin>, seen: Mark, out: &mut Vec<Stamped>) -> Result<Option<Mark>> {
    use std::os::unix::fs::MetadataExt;
    let mut f = match File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((seen.len == 0).then_some(Mark::default())),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let meta = f.metadata().with_context(|| format!("reading {}", file.display()))?;
    if seen.len > 0 && (meta.ino() != seen.ino || meta.len() < seen.len) {
        return Ok(None);
    }
    let offset = seen.len;
    let mut bytes = Vec::new();
    f.seek(SeekFrom::Start(offset))
        .and_then(|_| f.read_to_end(&mut bytes))
        .with_context(|| format!("reading {}", file.display()))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&bytes[..complete]);
    out.extend(text.lines().filter_map(|l| {
        let line = serde_json::from_str::<Line<E>>(l).ok()?;
        Some(Stamped { t: line.t, event: line.event.wrap(), from: from.cloned() })
    }));
    Ok(Some(Mark { ino: meta.ino(), len: offset + complete as u64 }))
}

/// Every event on the bus, oldest first.
pub fn read_all(bus: &Path) -> Result<Vec<Stamped>> {
    Ok(read_since(bus, &Offsets::new())?.map(|(events, _)| events).unwrap_or_default())
}

/// The events written since `offsets` were taken, oldest first, with the
/// offsets to read from next time. `None` if a file is not the one read
/// before, when only a full read from empty offsets makes sense.
pub fn read_since(bus: &Path, offsets: &Offsets) -> Result<Option<(Vec<Stamped>, Offsets)>> {
    type Read = fn(&Path, Option<&Origin>, Mark, &mut Vec<Stamped>) -> Result<Option<Mark>>;
    let systems: [(&str, Read); 5] = [
        ("queue", read_file::<QueueEvent>),
        ("dispatch", read_file::<DispatchEvent>),
        ("quota", read_file::<QuotaEvent>),
        ("land", read_file::<LandEvent>),
        // Before sessions had a file per attempt, they shared one; read it as is.
        ("session", read_file::<SessionEvent>),
    ];
    let mut sessions: Vec<(PathBuf, Origin)> = std::fs::read_dir(bus.join("session"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse_session_file(&name).map(|o| (Path::new("session").join(name), o))
        })
        .collect();
    // A fixed file order, so a stable sort by time breaks ties the same way in every read.
    sessions.sort_by(|a, b| a.0.cmp(&b.0));
    let files = systems
        .into_iter()
        .map(|(sys, read)| (system_file(sys), None, read))
        .chain(sessions.into_iter().map(|(rel, o)| (rel, Some(o), read_file::<SessionEvent> as Read)));
    let (mut all, mut next) = (Vec::new(), Offsets::new());
    for (rel, origin, read) in files {
        let seen = offsets.get(&rel).copied().unwrap_or_default();
        let Some(mark) = read(&bus.join(&rel), origin.as_ref(), seen, &mut all)? else { return Ok(None) };
        next.insert(rel, mark);
    }
    all.sort_by_key(|s| s.t);
    Ok(Some((all, next)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    /// Writes a line to a port's file directly: tests stand outside the systems.
    fn emit<E: Owned>(bus: &Path, file: PathBuf, e: E) {
        let p = bus.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let line = serde_json::to_string(&Line { t: now_micros(), event: e }).unwrap();
        let old = std::fs::read_to_string(&p).unwrap_or_default();
        std::fs::write(&p, format!("{old}{line}\n")).unwrap();
    }

    #[test]
    fn round_trip_and_ownership() {
        let d = Dir::new();
        emit(d.path(), system_file("queue"), QueueEvent::Dequeued { job: "a".into() });
        emit(d.path(), system_file("land"), LandEvent::Forgotten { job: "a".into() });
        // A queue event written into the land file is not land's to emit: dropped.
        std::fs::write(
            d.path().join("land.ndjson"),
            format!(
                "{}\n{{\"t\":1,\"type\":\"dequeued\",\"job\":\"x\"}}\n{{\"t\":2,\"type\":",
                std::fs::read_to_string(d.path().join("land.ndjson")).unwrap().trim()
            ),
        )
        .unwrap();
        let evs = read_all(d.path()).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0].event, Event::Queue(QueueEvent::Dequeued { .. })));
        assert!(matches!(evs[1].event, Event::Land(LandEvent::Forgotten { .. })));
    }

    #[test]
    fn session_files_say_their_job_incarnation_and_attempt() {
        let d = Dir::new();
        emit(d.path(), session_file("my-job", 17, 3), SessionEvent::SignedOut { account: "w".into() });
        emit(d.path(), PathBuf::from("session/my-job.4.ndjson"), SessionEvent::SignedOut { account: "w".into() });
        let from: Vec<_> = read_all(d.path()).unwrap().into_iter().map(|e| e.from.unwrap()).collect();
        assert_eq!(from[0], Origin { job: "my-job".into(), created: Some(17), attempt: 3 });
        assert_eq!(from[1], Origin { job: "my-job".into(), created: None, attempt: 4 }, "named before incarnations");
    }

    #[test]
    fn events_print_as_json_lines() {
        let s = Stamped { t: 5, event: Event::Land(LandEvent::Forgotten { job: "j".into() }), from: None };
        assert_eq!(s.to_json(), serde_json::json!({"t":5,"system":"land","type":"forgotten","job":"j"}));
    }

    #[test]
    fn line_format_is_flat() {
        let l = Line { t: 5, event: SessionEvent::SignedOut { account: "w".into() } };
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"{"t":5,"type":"signed-out","account":"w"}"#);
    }
}
