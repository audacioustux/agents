//! What one frame of the board shows, taken from every project's fold in one read.
//! Building it touches no terminal; only the selected job's log and answer are
//! read later, by the job pane.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::systems::quota::AccountsView;
use crate::systems::view::machine::{Machine, Project};
use crate::systems::view::{self, decision_cells, listed, row_cells};
use crate::world::{Decision, Job, Status};

use super::pane::Column;

/// A job on the board, by its project and name: the selection follows it as rows move.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Key {
    pub project: String,
    pub name: String,
}

/// A listed job.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: Key,
    pub status: Status,
    /// Its row: a cell per column of [`Board::job_columns`], `-` where there is nothing to say.
    pub cells: Vec<String>,
    /// Its whole task, which heads its pane.
    pub task: String,
    /// What follows the task there: where the job stands.
    pub head: Vec<String>,
    /// Its project's jobs directory, where its log and answer are.
    pub jobs_dir: PathBuf,
}

impl Entry {
    pub fn file(&self, ext: &str) -> PathBuf {
        self.jobs_dir.join(format!("{}.{ext}", self.key.name))
    }
}

/// One frame's worth of the board.
#[derive(Debug, Default)]
pub struct Board {
    /// The jobs pane's title: the project's name, or `every project`.
    pub title: String,
    /// What follows it, after any diagnostic: the jobs running and queued, per project when every
    /// project shows (unbounded, so a narrow title cuts these first).
    pub counts: String,
    pub everywhere: bool,
    pub jobs: Vec<Entry>,
    pub decisions: Vec<Vec<String>>,
    pub accounts_head: String,
    pub accounts: Vec<Vec<String>>,
    pub ready: Vec<bool>,
    /// What an empty jobs list or accounts pane says instead.
    pub no_jobs: String,
    pub no_accounts: String,
    /// The projects that could not be read, each with why.
    pub unreadable: Vec<String>,
}

const fn col(head: &'static str, cap: usize) -> Column {
    Column { head, cap }
}

/// The jobs' columns, the project's first when the board shows every project.
const JOBS: [Column; 10] = [
    col("project", 16),
    col("job", 28),
    col("state", 8),
    col("account", 20),
    col("as", 18),
    col("model", 16),
    col("since", 5),
    col("spent", 8),
    col("disk", 7),
    col("task", 0),
];
const DECISIONS: [Column; 5] =
    [col("project", 16), col("decision", 30), col("job", 28), col("point", 16), col("until", 0)];
pub const ACCOUNTS: [Column; 5] =
    [col("account", 22), col("signed in as", 18), col("state", 24), col("sessions", 10), col("deciding meter", 0)];

impl Board {
    /// The board of the projects `m` shows (`everywhere`, or ours alone), recent jobs or `all`.
    pub fn new(cfg: &Config, m: &Machine, accounts: AccountsView, all: bool, everywhere: bool, now: u64) -> Self {
        let everywhere = everywhere || !m.projects.iter().any(|p| p.own);
        let shown: Vec<&Project> = m.shown(everywhere).collect();
        // The decisions waiting, found once per project rather than once per job.
        let mut waiting: HashMap<(&str, &str), Vec<&Decision>> = HashMap::new();
        for p in &shown {
            for d in crate::systems::decide::pending(&p.world, now) {
                waiting.entry((p.name.as_str(), d.job.as_str())).or_default().push(d);
            }
        }
        let jobs: Vec<Entry> = listed(shown.iter().copied(), all, now)
            .into_iter()
            .map(|(p, j)| {
                let on = waiting.get(&(p.name.as_str(), j.name.as_str())).map_or(&[][..], Vec::as_slice);
                entry(cfg, p, j, on, everywhere, now)
            })
            .collect();
        let older = listed(shown.iter().copied(), true, now).len() - listed(shown.iter().copied(), false, now).len();
        let elsewhere = if everywhere { 0 } else { listed(m.projects.iter().filter(|p| !p.own), all, now).len() };
        let harnesses: Vec<&str> = cfg.harnesses().iter().map(|h| h.id()).collect();
        let harnesses = if harnesses.is_empty() {
            crate::harness::ALL.iter().map(|h| h.id()).collect::<Vec<_>>().join(", ")
        } else {
            harnesses.join(", ")
        };
        Self {
            title: if everywhere {
                "every project".to_owned()
            } else {
                shown.first().map_or_else(String::new, |p| p.name.clone())
            },
            counts: counts(&shown, everywhere, now),
            everywhere,
            decisions: decision_cells(shown.iter().copied(), everywhere, now),
            no_jobs: no_jobs(!all && older > 0, older, elsewhere),
            no_accounts: format!(
                "No accounts found for {harnesses}. Sign one in: CLAUDE_CONFIG_DIR=~/.claude.NAME claude, or omp with a profile under ~/.omp/profiles"
            ),
            jobs,
            accounts_head: format!(" {} ", accounts.head),
            // An account's empty cell says `-`; a sub-row for another model leaves blank what it shares.
            accounts: accounts
                .cells
                .into_iter()
                .map(|r| if r.first().is_some_and(|c| c.starts_with("  ")) { r } else { dashed(r) })
                .collect(),
            ready: accounts.ready,
            unreadable: if everywhere { m.unreadable.clone() } else { Vec::new() },
        }
    }

