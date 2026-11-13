//! The room a queued job is judged against, read once per dispatch pass or board: the machine's
//! accounts as every project knows them, what every pool holds, and what the machine's gauges read.
//!
//! Admission asks two kinds of constraint. A [`Pool`] is counted: sessions take whole units of it
//! and it has a capacity (an account's seats, a profile's running jobs, the project's
//! `max_running`, the machine's compiling jobs), and [`Room::fits`] is the one test of whether
//! another unit fits. A gate is measured: the load, the machine's available memory, free disk, the
//! development environment. A job that does not start is held by the first constraint that binds,
//! which its [`Hold`] names ([`Constraint`]) beside the reason a person reads.
//!
//! [`Room::admit`] is the one judgment of whether an account takes a session on a model now, and
//! [`Room::hold`] the one judgment of why a queued job does not start. The dispatcher, the accounts
//! pane, the board's reasons and a checker's seat all ask these, so none of them can call an
//! account `ready` or a job startable where another would not. A seat is read from what its
//! dispatch recorded (`Job::seat`), or a decision's session from what its booking recorded
//! (`SessionSeat::held`), never resolved again through a config.

use std::collections::BTreeMap;

use crate::accounts::{self, Draws, Refusal};
use crate::config::{Config, Floor, Limits, Machine};
use crate::ctx::Others;
use crate::harness::Home;
use crate::world::{AccountState, Job, SessionSeat, Status, World};

use super::{kept, may_use, model_for, profile_hold};
use crate::systems::devshell::Gate;

/// How long a job waits for a reading under way before it starts on an account read already: from
/// when the reading went stale or the job was queued, whichever is later.
pub const READING_WAIT_MICROS: u64 = 2 * 60 * 1_000_000;

/// How long until a job held by something no event announces is judged again (bus time): a gauge
/// coming back (memory, load, free disk), or a unit of the machine's compiling pool whose runner
/// died in another project, which nobody here reaps.
pub const LOOK_AGAIN_MICROS: u64 = 60 * 1_000_000;

/// How long until a job is judged again when a unit it waits for is held by a runner on its way
/// out (past its job's `ended`, or set aside): it exits within moments, with no event.
pub const LINGER_MICROS: u64 = 2 * 1_000_000;

/// Something counted that sessions take whole units of, up to a capacity. A machine-wide pool
/// (the compiling jobs, an account's seats) counts every project's live runs; a project's own
/// counts its jobs alone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pool {
    /// Every project's compiling jobs on this machine ([`Job::compiling`]): every job with a runner,
    /// read-only or not, but one queued as compiling nothing. Capacity:
    /// `[machine] max_compiling` in the owner's policy; none when it is not set.
    Compiling,
    /// This project's running jobs. Capacity: `limits.max_running`.
    Running,
    /// This project's jobs of one profile that a runner holds. Capacity: the profile's `max_running`.
    Profile(String),
    /// Every project's sessions on one seat budget: the account (or its first twin), and what they
    /// draw on there. Capacity: `limits.per_account`.
    Seat(String, Draws),
}

impl Pool {
    /// The setting that is its capacity, which is also what the bus calls it (`in-line`).
    pub fn key(&self) -> String {
        match self {
            Self::Compiling => "machine.max_compiling".to_owned(),
            Self::Running => "limits.max_running".to_owned(),
            Self::Profile(name) => format!("profiles.{name}.max_running"),
            Self::Seat(..) => "limits.per_account".to_owned(),
        }
    }
}

/// A pool with no unit left: how many are taken, of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full {
    pub taken: usize,
    pub capacity: usize,
}

