//! `relay view stats`: a read-only fold over the bus. Per profile and model, what became of the jobs
//! queued in a window, what they cost, how often they were retried, resumed, checked and reviewed,
//! and how long they waited and ran; and which accounts did the work (labels only, never who they are).
//! It reads events that already exist and writes none.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use crate::bus::{
    Action, DecideEvent, DispatchEvent, Event, Outcome, QueueEvent, SessionEvent, Stamped, Verdict, now_micros,
};
use crate::ctx::Ctx;
use crate::world::World;

/// `30m`, `12h`, `7d`, `2w`, or a bare number of seconds, in microseconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
    let secs = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    n.checked_mul(secs)?.checked_mul(1_000_000)
}

/// What one incarnation of a job went through.
#[derive(Debug, Default)]
struct JobStat {
    created: u64,
    profile: Option<String>,
    models: BTreeMap<String, String>,
    /// How it ended last; `None` while it is queued or running.
    outcome: Option<Outcome>,
    cost: f64,
    retries: u32,
    resumes: u32,
    decisions: BTreeMap<String, u32>,
    checks: BTreeMap<&'static str, u32>,
    /// Reviews of this job's branch: accepts, and rejects by reason.
    accepted: u32,
    rejected: BTreeMap<String, u32>,
    waits: Vec<f64>,
    runs: Vec<f64>,
    /// When it last went into the queue, and when its run began.
    queued_since: Option<u64>,
    run_since: Option<u64>,
    /// Runs and spend per account label.
    accounts: BTreeMap<String, (u32, f64)>,
    account: Option<String>,
}

#[derive(Debug, Default)]
struct Fold {
    jobs: Vec<JobStat>,
    /// The index of each name's latest incarnation.
    current: BTreeMap<String, usize>,
    /// Which job each decision was about.
    decided_for: BTreeMap<String, String>,
    /// Decisions whose carried-out action is counted already.
    counted: BTreeSet<String>,
}

impl Fold {
    fn job(&mut self, name: &str) -> Option<&mut JobStat> {
        let i = *self.current.get(name)?;
        self.jobs.get_mut(i)
    }

