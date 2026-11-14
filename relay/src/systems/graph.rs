//! graph: the one place a change to the dependency graph is recorded. A job added, resumed or requeued, a
//! review replacing a chain, a run's end and a builder resumed by its review's reject are each read, checked
//! and recorded under the jobs lock, on the world as it is then ([`Change`]), so no other process can change
//! the graph between the check and the record. [`crate::bus::Port::emit`] refuses such an event: only a
//! [`Change`] records one ([`crate::bus::Port::record`]). The lock is released when the change is dropped,
//! which must come before anything this process does that takes the lock itself (a re-review after a run's
//! end).
//!
//! A change is published in order, and a reader that takes no lock (`view result --wait`, the board) may
//! read it half way: so every job an event names is recorded before it ([`unpublished`]). A new job comes
//! before anything that names it (a replacement review before the `unchained` that points at it).

use anyhow::Result;

use crate::bus::{Event, Owned, Port, QueueEvent, SessionEvent, Stamped};
use crate::ctx::Ctx;
use crate::os::Role;
use crate::world::World;

/// Proof, for [`Port::record`], that an event is recorded by a [`Change`]: only this module makes one.
pub struct Recorded(());

/// The jobs lock, held, and the world read under it: what every check of a change reads.
pub struct Change<'a> {
    ctx: &'a Ctx,
    world: World,
    _lock: Role,
}

impl<'a> Change<'a> {
    /// Takes the jobs lock (waiting for another holder) and reads the world under it.
    pub fn take(ctx: &'a Ctx) -> Result<Self> {
        let lock = ctx.jobs_lock()?;
        let world = ctx.world()?;
        Ok(Self { ctx, world, _lock: lock })
    }

    pub fn ctx(&self) -> &'a Ctx {
        self.ctx
    }

    /// The world as it is under the lock, with what this change recorded.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The cycle `events` would close through `node`, the job they add or revive, on the world as it is now.
    pub fn cycle<E: Owned + Clone>(&self, events: &[E], node: &str) -> Option<String> {
        self.world.closes_cycle(events.iter().cloned().map(Owned::wrap), node)
    }

    /// Records `events` through `port` unless they would close a cycle through `node`: then nothing is
    /// recorded, and the cycle is the answer (`Ok(Err(cycle))`).
    pub fn try_record<E: Owned + Clone>(
        &mut self,
        port: &Port<E>,
        events: Vec<E>,
        node: &str,
    ) -> Result<Result<(), String>> {
        if let Some(cycle) = self.cycle(&events, node) {
            return Ok(Err(cycle));
        }
        self.record(port, events)?;
        Ok(Ok(()))
    }

    /// Records `events` through `port` as they are: a change already checked, or one that cannot be refused
    /// (a run has ended). The world this change reads takes them in, so a later check in it sees them.
    ///
    /// Refuses, recording nothing more, an event that names a job not recorded yet ([`unpublished`]): what a
    /// lockless reader sees half way through is then always a state the jobs could be in.
    pub fn record<E: Owned + Clone>(&mut self, port: &Port<E>, events: Vec<E>) -> Result<()> {
        for event in events {
            if let Some(name) = unpublished(&self.world, &event.clone().wrap()) {
                anyhow::bail!("a change names {name} before {name} is recorded; record it first");
            }
            port.record(event.clone(), &Recorded(()))?;
            self.world.apply(&Stamped { t: crate::bus::now_micros(), event: event.wrap(), from: None });
        }
        Ok(())
    }
}