/// What a caller measured of the machine for the gates. One left unmeasured holds nothing, the
/// disk aside: whether it has room is the caller's to say ([`Room::new`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Gauges {
    /// The 1-minute load average.
    pub load: Option<f64>,
    /// The machine's available memory, in GiB.
    pub mem_gb: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct Room {
    found: Vec<Home>,
    /// Homes signed in as one account, each mapped to the first: they share a seat budget.
    twins: BTreeMap<String, String>,
    /// Each account's state as every project knows it, settled against its files.
    accounts: BTreeMap<String, AccountState>,
    /// The units taken of each pool.
    load: BTreeMap<Pool, usize>,
    /// Since when (bus time) each job of another project has been in line for the machine's
    /// compiling pool: a younger job of ours waits behind it.
    line: Vec<u64>,
    /// When this project's sessions of each profile started (bus time).
    starts: BTreeMap<String, Vec<u64>>,
    /// Whether free disk allows a build job to start.
    disk_ok: bool,
    /// The load and the memory, as far as the caller measured them.
    gauges: Gauges,
    /// The owner's machine caps, as the policy said when the room was read ([`Config::machine`]).
    machine: Machine,
    /// The projects whose state could not be read: what they run is in no count of this room.
    unread: Vec<String>,
    /// Whether a compiling unit is held by a runner on its way out ([`World::lingering`]).
    lingering: bool,
    /// The project's development environment, when it names one: no job starts until it is warm.
    dev_shell: Option<Gate>,
}

/// Why an account takes no session: every cause, so a busy account at its floor says both.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    /// `busy (1 of 1 seat on openai-codex)`.
    pub busy: Option<String>,
    /// Its own state or quota.
    pub fit: Option<Refusal>,
}

impl Refused {
    pub fn describe(&self) -> String {
        let fit = self.fit.as_ref().map(Refusal::describe);
        self.busy.iter().cloned().chain(fit).collect::<Vec<_>>().join(", ")
    }
}

/// Why a queued job does not start now, when (bus time) that could change with no event, and the
/// constraint that binds: the first one that holds it, in the order [`Room::judge`] asks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub why: String,
    pub until: Option<u64>,
    pub by: Constraint,
}

/// What holds a queued job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    /// A pool with no unit left for it.
    Pool(Pool),
    /// Its profile's `max_per_day`.
    PerDay,
    /// The load gate: `limits.max_load`.
    Load,
    /// The memory gate: `[machine] min_free_mem_gb` in the owner's policy.
    Memory,
    /// A machine-wide count is incomplete: a project's state could not be read.
    Uncounted,
    /// The disk gate: `limits.min_free_disk_gb`, which is at least the machine's.
    Disk,
    /// The project's development environment is not warm.
    DevShell,
    /// No account it may use takes it: each one's seats or its quota.
    Accounts,
    /// An account that may have more room is being read.
    Reading,
}

/// What a dispatch gives a job: the account, and the model and provider it runs there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub job: String,
    pub account: String,
    pub model: Option<String>,
    pub provider: String,
}

impl Room {
    /// The room for `world` (this project's fold; an empty one outside any project) among
    /// `others`, the other projects' folds on this machine: what they know of the accounts is
    /// heard, and their live runners' seats count; their jobs stay theirs. `disk_ok` is whether
    /// free disk lets a build job start, as the caller measured it. The projects `others` could
    /// not read come with the folds it did read, so a count over every project knows it is short.
    /// The owner's machine caps are read from the policy here, each time ([`Config::machine`]), and
    /// the machine's available memory when they set a floor for it, so whoever starts a session of
    /// any kind is held by the caps as they are now.
    pub fn new(cfg: &Config, world: &World, others: &Others<'_>, disk_ok: bool) -> Self {
        let found = accounts::discover(cfg);
        let heard = heard(&found, world, &others.worlds);
        let machine = cfg.machine();
        let mem_gb = machine.min_free_mem_gb.and_then(|_| crate::os::mem_available_gb());
        Self::of(found, heard, world, &others.worlds, disk_ok)
            .with_machine(machine)
            .with_unread(others.unreadable.clone())
            .with_gauges(Gauges { load: None, mem_gb })
    }

    /// The room outside every project: every account on the machine (`accounts::discover_all`),
    /// as all the projects know it, and the seats their live runners hold. No project's rules
    /// apply, so nothing should be admitted in it; it is what the machine-wide pane reads.
    pub fn machine_wide(world: &World, others: &[&World]) -> Self {
        let found = accounts::discover_all();
        let heard = heard(&found, world, others);
        Self::of(found, heard, world, others, true)
    }