    pub fn job_columns(&self) -> &'static [Column] {
        if self.everywhere { &JOBS } else { &JOBS[1..] }
    }

    pub fn decision_columns(&self) -> &'static [Column] {
        if self.everywhere { &DECISIONS } else { &DECISIONS[1..] }
    }

    pub fn find(&self, key: &Key) -> Option<usize> {
        self.jobs.iter().position(|e| e.key == *key)
    }
}

fn entry(cfg: &Config, p: &Project, j: &Job, waiting: &[&Decision], everywhere: bool, now: u64) -> Entry {
    let cells = row_cells(cfg, p, j, everywhere, now);
    Entry {
        key: Key { project: p.name.clone(), name: j.name.clone() },
        status: j.status,
        cells: dashed(cells),
        task: j.task.clone(),
        head: head(cfg, p, j, waiting, now),
        jobs_dir: p.jobs.clone(),
    }
}

/// The cells with `-` for each empty one, so a column never looks missing.
fn dashed(cells: Vec<String>) -> Vec<String> {
    cells.into_iter().map(|c| if c.is_empty() { "-".to_owned() } else { c }).collect()
}

/// What a job's pane says under its task: where it stands, what it runs as, why it waits or
/// ended, its checks, reviews and notes, and the decisions it waits on.
fn head(cfg: &Config, p: &Project, j: &Job, waiting: &[&Decision], now: u64) -> Vec<String> {
    let place = j.account.as_deref().map_or_else(|| "waiting for an account".to_owned(), |a| format!("on {a}"));
    let mut stand = vec![format!("{} {place}", j.status.as_str())];
    if j.attempts > 1 {
        stand.push(format!("attempt {}", j.attempts));
    }
    if j.turns > 0 {
        stand.push(format!("{} turns", j.turns));
    }
    if j.cost_usd > 0.0 {
        stand.push(format!("${:.2}", j.cost_usd));
    }
    stand.extend(j.branch.clone());
    let mut head = vec![stand.join(" · ")];
    head.extend(runs_as(p.own.then_some(cfg), j));
    if let Some(r) = view::reason(p.own.then_some(cfg), &p.world, j, now) {
        head.push(if j.status == Status::Queued { r } else { format!("why: {r}") });
    }
    head.extend(view::notes(j));
    for d in waiting {
        head.push(format!(
            "decision waiting at {} until {}: relay decide answer {} --action ...",
            d.point.as_str(),
            crate::systems::decide::clock(d.parked_until.unwrap_or(0)),
            d.id
        ));
    }
    head
}

/// What a job runs as, for its pane: its profile (else nothing, the model says it), the full model
/// id (with its harness when several are still possible) and the effort.
fn runs_as(cfg: Option<&Config>, j: &Job) -> Option<String> {
    let models = view::models(cfg, j);
    let model = match models.as_slice() {
        [] => None,
        [(_, m)] => Some(format!("model {m}")),
        many => Some(format!("model {}", many.iter().map(|(h, m)| format!("{h}: {m}")).collect::<Vec<_>>().join(", "))),
    };
    let parts: Vec<String> =
        j.profile.iter().cloned().chain(model).chain(j.effort.as_ref().map(|e| format!("effort {e}"))).collect();
    (!parts.is_empty()).then(|| format!("as {}", parts.join(" · ")))
}

/// `2 running, 1 queued`, or for every project each one's counts after its name.
fn counts(shown: &[&Project], everywhere: bool, now: u64) -> String {
    let counts = |p: &Project| {
        let n = |s: Status| p.world.jobs().values().filter(|j| j.status == s).count();
        let mut c = vec![format!("{} running", n(Status::Running))];
        if n(Status::Queued) > 0 {
            c.push(format!("{} queued", n(Status::Queued)));
        }
        c.join(", ")
    };
    if !everywhere {
        return shown.first().map_or_else(String::new, |p| counts(p));
    }
    let busy: Vec<String> = shown
        .iter()
        .filter(|p| p.world.jobs().values().any(|j| view::recent(j, now)))
        .map(|p| format!("{} {}", p.name, counts(p)))
        .collect();
    if busy.is_empty() { "nothing recent".to_owned() } else { busy.join(" · ") }
}

/// What an empty jobs list says: what is hidden, and the key that shows it.
fn no_jobs(older_hidden: bool, older: usize, elsewhere: usize) -> String {
    let mut hints = Vec::new();
    if older_hidden {
        hints.push(format!("press a to show {older} older one{}", if older == 1 { "" } else { "s" }));
    }
    if elsewhere > 0 {
        hints.push(format!("press p for the {elsewhere} in other projects"));
    }
    if hints.is_empty() {
        "No jobs yet. Queue one: relay queue add NAME --text 'what to do'".to_owned()
    } else {
        let what = if older_hidden { "No jobs in the last day here" } else { "No jobs here" };
        format!("{what}; {}.", hints.join(", or "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_list_says_what_it_hides() {
        assert_eq!(no_jobs(true, 13, 0), "No jobs in the last day here; press a to show 13 older ones.");
        assert_eq!(
            no_jobs(true, 1, 2),
            "No jobs in the last day here; press a to show 1 older one, or press p for the 2 in other projects."
        );
        assert_eq!(no_jobs(false, 0, 2), "No jobs here; press p for the 2 in other projects.");
        assert!(no_jobs(false, 0, 0).starts_with("No jobs yet"));
    }
}
