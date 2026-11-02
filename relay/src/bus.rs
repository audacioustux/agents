//! The event bus. Each system owns its output port: an append-only file,
//! `bus/<system>.ndjson`, holding only that system's event type. A session
//! runner is a system of its own for one attempt at one job, so it writes
//! `bus/session/<job>.<attempt>.ndjson`, and nothing else writes there.
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
use std::fs::OpenOptions;
use std::io::Write;
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
    },
    Resumed {
        job: String,
        message: String,
        budget_usd: Option<f64>,
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
    Ended {
        job: String,
        outcome: Outcome,
        cost_usd: f64,
        turns: u32,
        reason: Option<String>,
    },
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

/// One attempt's session file, and back.
pub fn session_file(job: &str, attempt: u32) -> PathBuf {
    PathBuf::from(format!("session/{job}.{attempt}.ndjson"))
}

fn parse_session_file(name: &str) -> Option<(String, u32)> {
    let (job, attempt) = name.strip_suffix(".ndjson")?.rsplit_once('.')?;
    Some((job.to_owned(), attempt.parse().ok()?))
}

impl SessionEvent {
    /// The job the event is about, when it names one.
    pub fn job(&self) -> Option<&str> {
        match self {
            Self::Cloned { job, .. }
            | Self::Started { job, .. }
            | Self::Identified { job, .. }
            | Self::FollowUp { job, .. }
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
    /// For a session event, the job and attempt whose file it came from.
    pub from: Option<(String, u32)>,
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

fn read_file<E: Owned>(file: &Path, from: Option<&(String, u32)>, out: &mut Vec<(u64, usize, Stamped)>) -> Result<()> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    // A torn last line (a writer mid-append) or a foreign event type is skipped.
    out.extend(text.lines().enumerate().filter_map(|(i, l)| {
        let line = serde_json::from_str::<Line<E>>(l).ok()?;
        Some((line.t, i, Stamped { t: line.t, event: line.event.wrap(), from: from.cloned() }))
    }));
    Ok(())
}

/// Every event on the bus, oldest first.
pub fn read_all(bus: &Path) -> Result<Vec<Stamped>> {
    let mut all = Vec::new();
    read_file::<QueueEvent>(&bus.join(system_file("queue")), None, &mut all)?;
    read_file::<DispatchEvent>(&bus.join(system_file("dispatch")), None, &mut all)?;
    read_file::<QuotaEvent>(&bus.join(system_file("quota")), None, &mut all)?;
    read_file::<LandEvent>(&bus.join(system_file("land")), None, &mut all)?;
    // Before sessions had a file per attempt, they shared one; read it as is.
    read_file::<SessionEvent>(&bus.join(system_file("session")), None, &mut all)?;
    for e in std::fs::read_dir(bus.join("session")).into_iter().flatten().flatten() {
        if let Some(from) = parse_session_file(&e.file_name().to_string_lossy()) {
            read_file::<SessionEvent>(&e.path(), Some(&from), &mut all)?;
        }
    }
    all.sort_by_key(|(t, i, _)| (*t, *i));
    Ok(all.into_iter().map(|(_, _, s)| s).collect())
}

/// Forgets a job's session history: its attempts' files.
pub fn forget_sessions(bus: &Path, job: &str) {
    for e in std::fs::read_dir(bus.join("session")).into_iter().flatten().flatten() {
        if parse_session_file(&e.file_name().to_string_lossy()).is_some_and(|(j, _)| j == job) {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
    fn session_files_say_their_job_and_attempt() {
        let d = Dir::new();
        emit(d.path(), session_file("my-job.v2", 3), SessionEvent::SignedOut { account: "w".into() });
        emit(d.path(), session_file("my-job.v2", 4), SessionEvent::SignedOut { account: "w".into() });
        let evs = read_all(d.path()).unwrap();
        assert_eq!(evs[0].from, Some(("my-job.v2".into(), 3)));
        assert_eq!(evs[1].from, Some(("my-job.v2".into(), 4)));
        forget_sessions(d.path(), "my-job.v2");
        assert_eq!(read_all(d.path()).unwrap(), Vec::new());
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
