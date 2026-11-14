//! The room a queued job is judged against, read once per dispatch pass or board: the machine's
//! accounts as every project knows them, the seats every project's runs hold, this project's
//! profile counts, and whether the disk has room.
//!
//! [`Room::admit`] is the one judgment of whether an account takes a session on a model now, and
//! [`Room::hold`] the one judgment of why a queued job does not start. The dispatcher, the accounts
//! pane, the board's reasons and a checker's seat all ask these, so none of them can call an
//! account `ready` or a job startable where another would not. A seat is read from what its
//! dispatch recorded (`Job::seat`), or a decision's session from what its booking recorded
//! (`SessionSeat::held`), never resolved again through a config.

use std::collections::BTreeMap;

use crate::accounts::{self, Draws, Refusal};
use crate::config::{Config, Limits};
use crate::harness::Home;
use crate::world::{AccountState, Job, SessionSeat, World};

use super::{kept, may_use, model_for, profile_hold};
use crate::systems::devshell::Gate;

/// How long a job waits for a reading under way before it starts on an account read already: from
/// when the reading went stale or the job was queued, whichever is later.
pub const READING_WAIT_MICROS: u64 = 2 * 60 * 1_000_000;

#[derive(Debug, Clone, Default)]
pub struct Room {
    found: Vec<Home>,
    /// Homes signed in as one account, each mapped to the first: they share a seat budget.
    twins: BTreeMap<String, String>,
    /// Each account's state as every project knows it, settled against its files.
    accounts: BTreeMap<String, AccountState>,
    /// Sessions per seat: (the account, or its first twin; what they draw on there).
    load: BTreeMap<(String, Draws), usize>,
    /// This project's jobs with a runner, per profile.
    running: BTreeMap<String, usize>,
    /// When this project's sessions of each profile started (bus time).
    starts: BTreeMap<String, Vec<u64>>,
    /// Whether free disk allows a build job to start.
    disk_ok: bool,
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

/// What kind of thing holds a queued job, for counting the held by what holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Its profile's `max_running` or `max_per_day`.
    Profile,
    /// Free disk under the floor.
    Disk,
    /// The development environment is not warm.
    Setup,
    /// No account it may use has room, or a reading of one is awaited.
    Accounts,
}

