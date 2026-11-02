//! queue: takes work in and lets it be withdrawn.
//! Emits: queued, released, resumed, dequeued, stop-requested.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::bus::QueueEvent;
use crate::config::valid_name;
use crate::ctx::Ctx;
use crate::os;
use crate::world::{Job, Status};

/// The capability to write queue's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> std::path::PathBuf {
        crate::bus::system_file("queue")
    }
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Queue a task. The brief is the only context the session gets.
    Add(Add),
    /// Continue an ended or checking session with a new instruction.
    Resume {
        name: String,
        #[arg(long)]
        message: String,
        /// Spend cap for this run alone, in USD; not the job's total, which `limits.job_ceiling_usd` bounds.
        #[arg(long, value_parser = usd)]
        budget: Option<f64>,
    },
    /// Stop a queued job waiting for the jobs it was queued `--after`.
    Release {
        name: String,
        /// Drop only these (repeat, or comma-separate). Default: all of them.
        #[arg(long = "from", value_delimiter = ',')]
        from: Vec<String>,
    },
    /// Withdraw a job that has not started.
    Dequeue { name: String },
    /// End a running session.
    Stop { name: String },
}

#[derive(Debug, Args)]
pub struct Add {
    name: String,
    /// The brief, as a file.
    #[arg(long, conflicts_with = "text", required_unless_present = "text")]
    prompt: Option<PathBuf>,
    /// The brief, inline.
    #[arg(long)]
    text: Option<String>,
    /// A short title for the board. Defaults to the brief's first line.
    #[arg(long)]
    task: Option<String>,
    /// Mount the repository read-only and make no clone.
    #[arg(long)]
    readonly: bool,
    /// Run only on these harnesses (repeat, or comma-separate). Default: any enabled harness.
    #[arg(long = "harness", value_delimiter = ',')]
    harnesses: Vec<String>,
    /// Run only on this account (`harness:label`), and wait for it.
    #[arg(long)]
    account: Option<String>,
    /// `harness=model` (repeatable), or a bare model when exactly one harness applies.
    #[arg(long = "model")]
    models: Vec<String>,
    #[arg(long)]
    effort: Option<String>,
    /// Spend cap for each attempt, in USD.
    #[arg(long, value_parser = usd)]
    budget: Option<f64>,
    /// Queue a read-only job even when the same question was already answered at this commit.
    #[arg(long)]
    again: bool,
    /// Start only once these jobs are done (repeat, or comma-separate). Archived jobs count; landing is a later step.
    #[arg(long = "after", value_delimiter = ',')]
    after: Vec<String>,
}

fn usd(s: &str) -> Result<f64, String> {
    s.parse().ok().filter(|x| crate::config::positive(*x)).ok_or_else(|| format!("{s:?} is not a positive amount"))
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    // From the check to the record, so two adds of one name cannot both pass.
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    let name = match &cmd {
        Cmd::Add(Add { name, .. })
        | Cmd::Resume { name, .. }
        | Cmd::Release { name, .. }
        | Cmd::Dequeue { name }
        | Cmd::Stop { name } => name.clone(),
    };
    let job = world.jobs.get(&name);
    // The job, when its status is one the command applies to.
    let when = |ok: fn(Status) -> bool| match job {
        None => fail!(NotFound, "no job named {name}"),
        Some(j) if !ok(j.status) => fail!(Conflict, "{name} is {}", j.status.as_str()),
        Some(j) => anyhow::Ok(j),
    };
    // The event, and for a stop the runner to signal once it is recorded.
    let (event, runner) = match cmd {
        Cmd::Add(add) => {
            // A job that ended before it ever started (dequeued) has nothing to keep:
            // its name is free again.
            if job.is_some_and(|j| j.status.is_live() || j.attempts > 0) {
                fail!(Conflict, "job {name} exists; pick another name, or: relay land clean {name}");
            }
            (add.queued(ctx, &world.jobs)?, None)
        }
        Cmd::Resume { message, budget, .. } => {
            when(|s| !s.is_live() || s == Status::Checking)?;
            (QueueEvent::Resumed { job: name.clone(), message, budget_usd: budget }, None)
        }
        Cmd::Release { from, .. } => {
            let waiting = &when(|s| s == Status::Queued)?.after;
            if waiting.is_empty() {
                fail!(Conflict, "{name} waits for no job");
            }
            if let Some(other) = from.iter().find(|f| !waiting.contains(f)) {
                fail!(Usage, "{name} does not wait for {other} (it waits for {})", waiting.join(", "));
            }
            (QueueEvent::Released { job: name.clone(), from }, None)
        }
        Cmd::Dequeue { .. } => {
            when(|s| s == Status::Queued)?;
            (QueueEvent::Dequeued { job: name.clone() }, None)
        }
        Cmd::Stop { .. } => (QueueEvent::StopRequested { job: name.clone() }, when(|s| s == Status::Running)?.runner),
    };
    let recorded = serde_json::to_value(&event)?["type"].clone();
    ctx.port(&Source(())).emit(event)?;
    // Only a process that is still the recorded runner is signalled, never a reused pid.
    if let Some((pid, started)) = runner
        && os::same_process(pid, started)
    {
        os::terminate_group(pid)?;
    }
    ctx.wake_dispatch()?;
    ctx.reply(&serde_json::json!({ "job": name, "recorded": recorded }), &format!("{name}: recorded"))
}

