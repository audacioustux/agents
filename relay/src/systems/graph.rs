//! graph: the one place a change to the dependency graph is recorded. A job added, resumed or requeued, a
//! review replacing a chain, a run's end and a builder resumed by its review's reject are each read, checked
//! and recorded under the jobs lock, on the world as it is then ([`Change`]), so no other process can change
//! the graph between the check and the record. [`crate::bus::Port::emit`] refuses such an event: only a
//! [`Change`] records one ([`crate::bus::Port::record`]). The lock is released when the change is dropped,
//! which must come before anything this process does that takes the lock itself (a re-review after a run's
//! end).

use anyhow::Result;

use crate::bus::{Owned, Port, Stamped};
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
    pub fn record<E: Owned + Clone>(&mut self, port: &Port<E>, events: Vec<E>) -> Result<()> {
        for event in events {
            port.record(event.clone(), &Recorded(()))?;
            self.world.apply(&Stamped { t: crate::bus::now_micros(), event: event.wrap(), from: None });
        }
        Ok(())
    }
}
