//! The room a queued job is judged against, read once per dispatch pass or board: the machine's
//! accounts as every project knows them, the seats every project's runs hold, this project's
//! profile counts, and whether the disk has room.
//!
//! [`Room::admit`] is the one judgment of whether an account takes a session on a model now, and
//! [`Room::hold`] the one judgment of why a queued job does not start. The dispatcher, the accounts
//! pane, the board's reasons and a checker's seat all ask these, so none of them can call an
//! account `ready` or a job startable where another would not. A seat is read from what its
//! dispatch recorded (`Job::seat`), never resolved again through a config.

use std::collections::BTreeMap;

use crate::accounts::{self, Refusal};
use crate::config::{Config, Limits};
use crate::harness::Home;
use crate::world::{AccountState, Job, World};

use super::{may_use, model_for, profile_hold};

#[derive(Debug, Clone, Default)]
pub struct Room {
    found: Vec<Home>,
    /// Homes signed in as one account, each mapped to the first: they share a seat budget.
    twins: BTreeMap<String, String>,
    /// Each account's state as every project knows it, settled against its files.
    accounts: BTreeMap<String, AccountState>,
    /// Sessions per seat: (the account, or its first twin; the provider, empty for one budget).
    load: BTreeMap<(String, String), usize>,
    /// This project's jobs with a runner, per profile.
    running: BTreeMap<String, usize>,
    /// When this project's sessions of each profile started (bus time).
    starts: BTreeMap<String, Vec<u64>>,
    /// Whether free disk allows a build job to start.
    disk_ok: bool,
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

/// Why a queued job does not start now, and when (bus time) that could change with no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
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
        for (account, provider) in world.jobs().values().chain(theirs).filter_map(Job::seat) {
            room.book(account, provider);
        }
        for p in world.jobs().values().filter(|j| j.status.has_runner()).filter_map(|j| j.profile.as_deref()) {
            *room.running.entry(p.to_owned()).or_default() += 1;
        }
        room
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
        self.load.iter().filter(|((a, _), _)| a == seat).map(|(_, n)| n).sum()
    }

    fn seat(&self, account: &str, provider: &str) -> (String, String) {
        (accounts::seat(&self.twins, account).to_owned(), provider.to_owned())
    }

    fn book(&mut self, account: &str, provider: &str) {
        *self.load.entry(self.seat(account, provider)).or_default() += 1;
    }

    /// Gives back the seat `job` holds, if any: a checker may take the seat its worker's ended run frees.
    pub fn free(&mut self, job: &Job) {
        if let Some((account, provider)) = job.seat()
            && let Some(n) = self.load.get_mut(&self.seat(account, provider))
        {
            *n = n.saturating_sub(1);
        }
    }

    /// Whether `home` takes a session on `model` now (`now_us`, bus time): a seat free under
    /// `limits.per_account` (per provider on omp), and its own state and quota fit
    /// ([`accounts::fit`]). `Ok` is how good a fit it is.
    pub fn admit(&self, limits: &Limits, home: &Home, model: Option<&str>, now_us: u64) -> Result<f64, Refused> {
        let key = home.key();
        let provider = accounts::provider_of(model, home.harness);
        let taken = self.load.get(&self.seat(&key, provider)).copied().unwrap_or(0);
        let busy = (taken >= limits.per_account).then(|| {
            let on = if provider.is_empty() { String::new() } else { format!(" on {provider}") };
            let s = if limits.per_account == 1 { "" } else { "s" };
            format!("busy ({taken} of {} seat{s}{on})", limits.per_account)
        });
        match (accounts::fit(home, self.accounts.get(&key), model, limits, now_us), busy) {
            (Ok(score), None) => Ok(score),
            (fit, busy) => Err(Refused { busy, fit: fit.err() }),
        }
    }

    /// Why `j`, queued and waiting for no other job, does not start at `now_us` by `cfg`'s rules:
    /// its profile's limits (unless a decision forced it past them), free disk for a build job,
    /// or no account it may use that [`Room::admit`]s it on its model there. `None` when it would
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
                return Err(Hold { why, until });
            }
        }
        if !j.readonly() && !self.disk_ok {
            return Err(Hold { why: format!("free disk below {} GB", cfg.limits.min_free_disk_gb), until: None });
        }
        // Every account of every harness the job allows, judged the same way: on the meters its
        // harness says govern the job's model there.
        let mut why = Vec::new();
        let mut fits = Vec::new();
        for a in self.found.iter().filter(|a| may_use(j, a)) {
            match self.admit(&cfg.limits, a, model_for(cfg, j, a.harness).as_deref(), now_us) {
                Ok(score) => fits.push((a, score)),
                Err(r) => why.push(format!("{} {}", a.key(), r.describe())),
            }
        }
        if fits.is_empty() {
            let why = if why.is_empty() { "the job's harness or pin allows no account".into() } else { why.join("; ") };
            return Err(Hold { why: format!("no account has room ({why})"), until: None });
        }
        Ok(fits)
    }

    /// Books `j`'s start on `home` at `now_us`, so the next job of the pass is judged with it
    /// running: its seat, and its profile's counts. Returns what its dispatch records.
    pub fn take(&mut self, cfg: &Config, j: &Job, home: &Home, now_us: u64) -> Start {
        let model = model_for(cfg, j, home.harness);
        let provider = accounts::provider_of(model.as_deref(), home.harness).to_owned();
        let account = home.key();
        self.book(&account, &provider);
        if let Some(name) = j.profile.as_deref().filter(|n| cfg.profiles.contains_key(*n)) {
            *self.running.entry(name.to_owned()).or_default() += 1;
            self.starts.entry(name.to_owned()).or_default().push(now_us);
        }
        Start { job: j.name.clone(), account, model, provider }
    }
}

/// What `world` and `others` know of the accounts `found`, merged and settled against their files.
fn heard(found: &[Home], world: &World, others: &[&World]) -> BTreeMap<String, AccountState> {
    let mut heard = World::default();
    heard.accounts.clone_from(&world.accounts);
    heard.hear_accounts(others.iter().copied());
    accounts::settle(&mut heard, found);
    heard.accounts
}