impl Add {
    /// Checks the request, stores the brief, and says what to record.
    fn queued(self, ctx: &Ctx, jobs: &BTreeMap<String, Job>) -> Result<QueueEvent> {
        let name = self.name;
        if !valid_name(&name) {
            fail!(Usage, "job names are lowercase letters, digits and dashes");
        }
        let brief = match (self.prompt, self.text) {
            (Some(p), _) => std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
            (None, Some(t)) => t,
            (None, None) => unreachable!("clap requires one"),
        };
        if brief.trim().is_empty() {
            fail!(Usage, "the brief is empty");
        }
        let after = dependencies(&name, self.after, jobs)?;
        let harnesses = allowed_harnesses(ctx, self.harnesses, self.account.as_deref())?;
        let models = parse_models(ctx, &harnesses, &self.models)?;
        if let Some((m, reason)) = models.values().find_map(|m| ctx.cfg.session.banned(m).map(|r| (m, r))) {
            fail!(Refused, "model {m} is banned: {reason}");
        }
        let commit = if self.readonly { Ctx::clean_head(&ctx.cfg.repo) } else { None };
        let reuse_key = commit.as_deref().map(|c| reuse_key(&brief, &harnesses, &models, self.effort.as_deref(), c));
        if let (Some(key), Some(commit), false) = (&reuse_key, &commit, self.again) {
            refuse_repeat(jobs, key, commit, |job| {
                std::fs::metadata(ctx.job_file(job, "result.md")).is_ok_and(|m| m.len() > 0)
            })?;
        }
        // The brief reaches the session on stdin or as a file, never in argv (/proc/<pid>/cmdline is public).
        std::io::Write::write_all(&mut Ctx::append_private(&ctx.job_file(&name, "prompt.md"))?, brief.as_bytes())?;
        let task =
            self.task.unwrap_or_else(|| brief.trim().lines().next().unwrap_or_default().chars().take(80).collect());
        Ok(QueueEvent::Queued {
            job: name,
            task,
            readonly: self.readonly,
            harnesses,
            models,
            effort: self.effort,
            budget_usd: self.budget.unwrap_or(ctx.cfg.limits.budget_usd),
            pinned: self.account,
            reuse_key,
            after,
        })
    }
}

/// The jobs to wait for, each once. Only existing jobs can be named, so no cycle can form;
/// a job naming itself is refused all the same.
fn dependencies(name: &str, asked: Vec<String>, jobs: &BTreeMap<String, Job>) -> Result<Vec<String>> {
    let mut after = Vec::new();
    for a in asked {
        if a == name {
            fail!(Usage, "{name} cannot wait for itself");
        }
        if !jobs.contains_key(&a) {
            fail!(NotFound, "--after {a}: no job named {a}");
        }
        if !after.contains(&a) {
            after.push(a);
        }
    }
    Ok(after)
}