    /// As [`Room::new`], with the accounts already found and their states already heard and settled.
    pub fn of(
        found: Vec<Home>,
        accounts: BTreeMap<String, AccountState>,
        world: &World,
        others: &[&World],
        disk_ok: bool,
    ) -> Self {
        let twins = accounts::twins(&found);
        let starts = world.profile_starts.clone();
        let mut room = Self { found, twins, accounts, starts, disk_ok, ..Self::default() };
        // Ours as the fold says (a dispatch pass reaps dead runners first); theirs only while
        // their runner lives, since nobody here reaps them. So a unit a dead runner held, of a
        // seat or of the machine's compiling pool, is free again the next time anyone looks.
        let theirs = || others.iter().flat_map(|w| w.jobs().values()).filter(|j| j.runner_alive());
        for (account, draws) in world.jobs().values().chain(theirs()).filter_map(Job::seat) {
            room.book(account, draws);
        }
        // A decision's `agent:` session is a session like any: every project's, while its process lives.
        let sessions = std::iter::once(world).chain(others.iter().copied()).flat_map(|w| w.seats.values());
        for (account, draws) in sessions.filter_map(SessionSeat::held) {
            room.book(account, draws);
        }
        // A compiling unit is its runner process's, in every project alike and whatever the job's
        // status says: held while that process lives, by the evidence a seat's is.
        // The claims are each fold's own, apart from its jobs, so a job cleaned away takes none with it.
        for w in std::iter::once(world).chain(others.iter().copied()) {
            (0..w.compiling_units()).for_each(|_| room.add(Pool::Compiling));
            room.lingering |= w.lingering();
        }
        for j in world.jobs().values() {
            if j.status == Status::Running {
                room.add(Pool::Running);
            }
            if let Some(p) = j.profile.as_deref().filter(|_| j.status.has_runner()) {
                room.add(Pool::Profile(p.to_owned()));
            }
        }
        let compiling = Pool::Compiling.key();
        let in_line = others.iter().flat_map(|w| w.jobs().values()).filter(|j| j.in_line_for(&compiling));
        room.line = in_line.map(Job::waiting_since).collect();
        room
    }

    /// The room under the owner's caps `machine`.
    #[must_use]
    pub fn with_machine(mut self, machine: Machine) -> Self {
        self.machine = machine;
        self
    }

    /// The room where free disk does (`true`) or does not let a build job start.
    #[cfg(test)]
    #[must_use]
    pub fn with_disk(mut self, disk_ok: bool) -> Self {
        self.disk_ok = disk_ok;
        self
    }

    /// The room that could not read the state of the projects `unread`.
    #[must_use]
    pub fn with_unread(mut self, unread: Vec<String>) -> Self {
        self.unread = unread;
        self
    }

    /// The room with the machine as `gauges` read it.
    #[must_use]
    pub fn with_gauges(mut self, gauges: Gauges) -> Self {
        self.gauges = gauges;
        self
    }

    /// The room with the 1-minute load average as the caller read it, when `cfg` sets a limit on
    /// it: what the dispatcher and the board judge by, and nobody who books a decision's session.
    #[must_use]
    pub fn with_load(mut self, cfg: &Config) -> Self {
        self.gauges.load = cfg.limits.max_load.and_then(|_| crate::os::load_average());
        self
    }

    /// The room with the project's development environment as `gate` judges it ([`Gate::of`]).
    #[must_use]
    pub fn with_dev_shell(mut self, gate: Option<Gate>) -> Self {
        self.dev_shell = gate;
        self
    }

    /// Every account found, in discovery order.
    pub fn found(&self) -> &[Home] {
        &self.found
    }

    /// What every project knows of the account `key` (`harness:label`).
    pub fn account(&self, key: &str) -> Option<&AccountState> {
        self.accounts.get(key)
    }

    /// The first home signed in as the same account as `key`, when `key` is not the first.
    pub fn twin_of(&self, key: &str) -> Option<&String> {
        self.twins.get(key)
    }

    /// The sessions every project runs on the account `key` (and its twins), on any provider.
    pub fn running_on(&self, key: &str) -> usize {
        let seat = accounts::seat(&self.twins, key);
        self.load.iter().filter(|(p, _)| matches!(p, Pool::Seat(a, _) if a == seat)).map(|(_, n)| n).sum()
    }

    /// The seat pool of `account` (its first twin's) for a session drawing on `draws` there.
    fn seat(&self, account: &str, draws: Draws) -> Pool {
        Pool::Seat(accounts::seat(&self.twins, account).to_owned(), draws)
    }