/// Why a queued job does not start now, and when (bus time) that could change with no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub cause: Cause,
    pub why: String,
    pub until: Option<u64>,
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
    /// free disk lets a build job start, as the caller measured it.
    pub fn new(cfg: &Config, world: &World, others: &[&World], disk_ok: bool) -> Self {
        let found = accounts::discover(cfg);
        let heard = heard(&found, world, others);
        Self::of(found, heard, world, others, disk_ok)
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
        // their runner lives, since nobody here reaps them.
        let theirs = others.iter().flat_map(|w| w.jobs().values()).filter(|j| j.runner_alive());
        for (account, draws) in world.jobs().values().chain(theirs).filter_map(Job::seat) {
            room.book(account, draws);
        }
        // A decision's `agent:` session is a session like any: every project's, while its process lives.
        let sessions = std::iter::once(world).chain(others.iter().copied()).flat_map(|w| w.seats.values());
        for (account, draws) in sessions.filter_map(SessionSeat::held) {
            room.book(account, draws);
        }
        for p in world.jobs().values().filter(|j| j.status.has_runner()).filter_map(|j| j.profile.as_deref()) {
            *room.running.entry(p.to_owned()).or_default() += 1;
        }
        room
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

    /// How many of this project's jobs of profile `name` have a runner.
    pub fn profile_running(&self, name: &str) -> usize {
        self.running.get(name).copied().unwrap_or(0)
    }

    /// The sessions every project runs on the account `key` (and its twins), on any provider.
    pub fn running_on(&self, key: &str) -> usize {
        let seat = accounts::seat(&self.twins, key);
        self.load.iter().filter(|((a, _), _)| a == seat).map(|(_, n)| n).sum()
    }

    fn seat(&self, account: &str, draws: Draws) -> (String, Draws) {
        (accounts::seat(&self.twins, account).to_owned(), draws)
    }

    fn book(&mut self, account: &str, draws: Draws) {
        *self.load.entry(self.seat(account, draws)).or_default() += 1;
    }

    /// The sessions a new one drawing on `draws` at `account` would share its seats with: those on
    /// the same budget, and on a per-provider harness every session whose provider is unknown,
    /// since it may be on this one; a new session whose provider is unknown shares with them all.
    fn taken(&self, account: &str, draws: &Draws) -> usize {
        let on = |d: Draws| self.load.get(&self.seat(account, d)).copied().unwrap_or(0);
        match draws {
            Draws::Account => on(Draws::Account),
            Draws::Provider(_) => on(draws.clone()) + on(Draws::Unknown),
            Draws::Unknown => self.running_on(account),
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
        let taken = self.taken(&key, &draws);
        let busy = (taken >= limits.per_account).then(|| {
            let on = if let Draws::Provider(p) = &draws { format!(" on {p}") } else { String::new() };
            let s = if limits.per_account == 1 { "" } else { "s" };
            format!("busy ({taken} of {} seat{s}{on})", limits.per_account)
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
        let taken = self.taken(&key, &accounts::draws(model, "", home.harness));
        Some((until, Some(share(last, taken))))
    }

    /// Why `j`, queued and waiting for no other job, does not start at `now_us` by `cfg`'s rules:
    /// its profile's limits (unless a decision forced it past them), free disk for a build job,
    /// the project's development environment not yet warm (read-only jobs need it too), or no account it may use that [`Room::admit`]s it on its model there. `None` when it would
    /// start, the project's own `max_running` and load aside. Pure: it reads only the room.
    pub fn hold(&self, cfg: &Config, j: &Job, now_us: u64) -> Option<Hold> {
        self.judge(cfg, j, now_us).err()
    }

    /// As [`Room::hold`], with every account that admits `j` and its fit when nothing holds it.
    pub fn judge<'a>(&'a self, cfg: &Config, j: &Job, now_us: u64) -> Result<Vec<(&'a Home, f64)>, Hold> {
        let profile = j.profile.as_deref().and_then(|n| cfg.profiles.get(n).map(|p| (n, p)));
        if let Some((name, p)) = profile.filter(|_| !j.forced) {
            let running = self.running.get(name).copied().unwrap_or(0);
            let starts = self.starts.get(name).map_or(&[][..], Vec::as_slice);
            if let Some((why, until)) = profile_hold(name, p, running, starts, now_us) {
                return Err(Hold { cause: Cause::Profile, why, until });
            }
        }
        if !j.readonly() && !self.disk_ok {
            let (floor, need) = (cfg.limits.min_free_disk_gb, crate::systems::dispatch::resume_gb(&cfg.limits));
            return Err(Hold {
                cause: Cause::Disk,
                why: format!("free disk below {need} GB (the {floor} GB floor and its margin)"),
                until: None,
            });
        }
        if let Some(hold) = self.dev_shell.as_ref().and_then(|g| g.hold(now_us)) {
            return Err(hold);
        }
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
            return Err(Hold { cause: Cause::Accounts, why: format!("no account has room ({why})"), until: None });
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
            return Err(Hold { cause: Cause::Accounts, why, until: Some(until) });
        }
        Ok(fits)
    }

    /// Books `j`'s start on `home` at `now_us`, so the next job of the pass is judged with it
    /// running: its seat, and its profile's counts. Returns what its dispatch records.
    pub fn take(&mut self, cfg: &Config, j: &Job, home: &Home, now_us: u64) -> Start {
        let model = model_for(cfg, j, home.harness);
        let provider = accounts::provider_of(model.as_deref(), home.harness).to_owned();
        let account = home.key();
        self.book(&account, accounts::draws(model.as_deref(), &provider, home.harness));
        if let Some(name) = j.profile.as_deref().filter(|n| cfg.profiles.contains_key(*n)) {
            *self.running.entry(name.to_owned()).or_default() += 1;
            self.starts.entry(name.to_owned()).or_default().push(now_us);
        }
        Start { job: j.name.clone(), account, model, provider }
    }
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