/// The harnesses a job may use: those asked for (all must be enabled), or the
/// pinned account's, or none (meaning any enabled harness).
fn allowed_harnesses(ctx: &Ctx, asked: Vec<String>, account: Option<&str>) -> Result<Vec<String>> {
    let enabled: Vec<&str> = ctx.cfg.harnesses().iter().map(|h| h.id()).collect();
    for h in &asked {
        if !enabled.contains(&h.as_str()) {
            fail!(Refused, "harness {h} is not enabled here (enabled: {})", enabled.join(", "));
        }
    }
    let Some(key) = account else { return Ok(asked) };
    let Some(home) = crate::accounts::discover(&ctx.cfg).into_iter().find(|a| a.key() == key) else {
        fail!(NotFound, "unknown account {key}; accounts are harness:label, see: relay quota show");
    };
    if !asked.is_empty() && !asked.iter().any(|h| h == home.harness) {
        fail!(Usage, "account {key} is not on any of the harnesses asked for");
    }
    Ok(vec![home.harness.to_owned()])
}

/// `harness=model` pairs, or one bare model for the single harness that applies.
fn parse_models(ctx: &Ctx, harnesses: &[String], given: &[String]) -> Result<BTreeMap<String, String>> {
    let applicable: Vec<String> = if harnesses.is_empty() {
        ctx.cfg.harnesses().iter().map(|h| h.id().to_owned()).collect()
    } else {
        harnesses.to_vec()
    };
    let mut out = BTreeMap::new();
    for m in given {
        let (h, model) = match m.split_once('=') {
            Some((h, model)) => (h.to_owned(), model.to_owned()),
            None if applicable.len() == 1 => (applicable[0].clone(), m.clone()),
            None => fail!(
                Usage,
                "--model {m}: say which harness it is for (harness=model); this job may run on {}",
                applicable.join(", ")
            ),
        };
        if !applicable.contains(&h) {
            fail!(Usage, "--model {m}: harness {h} is not one this job may run on");
        }
        out.insert(h, model);
    }
    Ok(out)
}

/// What names a read-only question: the brief, where and how it may run, and
/// the clean commit it reads (a dirty tree gets no key: no commit names it).
fn reuse_key(
    brief: &str,
    harnesses: &[String],
    models: &BTreeMap<String, String>,
    effort: Option<&str>,
    commit: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut harnesses = harnesses.to_vec();
    harnesses.sort();
    harnesses.dedup();
    // A JSON array is an unambiguous encoding; the leading 1 versions it.
    let fields = serde_json::json!([1, brief, harnesses, models, effort, commit]).to_string();
    Sha256::digest(fields).iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().concat()
}

