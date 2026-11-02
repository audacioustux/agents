//! queue: takes work in and lets it be withdrawn.
//! Emits: queued, resumed, dequeued, stop-requested.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::bus::QueueEvent;
use crate::config::valid_name;
use crate::ctx::Ctx;
use crate::os;
use crate::world::Status;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Queue a task. The brief is the only context the session gets.
    Add(Add),
    /// Continue an ended session with a new instruction.
    Resume {
        name: String,
        #[arg(long)]
        message: String,
        #[arg(long, value_parser = usd)]
        budget: Option<f64>,
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
}

fn usd(s: &str) -> Result<f64, String> {
    s.parse().ok().filter(|x| crate::config::positive(*x)).ok_or_else(|| format!("{s:?} is not a positive amount"))
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    // From the check to the record, so two adds of one name cannot both pass.
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    let name = match &cmd {
        Cmd::Add(Add { name, .. }) | Cmd::Resume { name, .. } | Cmd::Dequeue { name } | Cmd::Stop { name } => {
            name.clone()
        }
    };
    let job = world.jobs.get(&name);
    // The job, when its status is one the command applies to.
    let when = |ok: fn(Status) -> bool| match job {
        None => bail!("no job named {name}"),
        Some(j) if !ok(j.status) => bail!("{name} is {}", j.status.as_str()),
        Some(j) => Ok(j),
    };
    let event = match cmd {
        Cmd::Add(add) => {
            if job.is_some() {
                bail!("job {name} exists; pick another name, or: relay land clean {name}");
            }
            add.queued(ctx)?
        }
        Cmd::Resume { message, budget, .. } => {
            when(|s| !s.is_live())?;
            QueueEvent::Resumed { job: name.clone(), message, budget_usd: budget }
        }
        Cmd::Dequeue { .. } => {
            when(|s| s == Status::Queued)?;
            QueueEvent::Dequeued { job: name.clone() }
        }
        Cmd::Stop { .. } => {
            let j = when(|s| s == Status::Running)?;
            ctx.port::<QueueEvent>().emit(QueueEvent::StopRequested { job: name.clone() })?;
            // Only a process that is still the recorded runner is signalled, never a reused pid.
            if let Some((pid, started)) = j.runner
                && os::same_process(pid, started)
            {
                os::terminate_group(pid)?;
            }
            return finish(ctx, &name);
        }
    };
    ctx.port::<QueueEvent>().emit(event)?;
    finish(ctx, &name)
}

fn finish(ctx: &Ctx, name: &str) -> Result<()> {
    ctx.wake_dispatch()?;
    out!("{name}: recorded");
    Ok(())
}

impl Add {
    /// Checks the request, stores the brief, and says what to record.
    fn queued(self, ctx: &Ctx) -> Result<QueueEvent> {
        let name = self.name;
        if !valid_name(&name) {
            bail!("job names are lowercase letters, digits and dashes");
        }
        let brief = match (self.prompt, self.text) {
            (Some(p), _) => std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
            (None, Some(t)) => t,
            (None, None) => unreachable!("clap requires one"),
        };
        if brief.trim().is_empty() {
            bail!("the brief is empty");
        }
        let harnesses = allowed_harnesses(ctx, self.harnesses, self.account.as_deref())?;
        let models = parse_models(ctx, &harnesses, &self.models)?;
        if let Some((m, reason)) = models.values().find_map(|m| ctx.cfg.session.banned(m).map(|r| (m, r))) {
            bail!("model {m} is banned: {reason}");
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
        })
    }
}

/// The harnesses a job may use: those asked for (all must be enabled), or the
/// pinned account's, or none (meaning any enabled harness).
fn allowed_harnesses(ctx: &Ctx, asked: Vec<String>, account: Option<&str>) -> Result<Vec<String>> {
    let enabled: Vec<&str> = ctx.cfg.harnesses().iter().map(|h| h.id()).collect();
    for h in &asked {
        if !enabled.contains(&h.as_str()) {
            bail!("harness {h} is not enabled here (enabled: {})", enabled.join(", "));
        }
    }
    let Some(key) = account else { return Ok(asked) };
    let Some(home) = crate::accounts::discover(&ctx.cfg).into_iter().find(|a| a.key() == key) else {
        bail!("unknown account {key}; accounts are harness:label, see: relay quota show");
    };
    if !asked.is_empty() && !asked.iter().any(|h| h == home.harness) {
        bail!("account {key} is not on any of the harnesses asked for");
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
            None => bail!(
                "--model {m}: say which harness it is for (harness=model); this job may run on {}",
                applicable.join(", ")
            ),
        };
        if !applicable.contains(&h) {
            bail!("--model {m}: harness {h} is not one this job may run on");
        }
        out.insert(h, model);
    }
    Ok(out)
}
