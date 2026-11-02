//! queue: takes work in and lets it be withdrawn.
//! Emits: queued, resumed, dequeued, stop-requested.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::bus::QueueEvent;
use crate::config::valid_name;
use crate::ctx::Ctx;
use crate::os;
use crate::world::Status;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Queue a task. The brief is the only context the session gets.
    Add {
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
        /// Run only on this account, and wait for it.
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        /// Spend cap for each attempt, in USD.
        #[arg(long)]
        budget: Option<f64>,
    },
    /// Continue an ended session with a new instruction.
    Resume {
        name: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        budget: Option<f64>,
    },
    /// Withdraw a job that has not started.
    Dequeue { name: String },
    /// End a running session.
    Stop { name: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    let port = ctx.port::<QueueEvent>();
    let world = ctx.world()?;
    let name = match &cmd {
        Cmd::Add { name, .. } | Cmd::Resume { name, .. } | Cmd::Dequeue { name } | Cmd::Stop { name } => name.clone(),
    };
    let job = world.jobs.get(&name);
    let need = || job.with_context(|| format!("no job named {name}"));
    match cmd {
        Cmd::Add { prompt, text, task, readonly, account, model, effort, budget, .. } => {
            if !valid_name(&name) {
                bail!("job names are lowercase letters, digits and dashes");
            }
            if job.is_some() {
                bail!("job {name} exists; pick another name, or: relay land clean {name}");
            }
            let brief = match (prompt, text) {
                (Some(p), _) => std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
                (None, Some(t)) => t,
                (None, None) => unreachable!("clap requires one"),
            };
            if brief.trim().is_empty() {
                bail!("the brief is empty");
            }
            if let Some(a) = &account
                && !crate::accounts::discover(&ctx.cfg).iter().any(|x| &x.label == a)
            {
                bail!("unknown account {a}; see: relay quota show");
            }
            let model = model.or_else(|| ctx.cfg.session.model.clone());
            if let Some(reason) = model.as_deref().and_then(|m| ctx.cfg.session.banned(m)) {
                bail!("model {} is banned: {reason}", model.as_deref().unwrap_or_default());
            }
            // The brief goes to the session on stdin, never in argv (/proc/<pid>/cmdline is public).
            std::io::Write::write_all(&mut Ctx::append_private(&ctx.job_file(&name, "prompt.md"))?, brief.as_bytes())?;
            let task =
                task.unwrap_or_else(|| brief.trim().lines().next().unwrap_or_default().chars().take(80).collect());
            port.emit(QueueEvent::Queued {
                job: name.clone(),
                task,
                readonly,
                model,
                effort: effort.or_else(|| ctx.cfg.session.effort.clone()),
                budget_usd: budget.unwrap_or(ctx.cfg.limits.budget_usd),
                pinned: account,
                session_id: uuid::Uuid::new_v4().to_string(),
            })?;
        }
        Cmd::Resume { message, budget, .. } => {
            let j = need()?;
            if j.status.is_live() {
                bail!("{name} is {}", j.status.as_str());
            }
            port.emit(QueueEvent::Resumed { job: name.clone(), message, budget_usd: budget })?;
        }
        Cmd::Dequeue { .. } => {
            let j = need()?;
            if j.status != Status::Queued {
                bail!("{name} is {}", j.status.as_str());
            }
            port.emit(QueueEvent::Dequeued { job: name.clone() })?;
        }
        Cmd::Stop { .. } => {
            let j = need()?;
            if j.status != Status::Running {
                bail!("{name} is {}", j.status.as_str());
            }
            port.emit(QueueEvent::StopRequested { job: name.clone() })?;
            // Only a process that is still the recorded runner is signalled, never a reused pid.
            if let Some((pid, started)) = j.runner
                && os::same_process(pid, started)
            {
                os::terminate_group(pid)?;
            }
        }
    }
    ctx.wake_dispatch()?;
    out!("{name}: recorded");
    Ok(())
}