/// Refuses a question another job has answered at `commit`, or is answering.
/// An answer counts only if its session read that commit: the job may have
/// waited for quota while the tree moved on.
fn refuse_repeat(
    jobs: &BTreeMap<String, Job>,
    key: &str,
    commit: &str,
    has_result: impl Fn(&str) -> bool,
) -> Result<()> {
    let same = || jobs.values().filter(|j| j.reuse_key.as_deref() == Some(key));
    if let Some(j) = same().find(|j| j.status.is_live()) {
        fail!(Conflict, "already being answered by {} ({}); to ask again anyway: --again", j.name, j.status.as_str());
    }
    if let Some(j) = same()
        .filter(|j| j.status == Status::Done && j.ran_at.as_deref() == Some(commit) && has_result(&j.name))
        .max_by_key(|j| j.created_at)
    {
        fail!(
            Conflict,
            "{} already answered this at this commit; read it with: relay view result {}; to ask again anyway: --again",
            j.name,
            j.name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Event, Stamped};
    use crate::world::World;

    fn key(brief: &str, models: &[(&str, &str)], commit: &str) -> String {
        let models = models.iter().map(|(h, m)| ((*h).to_owned(), (*m).to_owned())).collect();
        reuse_key(brief, &["claude".to_owned()], &models, Some("high"), commit)
    }

    #[test]
    fn key_names_the_question() {
        let base = key("review x", &[("claude", "opus")], "abc");
        assert_eq!(base, key("review x", &[("claude", "opus")], "abc"));
        assert_ne!(base, key("review y", &[("claude", "opus")], "abc"));
        assert_ne!(base, key("review x", &[("claude", "sonnet")], "abc"));
        assert_ne!(base, key("review x", &[("claude", "opus")], "abd"));
        assert_ne!(base, reuse_key("review x", &[], &BTreeMap::new(), Some("high"), "abc"));
        assert_ne!(
            base,
            reuse_key(
                "review x",
                &["claude".to_owned()],
                &[("claude".to_owned(), "opus".to_owned())].into(),
                None,
                "abc"
            )
        );
    }

    #[test]
    fn key_is_stable() {
        // Pinned, so a change to the encoding (or the hash) is a deliberate one.
        assert_eq!(
            reuse_key("b", &[], &BTreeMap::new(), None, "c"),
            "64fbefd3a17fb06003b60075168536c4b6fc46e497de0bfb58c1582c4e4ce878"
        );
        let a = reuse_key("b", &["x".into(), "y".into()], &BTreeMap::new(), None, "c");
        let b = reuse_key("b", &["y".into(), "x".into()], &BTreeMap::new(), None, "c");
        assert_eq!(a, b);
    }

    fn jobs(entries: &[(&str, Option<&str>, Option<crate::bus::Outcome>)]) -> BTreeMap<String, Job> {
        let mut events = Vec::new();
        for (t, (name, key, outcome)) in (1..).zip(entries) {
            events.push(Stamped {
                t,
                from: None,
                event: Event::Queue(QueueEvent::Queued {
                    job: (*name).into(),
                    task: "t".into(),
                    readonly: true,
                    harnesses: Vec::new(),
                    models: BTreeMap::new(),
                    effort: None,
                    budget_usd: 1.0,
                    pinned: None,
                    reuse_key: key.map(Into::into),
                    after: Vec::new(),
                }),
            });
            if let Some(outcome) = outcome {
                events.push(Stamped {
                    t,
                    from: None,
                    event: Event::Session(crate::bus::SessionEvent::Started {
                        job: (*name).into(),
                        account: "claude:a".into(),
                        session_id: "s".into(),
                        commit: Some("c".into()),
                    }),
                });
                events.push(Stamped {
                    t,
                    from: None,
                    event: Event::Session(crate::bus::SessionEvent::Ended {
                        job: (*name).into(),
                        outcome: *outcome,
                        cost_usd: 0.1,
                        turns: 1,
                        reason: None,
                    }),
                });
            }
        }
        World::fold(&events).jobs
    }

    fn refusal(r: Result<()>) -> String {
        let e = r.unwrap_err();
        assert_eq!(e.downcast_ref::<crate::reply::Fail>().map(|f| f.kind), Some(crate::reply::Kind::Conflict));
        e.to_string()
    }

    #[test]
    fn a_done_answer_is_not_bought_twice() {
        use crate::bus::Outcome;
        let js = jobs(&[("old", Some("k"), Some(Outcome::Done)), ("other", Some("j"), Some(Outcome::Done))]);
        let msg = refusal(refuse_repeat(&js, "k", "c", |_| true));
        assert!(msg.contains("relay view result old"), "{msg}");
        // No result file, a failed run, another key, or an answer read at another commit: ask.
        refuse_repeat(&js, "k", "c", |_| false).unwrap();
        refuse_repeat(&jobs(&[("old", Some("k"), Some(Outcome::Failed))]), "k", "c", |_| true).unwrap();
        refuse_repeat(&js, "z", "c", |_| true).unwrap();
        refuse_repeat(&jobs(&[("old", None, Some(Outcome::Done))]), "k", "c", |_| true).unwrap();
        refuse_repeat(&js, "k", "d", |_| true).unwrap();
    }

    #[test]
    fn a_question_being_answered_is_not_asked_again() {
        let msg = refusal(refuse_repeat(&jobs(&[("busy", Some("k"), None)]), "k", "c", |_| false));
        assert!(msg.contains("already being answered by busy"), "{msg}");
    }
}
