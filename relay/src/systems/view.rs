//! view: read-only. Who is working on what, and what each session said.

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::bus::now_micros;
use crate::ctx::Ctx;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Jobs and accounts.
    Status {
        /// Include jobs that ended more than a day ago.
        #[arg(long)]
        all: bool,
        /// Machine-readable: the folded world as JSON.
        #[arg(long)]
        json: bool,
    },
    /// A job's final answer, branch, spend and log.
    Result { name: String },
    /// The merged event log, oldest first, one JSON object per line.
    Events,
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Status { all, json } => status(ctx, all, json),
        Cmd::Result { name } => result(ctx, &name),
        Cmd::Events => {
            for e in ctx.events()? {
                out!("{}", e.to_json());
            }
            Ok(())
        }
    }
}

pub fn ago(now_us: u64, then_us: u64) -> String {
    let s = now_us.saturating_sub(then_us) / 1_000_000;
    match s {
        0..120 => format!("{s}s"),
        120..7200 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

fn status(ctx: &Ctx, all: bool, json: bool) -> Result<()> {
    let world = ctx.world()?;
    if json {
        out!("{}", serde_json::to_string_pretty(&world)?);
        return Ok(());
    }
    let now = now_micros();
    let day_ago = now.saturating_sub(24 * 3600 * 1_000_000);
    let mut jobs: Vec<_> =
        world.jobs.values().filter(|j| all || j.status.is_live() || j.ended_at.is_some_and(|t| t > day_ago)).collect();
    jobs.sort_by_key(|j| j.created_at);
    out!("jobs (at most {} running)", ctx.cfg.limits.max_running);
    for j in &jobs {
        let since = match j.status {
            crate::world::Status::Running => j.started_at,
            crate::world::Status::Queued => j.ended_at.or(Some(j.created_at)),
            _ => j.ended_at,
        };
        let cost = if j.cost_usd > 0.0 { format!(" ${:.2}", j.cost_usd) } else { String::new() };
        let reason = j.reason.as_deref().map_or(String::new(), |r| format!("  [{r}]"));
        out!(
            "  {:<24} {:<8} {:<20} {:>4}{cost}  {}{reason}",
            j.name,
            j.status.as_str(),
            j.account.as_deref().or(j.pinned.as_deref()).unwrap_or("-"),
            since.map_or_else(|| "-".into(), |t| ago(now, t)),
            j.task
        );
    }
    if jobs.is_empty() {
        out!("  (none)");
    }
    crate::systems::quota::show(ctx, &world)
}

fn result(ctx: &Ctx, name: &str) -> Result<()> {
    let world = ctx.world()?;
    let j = world.jobs.get(name).with_context(|| format!("no job named {name}"))?;
    out!(
        "{}: {}{} on {}, ${:.2}, {} turns, {} attempt(s)",
        j.name,
        j.status.as_str(),
        j.reason.as_deref().map_or(String::new(), |r| format!(" ({r})")),
        j.account.as_deref().unwrap_or("-"),
        j.cost_usd,
        j.turns,
        j.attempts
    );
    if let (Some(b), Some(w)) = (&j.branch, &j.workdir) {
        out!("branch {b} in {}   bring it here: relay land fetch {name}", w.display());
    }
    out!("log {}\n", ctx.job_file(name, "log.ndjson").display());
    let text = std::fs::read_to_string(ctx.job_file(name, "result.md"));
    out!("{}", text.as_deref().map_or("(no result yet)", str::trim_end));
    Ok(())
}