    /// Takes one unit of `pool`.
    fn add(&mut self, pool: Pool) {
        *self.load.entry(pool).or_default() += 1;
    }

    fn book(&mut self, account: &str, draws: Draws) {
        self.add(self.seat(account, draws));
    }

    /// The units of `pool` a new one would be counted with. On a seat, the sessions a new one
    /// drawing on the same thing would share it with: those on the same budget, and on a
    /// per-provider harness every session whose provider is unknown, since it may be on this one;
    /// a new session whose provider is unknown shares with them all.
    pub fn taken(&self, pool: &Pool) -> usize {
        let on = |p: &Pool| self.load.get(p).copied().unwrap_or(0);
        match pool {
            Pool::Seat(account, Draws::Provider(_)) => on(pool) + on(&Pool::Seat(account.clone(), Draws::Unknown)),
            Pool::Seat(account, Draws::Unknown) => self.running_on(account),
            _ => on(pool),
        }
    }

    /// Whether one more unit of `pool` fits under `capacity` (`None`: the pool has no cap): the one
    /// test every pool is asked. `Ok` is the units taken, so the new one's share can be reckoned.
    pub fn fits(&self, pool: &Pool, capacity: Option<usize>) -> Result<usize, Full> {
        let taken = self.taken(pool);
        match capacity {
            Some(capacity) if taken >= capacity => Err(Full { taken, capacity }),
            _ => Ok(taken),
        }
    }

    /// Whether `home` takes a session on `model` now (`now_us`, bus time): a seat free under
    /// `limits.per_account` (per provider on omp), and its own state and quota fit
    /// ([`accounts::fit`]). `Ok` is how good a fit it is: its room per day shared among the
    /// sessions on its seat and the new one, so a pass that books a start here ([`Room::take`])
    /// judges the next job with it, and a burst spreads over accounts with like room.
    pub fn admit(&self, limits: &Limits, home: &Home, model: Option<&str>, now_us: u64) -> Result<f64, Refused> {
        let key = home.key();
        let draws = accounts::draws(model, "", home.harness);
        let seats = self.fits(&self.seat(&key, draws.clone()), Some(limits.per_account));
        let taken = seats.unwrap_or_else(|full| full.taken);
        let busy = seats.err().map(|Full { taken, capacity }| {
            let on = if let Draws::Provider(p) = &draws { format!(" on {p}") } else { String::new() };
            let s = if capacity == 1 { "" } else { "s" };
            format!("busy ({taken} of {capacity} seat{s}{on})")
        });
        match (accounts::fit(home, self.accounts.get(&key), model, limits, now_us), busy) {
            (Ok(score), None) => Ok(share(score, taken)),
            (fit, busy) => Err(Refused { busy, fit: fit.err() }),
        }
    }

    /// Whether `j` should wait for a reading of `home`, which `refused` it on `model`, rather
    /// than start elsewhere now: its only cause is a reading that is stale (or never taken), one
    /// is due (no failed read backing off, so the dispatcher has asked for it), and the wait is
    /// young ([`READING_WAIT_MICROS`]). Returns when the wait ends, and the fit its last reading
    /// gave it, shared as [`Room::admit`] shares it (`None` never read: its room is unknown).
    /// `None` when there is nothing worth waiting for: past its floor when last read, say.
    fn awaited(
        &self,
        cfg: &Config,
        j: &Job,
        home: &Home,
        model: Option<&str>,
        refused: &Refused,
        now_us: u64,
    ) -> Option<(u64, Option<f64>)> {
        if refused.busy.is_some() || !matches!(refused.fit, Some(Refusal::Stale(_) | Refusal::Unread)) {
            return None;
        }
        let key = home.key();
        let st = self.accounts.get(&key);
        if !accounts::due(accounts::harness_of(home), st, now_us) {
            return None;
        }
        let since = accounts::stale_since(st).map_or(j.queued_at, |s| s.max(j.queued_at));
        let until = since.saturating_add(READING_WAIT_MICROS);
        if now_us >= until {
            return None;
        }
        if st.and_then(|s| s.read_at).is_none() {
            return Some((until, None));
        }
        let last = accounts::fit_as_read(home, st, model, &cfg.limits, now_us).ok()?;
        let taken = self.taken(&self.seat(&key, accounts::draws(model, "", home.harness)));
        Some((until, Some(share(last, taken))))
    }

