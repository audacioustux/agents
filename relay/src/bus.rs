//! The event bus. Each system owns one output port: an append-only file,
//! `bus/<system>.ndjson`, holding only that system's event type. Ownership
//! is enforced twice: a system can only hold a [`Port`] for its own event
//! enum, and a line in a file that does not parse as that file's enum is
//! dropped on read.
//!
//! The files are the lossless record. Readers learn that something changed
//! from an inotify watch on the directory, which is only a doorbell.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub type Windows = BTreeMap<String, Window>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub utilization: f64,
    /// Unix seconds.
    #[serde(default, rename = "resetsAt", alias = "resets_at")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum QueueEvent {
    Queued {
        job: String,
        task: String,
        readonly: bool,
        model: Option<String>,
        effort: Option<String>,
        budget_usd: f64,
        /// An account the owner chose. A pinned job never moves.
        pinned: Option<String>,
        session_id: String,
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
    Started {
        job: String,
        account: String,
    },
    Rate {
        account: String,
        windows: Windows,
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
    /// A reading from `claude -p /usage`, which costs no model call.
    Reading {
        account: String,
        windows: Windows,
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
    fn wrap(self) -> Event;
}

macro_rules! owned {
    ($ty:ty, $system:literal, $variant:ident) => {
        impl Owned for $ty {
            const SYSTEM: &'static str = $system;
            fn wrap(self) -> Event {
                Event::$variant(self)
            }
        }
    };
}
owned!(QueueEvent, "queue", Queue);
owned!(DispatchEvent, "dispatch", Dispatch);
owned!(SessionEvent, "session", Session);
owned!(QuotaEvent, "quota", Quota);
owned!(LandEvent, "land", Land);

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
}

pub fn now_micros() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros()).unwrap_or(u64::MAX)
}

/// A system's output port. Only the owner of `E` can build one.
#[derive(Debug)]
pub struct Port<E> {
    file: PathBuf,
    _event: PhantomData<E>,
}

impl<E: Owned> Port<E> {
    pub fn new(bus: &Path) -> Self {
        Self { file: bus.join(format!("{}.ndjson", E::SYSTEM)), _event: PhantomData }
    }

    pub fn emit(&self, event: E) -> Result<()> {
        let mut line = serde_json::to_vec(&Line { t: now_micros(), event })?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .with_context(|| format!("opening {}", self.file.display()))?;
        // One write of a short line with O_APPEND: concurrent writers never interleave.
        let written = f.write(&line)?;
        anyhow::ensure!(written == line.len(), "short write to {}", self.file.display());
        Ok(())
    }
}

fn read_port<E: Owned>(bus: &Path, out: &mut Vec<(u64, usize, Event)>) -> Result<()> {
    let file = bus.join(format!("{}.ndjson", E::SYSTEM));
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    // A torn last line (a writer mid-append) or a foreign event type is skipped.
    out.extend(
        text.lines()
            .enumerate()
            .filter_map(|(i, l)| serde_json::from_str::<Line<E>>(l).ok().map(|line| (line.t, i, line.event.wrap()))),
    );
    Ok(())
}

/// Every event on the bus, oldest first.
pub fn read_all(bus: &Path) -> Result<Vec<Stamped>> {
    let mut all = Vec::new();
    read_port::<QueueEvent>(bus, &mut all)?;
    read_port::<DispatchEvent>(bus, &mut all)?;
    read_port::<SessionEvent>(bus, &mut all)?;
    read_port::<QuotaEvent>(bus, &mut all)?;
    read_port::<LandEvent>(bus, &mut all)?;
    all.sort_by_key(|(t, i, _)| (*t, *i));
    Ok(all.into_iter().map(|(t, _, event)| Stamped { t, event }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::Dir;

    #[test]
    fn round_trip_and_ownership() {
        let d = Dir::new();
        Port::<QueueEvent>::new(d.path()).emit(QueueEvent::Dequeued { job: "a".into() }).unwrap();
        Port::<LandEvent>::new(d.path()).emit(LandEvent::Forgotten { job: "a".into() }).unwrap();
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
    fn line_format_is_flat() {
        let l = Line { t: 5, event: SessionEvent::SignedOut { account: "w".into() } };
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"{"t":5,"type":"signed-out","account":"w"}"#);
    }
}