/// The first job `event` names that `world` has not recorded and the event does not itself create: `None`
/// when it can be published now. A queued job names its `--after` jobs (the acceptance gate is no job), the
/// job it forks, its parent and the build it reviews; an `unchained` names the review that replaced it; a
/// halt names its review; a verdict the build it judged; every other event its own job. A release names its
/// job but not the prerequisites it drops: taking a name away leaves nothing pointing at what is not there.
pub fn unpublished(world: &World, event: &Event) -> Option<String> {
    let mut named: Vec<&str> = Vec::new();
    match event {
        Event::Queue(QueueEvent::Queued { after, fork, parent, acceptance, .. }) => {
            named.extend(after.iter().map(String::as_str).filter(|a| *a != crate::bus::ACCEPTANCE_GATE));
            named.extend(fork.as_deref().map(|f| f.job.as_str()));
            named.extend(parent.as_deref());
            named.extend(acceptance.review_of());
        }
        Event::Queue(QueueEvent::Unchained { job, by }) => {
            named.extend([Some(job.as_str()), by.as_deref()].into_iter().flatten());
        }
        Event::Queue(QueueEvent::ReviewHalted { job, review, .. }) => named.extend([job.as_str(), review.as_str()]),
        // A release only takes names away: the prerequisites it drops may be gone (cleaned away) already.
        Event::Queue(
            QueueEvent::Released { job, .. }
            | QueueEvent::Resumed { job, .. }
            | QueueEvent::Rerouted { job, .. }
            | QueueEvent::Forced { job }
            | QueueEvent::Dequeued { job }
            | QueueEvent::ReviewBriefed { job, .. }
            | QueueEvent::StopRequested { job },
        ) => named.push(job),
        Event::Session(SessionEvent::Reviewed { job, of, .. }) => named.extend([job.as_str(), of.as_str()]),
        Event::Session(e) => named.extend(e.job()),
        _ => {}
    }
    named.into_iter().find(|n| !world.jobs().contains_key(*n)).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event from its JSON line, `{n}` replaced by the job it names.
    fn ev(json: &str, n: &str) -> Event {
        let line = json.replace("{n}", n);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        match v["system"].as_str() {
            Some("session") => Event::Session(serde_json::from_value(v).unwrap()),
            _ => Event::Queue(serde_json::from_value(v).unwrap()),
        }
    }

    const QUEUED: &str = r#""type":"queued","task":"t","readonly":false,"effort":null,"budget_usd":1.0,"pinned":null"#;

    /// Copilot on PR #69 (queue.rs:205): an event that names a job is published only once that job is. For
    /// every kind of event a change records, naming a job that is not there is refused, and the same event
    /// once the job is there is not.
    #[test]
    fn every_event_of_a_change_names_only_jobs_already_recorded() {
        let mut world = World::default();
        for name in ["build", "rev", "next"] {
            let event = ev(&format!(r#"{{{QUEUED},"job":"{{n}}"}}"#), name);
            world.apply(&Stamped { t: 1, event, from: None });
        }
        let kinds = [
            format!(r#"{{{QUEUED},"job":"new","after":["{{n}}"]}}"#),
            format!(r#"{{{QUEUED},"job":"new","acceptance":{{"review-of":"{{n}}"}}}}"#),
            format!(r#"{{{QUEUED},"job":"new","fork":{{"job":"{{n}}","session_id":"s","account":"c:a"}}}}"#),
            format!(r#"{{{QUEUED},"job":"new","parent":"{{n}}"}}"#),
            r#"{"type":"unchained","job":"rev","by":"{n}"}"#.to_owned(),
            r#"{"type":"resumed","job":"{n}","message":"m","budget_usd":null}"#.to_owned(),
            r#"{"type":"released","job":"{n}","from":["build"]}"#.to_owned(),
            r#"{"type":"review-halted","job":"build","review":"{n}","why":"w"}"#.to_owned(),
            r#"{"type":"review-briefed","job":"{n}","brief":"b"}"#.to_owned(),
            r#"{"type":"dequeued","job":"{n}"}"#.to_owned(),
            r#"{"system":"session","type":"reviewed","job":"rev","of":"{n}","commit":"c","verdict":"accept"}"#.to_owned(),
            r#"{"system":"session","type":"ended","job":"{n}","outcome":"done","cost_usd":0.0,"turns":1,"reason":null}"#
                .to_owned(),
        ];
        for kind in &kinds {
            assert_eq!(unpublished(&world, &ev(kind, "ghost")).as_deref(), Some("ghost"), "{kind}: a job not there");
            assert_eq!(unpublished(&world, &ev(kind, "build")), None, "{kind}: one that is");
        }
        // Copilot on PR #69 (graph.rs:103): a release may drop a prerequisite that was cleaned away.
        let gone = ev(r#"{"type":"released","job":"next","from":["{n}"]}"#, "ghost");
        assert_eq!(unpublished(&world, &gone), None, "a release from a job that is gone takes a name away");
        let new = ev(&format!(r#"{{{QUEUED},"job":"{{n}}"}}"#), "ghost");
        assert_eq!(unpublished(&world, &new), None, "a job queued is created by its own event");
    }
}