    /// Takes in `s`, which `before` (the world just ahead of it) has not seen.
    fn observe(&mut self, before: &World, s: &Stamped) {
        let t = s.t;
        match &s.event {
            Event::Queue(QueueEvent::Queued { job, models, profile, .. }) => {
                self.current.insert(job.clone(), self.jobs.len());
                self.jobs.push(JobStat {
                    created: t,
                    profile: profile.clone(),
                    models: models.clone(),
                    queued_since: Some(t),
                    ..JobStat::default()
                });
            }
            Event::Queue(QueueEvent::Resumed { job, models, .. }) => {
                if let Some(j) = self.job(job) {
                    j.resumes += 1;
                    j.outcome = None;
                    j.queued_since = Some(t);
                    j.models.extend(models.clone());
                }
            }
            Event::Queue(QueueEvent::Rerouted { job, models, profile, .. }) => {
                if let Some(j) = self.job(job) {
                    j.models.clone_from(models);
                    j.profile.clone_from(profile);
                }
            }
            Event::Queue(QueueEvent::Dequeued { job }) => {
                if let Some(j) = self.job(job) {
                    (j.outcome, j.queued_since) = (Some(Outcome::Stopped), None);
                }
            }
            Event::Queue(QueueEvent::Unchained { job, .. }) => {
                // Only a review still waiting to run is stopped by it; a running one finishes.
                if let Some(j) = self.job(job).filter(|j| j.queued_since.is_some()) {
                    (j.outcome, j.queued_since) = (Some(Outcome::Stopped), None);
                }
            }
            Event::Dispatch(DispatchEvent::Dispatched { job, account, .. }) => {
                if let Some(j) = self.job(job) {
                    j.waits.extend(j.queued_since.take().map(|q| micros(t, q)));
                    j.run_since = Some(t);
                    j.accounts.entry(account.clone()).or_default().0 += 1;
                    j.account = Some(account.clone());
                }
            }
            Event::Dispatch(DispatchEvent::Reaped { job }) => {
                let stopped = before.jobs().get(job).is_some_and(|j| j.stop_requested);
                if let Some(j) = self.job(job) {
                    j.runs.extend(j.run_since.take().map(|r| micros(t, r)));
                    j.outcome = Some(if stopped { Outcome::Stopped } else { Outcome::Failed });
                }
            }
            Event::Session(e) => self.session(t, e),
            Event::Decide(DecideEvent::DecisionRequested { id, job, .. }) => {
                self.decided_for.insert(id.clone(), job.clone());
            }
            Event::Decide(DecideEvent::Decided { id, action, cost_usd, refused, .. }) => {
                let Some(job) = self.decided_for.get(id).cloned() else { return };
                let first = refused.is_none() && *action != Action::Defer && self.counted.insert(id.clone());
                if let Some(j) = self.job(&job) {
                    j.cost += cost_usd;
                    if first {
                        *j.decisions.entry(action.name().to_owned()).or_default() += 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn session(&mut self, t: u64, e: &SessionEvent) {
        match e {
            SessionEvent::FollowUp { job, .. } => {
                if let Some(j) = self.job(job) {
                    j.resumes += 1;
                }
            }
            SessionEvent::Requeued { job, profile, models, .. } => {
                if let Some(j) = self.job(job) {
                    j.profile = Some(profile.clone());
                    j.models.clone_from(models);
                }
            }
            SessionEvent::Checked { job, check } => {
                if let Some(j) = self.job(job).filter(|_| check.decision.is_none()) {
                    j.cost += check.cost_usd;
                }
            }
            SessionEvent::BarChecked { job, result, .. } => {
                if let Some(j) = self.job(job) {
                    *j.checks.entry(result.as_str()).or_default() += 1;
                }
            }
            SessionEvent::Reviewed { of, verdict, reason, .. } => {
                if let Some(j) = self.job(of) {
                    match (verdict, reason) {
                        (Verdict::Accept, _) => j.accepted += 1,
                        (Verdict::Reject, r) => {
                            *j.rejected.entry(r.map_or("other", |r| r.as_str()).to_owned()).or_default() += 1;
                        }
                    }
                }
            }
            SessionEvent::Ended { job, outcome, cost_usd, .. } => {
                if let Some(j) = self.job(job) {
                    j.cost += cost_usd;
                    if let Some(a) = j.account.clone() {
                        j.accounts.entry(a).or_default().1 += cost_usd;
                    }
                    j.runs.extend(j.run_since.take().map(|r| micros(t, r)));
                    match outcome {
                        Outcome::Retry | Outcome::Rerun => {
                            j.retries += 1;
                            j.outcome = None;
                            j.queued_since = Some(t);
                        }
                        o => j.outcome = Some(*o),
                    }
                }
            }
            _ => {}
        }
    }
}

/// Seconds between two bus times.
#[allow(clippy::cast_precision_loss)]
fn micros(later: u64, earlier: u64) -> f64 {
    later.saturating_sub(earlier) as f64 / 1e6
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[mid] } else { f64::midpoint(v[mid - 1], v[mid]) })
}

#[derive(Debug, Default, Serialize)]
struct Jobs {
    total: u32,
    done: u32,
    failed: u32,
    stopped: u32,
    /// Queued or running.
    open: u32,
}

#[derive(Debug, Default, Serialize)]
struct Reviews {
    accept: u32,
    reject: u32,
    by_reason: BTreeMap<String, u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct Group {
    jobs: Jobs,
    spend_usd: f64,
    retries: u32,
    resumes: u32,
    decisions: BTreeMap<String, u32>,
    checks: BTreeMap<String, u32>,
    reviews: Reviews,
    queue_wait_median_s: Option<f64>,
    run_median_s: Option<f64>,
    #[serde(skip)]
    waits: Vec<f64>,
    #[serde(skip)]
    runs: Vec<f64>,
}

impl Group {
    fn add(&mut self, j: &JobStat) {
        self.jobs.total += 1;
        match j.outcome {
            Some(Outcome::Done) => self.jobs.done += 1,
            Some(Outcome::Failed) => self.jobs.failed += 1,
            Some(Outcome::Stopped) => self.jobs.stopped += 1,
            _ => self.jobs.open += 1,
        }
        self.spend_usd += j.cost;
        self.retries += j.retries;
        self.resumes += j.resumes;
        for (k, n) in &j.decisions {
            *self.decisions.entry(k.clone()).or_default() += n;
        }
        for (k, n) in &j.checks {
            *self.checks.entry((*k).to_owned()).or_default() += n;
        }
        self.reviews.accept += j.accepted;
        for (k, n) in &j.rejected {
            self.reviews.reject += n;
            *self.reviews.by_reason.entry(k.clone()).or_default() += n;
        }
        self.waits.extend(&j.waits);
        self.runs.extend(&j.runs);
    }

    fn finish(mut self) -> Self {
        (self.queue_wait_median_s, self.run_median_s) = (median(&self.waits), median(&self.runs));
        self
    }
}

#[derive(Debug, Serialize)]
pub struct AccountUse {
    account: String,
    runs: u32,
    run_share: f64,
    spend_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    /// `(profile, model)` groups, by profile then model; `-` where a job had none.
    groups: Vec<(String, String, Group)>,
    total: Group,
    accounts: Vec<AccountUse>,
}

/// The stats of the jobs queued at or after `since` (bus time), from the bus's whole history.
pub fn compute(events: &[Stamped], since: u64) -> Stats {
    let mut fold = Fold::default();
    let mut world = World::default();
    for s in events {
        let stale = matches!(&s.event, Event::Session(e) if !world.current(s, e));
        if !stale {
            fold.observe(&world, s);
        }
        world.apply(s);
    }
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    let mut total = Group::default();
    let mut accounts: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    for j in fold.jobs.iter().filter(|j| j.created >= since) {
        let profile = j.profile.clone().unwrap_or_else(|| "-".into());
        let model =
            if j.models.is_empty() { "-".to_owned() } else { j.models.values().cloned().collect::<Vec<_>>().join(",") };
        groups.entry((profile, model)).or_default().add(j);
        total.add(j);
        for (a, (runs, spend)) in &j.accounts {
            let e = accounts.entry(a.clone()).or_default();
            e.0 += runs;
            e.1 += spend;
        }
    }
    let runs: u32 = accounts.values().map(|a| a.0).sum();
    let mut accounts: Vec<AccountUse> = accounts
        .into_iter()
        .map(|(account, (n, spend_usd))| AccountUse {
            account,
            runs: n,
            run_share: if runs == 0 { 0.0 } else { f64::from(n) / f64::from(runs) },
            spend_usd,
        })
        .collect();
    accounts.sort_by(|a, b| b.runs.cmp(&a.runs).then_with(|| a.account.cmp(&b.account)));
    Stats {
        groups: groups.into_iter().map(|((p, m), g)| (p, m, g.finish())).collect(),
        total: total.finish(),
        accounts,
    }
}

fn dur(s: Option<f64>) -> String {
    match s {
        None => "-".into(),
        Some(s) if s < 120.0 => format!("{s:.0}s"),
        Some(s) if s < 7200.0 => format!("{:.0}m", s / 60.0),
        Some(s) => format!("{:.1}h", s / 3600.0),
    }
}

fn counts(m: &BTreeMap<String, u32>) -> String {
    m.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
}

fn lines(stats: &Stats, project: &str, since: Option<&str>) -> String {
    let t = &stats.total;
    let window = since.map_or(String::from("all time"), |s| format!("queued in the last {s}"));
    let mut out = vec![format!("{project}: {} job(s), {window}, ${:.2} spent", t.jobs.total, t.spend_usd)];
    if t.jobs.total == 0 {
        return out.join("\n");
    }
    let row = |p: &str, m: &str, g: &Group| {
        vec![
            p.to_owned(),
            m.to_owned(),
            g.jobs.total.to_string(),
            format!("{} done", g.jobs.done),
            format!("{} failed", g.jobs.failed),
            format!("{} stopped", g.jobs.stopped),
            format!("{} open", g.jobs.open),
            format!("${:.2}", g.spend_usd),
            format!("{} retries", g.retries),
            format!("{} resumes", g.resumes),
            format!("wait {}", dur(g.queue_wait_median_s)),
            format!("run {}", dur(g.run_median_s)),
        ]
    };
    let mut rows: Vec<Vec<String>> = stats.groups.iter().map(|(p, m, g)| row(p, m, g)).collect();
    rows.push(row("all", "", t));
    out.extend(super::table(&rows).into_iter().map(|r| format!("  {r}")));
    for (p, m, g) in &stats.groups {
        let mut said = Vec::new();
        if !g.decisions.is_empty() {
            said.push(format!("decisions: {}", counts(&g.decisions)));
        }
        if !g.checks.is_empty() {
            said.push(format!("checks: {}", counts(&g.checks)));
        }
        if g.reviews.accept + g.reviews.reject > 0 {
            let by = if g.reviews.by_reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", counts(&g.reviews.by_reason))
            };
            said.push(format!("reviews: {} accepted, {} rejected{by}", g.reviews.accept, g.reviews.reject));
        }
        if !said.is_empty() {
            out.push(format!("  {p} {m}: {}", said.join("; ")));
        }
    }
    if !stats.accounts.is_empty() {
        out.push(String::new());
        out.push("accounts (share of runs)".into());
        let rows: Vec<Vec<String>> = stats
            .accounts
            .iter()
            .map(|a| {
                vec![
                    a.account.clone(),
                    format!("{:.0}%", a.run_share * 100.0),
                    format!("{} runs", a.runs),
                    format!("${:.2}", a.spend_usd),
                ]
            })
            .collect();
        out.extend(super::table(&rows).into_iter().map(|r| format!("  {r}")));
    }
    out.join("\n")
}

pub fn run(ctx: &Ctx, since: Option<&str>) -> Result<()> {
    let window = match since {
        None => 0,
        Some(s) => match parse_duration(s) {
            Some(d) => now_micros().saturating_sub(d),
            None => fail!(Usage, "--since {s}: a duration such as 30m, 12h, 7d or 2w"),
        },
    };
    let stats = compute(&ctx.events()?, window);
    let value = json!({
        "project": ctx.cfg.name,
        "since": since,
        "groups": stats.groups.iter().map(|(p, m, g)| {
            let mut v = serde_json::to_value(g).unwrap_or_default();
            v["profile"] = p.clone().into();
            v["model"] = m.clone().into();
            v
        }).collect::<Vec<_>>(),
        "total": stats.total,
        "accounts": stats.accounts,
    });
    ctx.reply(&value, &lines(&stats, &ctx.cfg.name, since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BarResult;

    #[test]
    fn durations_are_a_number_and_a_unit() {
        assert_eq!(parse_duration("90"), Some(90_000_000));
        assert_eq!(parse_duration("30m"), Some(1_800_000_000));
        assert_eq!(parse_duration("2w"), Some(1_209_600_000_000));
        assert_eq!(parse_duration("0d"), None);
        assert_eq!(parse_duration("d"), None);
        assert_eq!(parse_duration("3y"), None);
        assert_eq!(parse_duration("-1h"), None);
    }

    #[test]
    fn the_median_is_the_middle() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }

    fn at(t: u64, event: Event) -> Stamped {
        Stamped { t, event, from: None }
    }

    #[test]
    fn waits_runs_and_outcomes_come_from_the_events_and_the_window_cuts_by_queueing() {
        let queued = |job: &str, profile: &str| {
            Event::Queue(QueueEvent::Queued {
                job: job.into(),
                task: "t".into(),
                readonly: false,
                harnesses: Vec::new(),
                models: [("claude".to_owned(), "m".to_owned())].into(),
                effort: None,
                budget_usd: 1.0,
                pinned: None,
                reuse_key: None,
                after: Vec::new(),
                profile: Some(profile.into()),
                fork: None,
                parent: None,
                depth: 0,
                acceptance: crate::bus::Acceptance::None,
            })
        };
        let dispatched = |job: &str| {
            Event::Dispatch(DispatchEvent::Dispatched {
                job: job.into(),
                account: "claude:a".into(),
                model: None,
                provider: String::new(),
                runner_pid: 1,
                runner_started: 1,
            })
        };
        let ended = |job: &str, outcome| {
            Event::Session(SessionEvent::Ended { job: job.into(), outcome, cost_usd: 0.5, turns: 1, reason: None })
        };
        let s = 1_000_000;
        let events = [
            at(s, queued("old", "p")),
            at(2 * s, dispatched("old")),
            at(3 * s, ended("old", Outcome::Done)),
            at(100 * s, queued("a", "p")),
            at(110 * s, dispatched("a")),
            at(130 * s, ended("a", Outcome::Retry)),
            at(140 * s, dispatched("a")),
            at(150 * s, ended("a", Outcome::Failed)),
            at(100 * s, queued("b", "p")),
            at(104 * s, dispatched("b")),
            at(108 * s, ended("b", Outcome::Done)),
            at(200 * s, queued("c", "q")),
        ];
        let all = compute(&events, 0);
        assert_eq!(all.total.jobs.total, 4);
        let since = compute(&events, 50 * s);
        assert_eq!(since.total.jobs.total, 3, "the window counts jobs queued within it");
        let p = since.groups.iter().find(|g| g.0 == "p").map(|g| &g.2).unwrap();
        assert_eq!((p.jobs.done, p.jobs.failed, p.jobs.open), (1, 1, 0));
        assert_eq!(p.retries, 1);
        // Waits: a 10 s, then 10 s after its retry; b 4 s. Runs: a 20 s and 10 s, b 4 s.
        assert_eq!(p.queue_wait_median_s, Some(10.0));
        assert_eq!(p.run_median_s, Some(10.0));
        assert!((p.spend_usd - 1.5).abs() < 1e-9, "three runs at 0.50");
        let q = since.groups.iter().find(|g| g.0 == "q").map(|g| &g.2).unwrap();
        assert_eq!((q.jobs.open, q.queue_wait_median_s), (1, None), "queued, not yet started");
        assert_eq!(since.accounts.len(), 1);
        assert_eq!((since.accounts[0].account.as_str(), since.accounts[0].runs), ("claude:a", 3));
    }

    #[test]
    fn a_result_is_counted_by_its_name() {
        assert_eq!(BarResult::CheckProvesNothing.as_str(), "check-proves-nothing");
    }
}