    /// Why `j`, queued and waiting for no other job, does not start at `now_us` by `cfg`'s rules:
    /// the constraint that binds first ([`Room::judge`]). `None` when it would start. Pure: it reads
    /// only the room.
    pub fn hold(&self, cfg: &Config, j: &Job, now_us: u64) -> Option<Hold> {
        self.judge(cfg, j, now_us).err()
    }

    /// As [`Room::hold`], with every account that admits `j` and its fit when nothing holds it. The
    /// constraints are asked in this order, and the first that binds is the hold:
    ///
    /// 1. its profile's limits, unless a decision forced it past them: the profile's running pool,
    ///    then its starts per day;
    /// 2. the project's running pool (`limits.max_running`);
    /// 3. the gates: the load, the machine's available memory, free disk for a build job, and the
    ///    project's development environment (read-only jobs need that too);
    /// 4. an account it may use that [`Room::admit`]s it on its model there;
    /// 5. for a job that compiles ([`Job::compiles`]: any but one queued as compiling nothing), the
    ///    machine's compiling pool, over every project. It is asked
    ///    last, so a job held by it would start but for it: that is what puts a job in line.
    pub fn judge<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64) -> Result<Vec<(&'a Home, f64)>, Hold> {
        self.judged(cfg, j, now_us, true)
    }

    /// Where `j` would start, the constraints that say only when aside: the project's
    /// `max_running`, the machine's compiling pool, the load and the memory.
    pub fn judge_where<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64) -> Result<Vec<(&'a Home, f64)>, Hold> {
        self.judged(cfg, j, now_us, false)
    }

    /// The seats a decision's `agent:` session `j` may take: a session like any, so the memory
    /// gate holds it, and it needs an account that admits it. It is no job of the queue, so no
    /// pool of jobs counts it (the machine's compiling pool neither: it judges a run from its
    /// record), and it is read-only, so the disk does not hold it.
    pub fn judge_session<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64) -> Result<Vec<(&'a Home, f64)>, Hold> {
        self.memory(now_us)?;
        self.accounts_for(cfg, j, now_us)
    }

    fn judged<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64, when: bool) -> Result<Vec<(&'a Home, f64)>, Hold> {
        let profile = j.profile.as_deref().and_then(|n| cfg.profiles.get(n).map(|p| (n, p)));
        if let Some((name, p)) = profile.filter(|_| !j.forced) {
            let pool = Pool::Profile(name.to_owned());
            let running = self.taken(&pool);
            let starts = self.starts.get(name).map_or(&[][..], Vec::as_slice);
            if let Some((why, until)) = profile_hold(name, p, running, starts, now_us) {
                let by = match self.fits(&pool, p.max_running) {
                    Err(_) => Constraint::Pool(pool),
                    Ok(_) => Constraint::PerDay,
                };
                return Err(Hold { why, until, by });
            }
        }
        if when {
            if let Err(Full { taken, capacity }) = self.fits(&Pool::Running, Some(cfg.limits.max_running)) {
                let why = format!("{taken} of {capacity} running in this project (limits.max_running)");
                return Err(Hold { why, until: None, by: Constraint::Pool(Pool::Running) });
            }
            self.load(cfg, now_us)?;
            self.memory(now_us)?;
        }
        if !j.readonly() && !self.disk_ok {
            // The floor as the room read it: the project's own, or the machine's where that is higher.
            let floor = Floor::of(&cfg.limits, &self.machine);
            let (floor, need) = (floor.gb, floor.resume_gb());
            let whose = if Floor::of(&cfg.limits, &self.machine).machines { "the machine's " } else { "the " };
            return Err(Hold {
                why: format!("free disk below {need} GB ({whose}{floor} GB floor and its margin)"),
                until: Some(now_us.saturating_add(LOOK_AGAIN_MICROS)),
                by: Constraint::Disk,
            });
        }
        if let Some(hold) = self.dev_shell.as_ref().and_then(|g| g.hold(now_us)) {
            return Err(hold);
        }
        let fits = self.accounts_for(cfg, j, now_us)?;
        if when && j.compiles() {
            self.compiling(j, now_us)?;
        }
        Ok(fits)
    }

    /// The load gate: nothing starts while the 1-minute load average, where the caller read it, is
    /// above `limits.max_load`.
    fn load(&self, cfg: &Config, now_us: u64) -> Result<(), Hold> {
        match (cfg.limits.max_load, self.gauges.load) {
            (Some(max), Some(load)) if load > max => Err(Hold {
                why: format!("load {load:.1} is above {max} (limits.max_load)"),
                until: Some(now_us.saturating_add(LOOK_AGAIN_MICROS)),
                by: Constraint::Load,
            }),
            _ => Ok(()),
        }
    }

    /// The memory gate: no session starts while the machine's available memory is under the
    /// owner's floor. Memory that could not be read holds nothing: a machine that does not say is
    /// as it was before the gate.
    fn memory(&self, now_us: u64) -> Result<(), Hold> {
        match memory_hold(&self.machine, self.gauges.mem_gb) {
            Some(why) => {
                Err(Hold { why, until: Some(now_us.saturating_add(LOOK_AGAIN_MICROS)), by: Constraint::Memory })
            }
            None => Ok(()),
        }
    }

    /// The machine's compiling pool, for a job that compiles and that nothing else holds: a unit
    /// must be free for it after one is left for each job of another project that has waited
    /// longer in the same line. Only strictly older jobs are waited for, so two of one age never
    /// wait for each other. A unit whose runner died comes back with no event, so a job held here
    /// is judged again soon.
    ///
    /// The count is over every project, so it is only a count when every project was read: with
    /// one unreadable, its compiling jobs are in nobody's sum and the cap would be passed without
    /// anyone seeing it. Then nothing that compiles starts, and the hold names the project.
    fn compiling(&self, j: &Job, now_us: u64) -> Result<(), Hold> {
        let Some(capacity) = self.machine.max_compiling else { return Ok(()) };
        if !self.unread.is_empty() {
            let s = if self.unread.len() == 1 { "" } else { "s" };
            return Err(Hold {
                why: format!(
                    "the compiling jobs on this machine cannot be counted: the state of project{s} {} cannot be read \
                     (machine.max_compiling in the owner's policy)",
                    self.unread.join(", ")
                ),
                until: Some(now_us.saturating_add(LOOK_AGAIN_MICROS)),
                by: Constraint::Uncounted,
            });
        }
        let taken = self.taken(&Pool::Compiling);
        let ahead = self.line.iter().filter(|&&since| since < j.waiting_since()).count();
        if taken + ahead < capacity {
            return Ok(());
        }
        let mut why = format!("{taken} of {capacity} compiling on this machine, over every project");
        if taken < capacity {
            let s = if ahead == 1 { " of another project has" } else { "s of other projects have" };
            why = format!("{why}, and {ahead} job{s} waited longer for the rest");
        }
        // A runner on its way out gives its unit back within moments, and says nothing.
        let again = if self.lingering { LINGER_MICROS } else { LOOK_AGAIN_MICROS };
        Err(Hold {
            why: format!("{why} (machine.max_compiling in the owner's policy)"),
            until: Some(now_us.saturating_add(again)),
            by: Constraint::Pool(Pool::Compiling),
        })
    }

    /// Every account `j` may use that admits it on its model there, each with its fit; else why
    /// none does, or which reading it waits for.
    fn accounts_for<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64) -> Result<Vec<(&'a Home, f64)>, Hold> {
        // Every account of every harness the job allows, judged the same way: on the meters its
        // harness says govern the job's model there.
        let mut why = Vec::new();
        let mut fits = Vec::new();
        let mut awaited = Vec::new();
        for a in self.found.iter().filter(|a| may_use(j, a)) {
            let model = model_for(cfg, j, a.harness);
            match self.admit(&cfg.limits, a, model.as_deref(), now_us) {
                Ok(score) => fits.push((a, score)),
                Err(r) => {
                    awaited.extend(self.awaited(cfg, j, a, model.as_deref(), &r, now_us).map(|w| (a.key(), w)));
                    why.push(format!("{} {}", a.key(), r.describe()));
                }
            }
        }
        if fits.is_empty() {
            let why = if why.is_empty() { "the job's harness or pin allows no account".into() } else { why.join("; ") };
            return Err(Hold { why: format!("no account has room ({why})"), until: None, by: Constraint::Accounts });
        }
        // An account read a moment ago should not take the job only because the others' readings are
        // still under way: wait for those that may have more room, a little while, unless the job
        // resumes a transcript on an account that fits now. Waiting admits nothing unread, so no floor moves.
        let best = fits.iter().map(|&(_, s)| s).fold(f64::NEG_INFINITY, f64::max);
        let resumes = kept(j).is_some_and(|k| fits.iter().any(|(a, _)| a.key() == k));
        awaited.retain(|(_, (_, last))| last.is_none_or(|s| s > best));
        if let Some(until) = awaited.iter().map(|(_, (u, _))| *u).min().filter(|_| !resumes) {
            let names: Vec<&str> = awaited.iter().map(|(k, _)| k.as_str()).collect();
            let fresh: Vec<String> = fits.iter().map(|(a, _)| a.key()).collect();
            let why = format!(
                "waiting for a reading of {} before starting on {}{}",
                names.join(", "),
                fresh.join(", "),
                at(until).map_or_else(String::new, |t| format!(" (until {t} at most)"))
            );
            return Err(Hold { why, until: Some(until), by: Constraint::Reading });
        }
        Ok(fits)
    }

    /// Books the queued job `j`'s start on `home` at `now_us`, so the next job of the pass is
    /// judged with it running: a unit of every pool it takes (its seat, the project's running jobs,
    /// its profile's, and the machine's compiling jobs when it compiles), and its profile's start.
    /// Returns what its dispatch records.
    pub fn take(&mut self, cfg: &Config, j: &Job, home: &Home, now_us: u64) -> Start {
        self.add(Pool::Running);
        if j.compiles() {
            self.add(Pool::Compiling);
        }
        if let Some(name) = j.profile.as_deref().filter(|n| cfg.profiles.contains_key(*n)) {
            self.add(Pool::Profile(name.to_owned()));
            self.starts.entry(name.to_owned()).or_default().push(now_us);
        }
        self.take_session(cfg, j, home)
    }

    /// Books a seat on `home` for the session `j`, a decision's `agent:` session or a job's: the
    /// one pool a session of any kind takes a unit of. Returns what its booking records.
    pub fn take_session(&mut self, cfg: &Config, j: &Job, home: &Home) -> Start {
        let model = model_for(cfg, j, home.harness);
        let provider = accounts::provider_of(model.as_deref(), home.harness).to_owned();
        let account = home.key();
        self.book(&account, accounts::draws(model.as_deref(), &provider, home.harness));
        Start { job: j.name.clone(), account, model, provider }
    }
}

/// The memory gate's one judgment, for a session and for an acceptance check alike: why nothing
/// starts with `free_gb` GiB of memory available (`MemAvailable`), by the floor in the owner's
/// policy. `None` when it is at or over the floor, when no floor is set, and when the memory could
/// not be read.
pub fn memory_hold(machine: &Machine, free_gb: Option<f64>) -> Option<String> {
    let (floor, free) = (machine.min_free_mem_gb?.get(), free_gb?);
    (free < floor).then(|| {
        format!(
            "{free:.1} GiB of memory available, under the machine's {floor} GiB floor \
             (machine.min_free_mem_gb in the owner's policy)"
        )
    })
}

/// `score` shared among the `taken` sessions on its seat and one more.
#[allow(clippy::cast_precision_loss, reason = "a handful of sessions")]
fn share(score: f64, taken: usize) -> f64 {
    score / (taken + 1) as f64
}

/// Bus time `us` as a local clock reading: `14:02 +06`.
fn at(us: u64) -> Option<String> {
    let t = jiff::Timestamp::from_microsecond(i64::try_from(us).ok()?).ok()?;
    Some(crate::os::local(t).strftime("%H:%M %Z").to_string())
}

/// What `world` and `others` know of the accounts `found`, merged and settled against their files.
fn heard(found: &[Home], world: &World, others: &[&World]) -> BTreeMap<String, AccountState> {
    let mut heard = World::default();
    heard.accounts.clone_from(&world.accounts);
    heard.hear_accounts(others.iter().copied());
    accounts::settle(&mut heard, found);
    heard.accounts
}
