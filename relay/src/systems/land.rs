//! land: brings a finished job's branch back and clears up after it.
//! Emits: forgotten. It never checks out, merges or pushes: putting work on
//! the main branch stays with whoever runs the project's gate.

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::bus::LandEvent;
use crate::ctx::Ctx;

/// The capability to write land's port; only this module can make one.
#[derive(Debug)]
pub struct Source(());

impl crate::bus::Source for Source {
    fn file(&self) -> std::path::PathBuf {
        crate::bus::system_file("land")
    }
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Copy the job's branch into the repository as a branch of the same name. No checkout.
    Fetch { name: String },
    /// Delete a finished job's clone and records.
    Clean { name: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    // A clean must not race a resume or a dispatch of the same job.
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    match cmd {
        Cmd::Fetch { name } => {
            let Some(j) = world.jobs.get(&name) else { fail!(NotFound, "no job named {name}") };
            let (Some(dir), Some(branch)) = (&j.workdir, &j.branch) else {
                fail!(Conflict, "{name} has no branch (a read-only job?)")
            };
            let st = Ctx::git(&ctx.cfg.repo)
                .args(["fetch", "--quiet"])
                .arg(dir)
                .arg(format!("{branch}:refs/heads/{branch}"))
                .status()?;
            if !st.success() {
                bail!("git fetch failed");
            }
            let log = Ctx::git(&ctx.cfg.repo).args(["log", "--oneline", &format!("HEAD..{branch}")]).output()?;
            let log = String::from_utf8_lossy(&log.stdout);
            let commits: Vec<&str> = log.lines().collect();
            ctx.reply(&serde_json::json!({ "job": name, "branch": branch, "commits": commits }), &log)?;
        }
        Cmd::Clean { name } => {
            let Some(j) = world.jobs.get(&name) else { fail!(NotFound, "no job named {name}") };
            if j.status.is_live() {
                fail!(Conflict, "{name} is {}; stop it first", j.status.as_str());
            }
            if let Some(dir) = j.workdir.as_ref().filter(|d| d.exists()) {
                // Only ever a directory relay made.
                if !dir.starts_with(&ctx.clones) || dir == &ctx.clones {
                    bail!("{} is outside relay's clones; refusing", dir.display());
                }
                std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
            }
            for ext in ["prompt.md", "log.ndjson", "stderr.log", "result.md", "runner.log"] {
                let _ = std::fs::remove_file(ctx.job_file(&name, ext));
            }
            let _ = std::fs::remove_dir_all(ctx.job_scratch(&name));
            ctx.port(&Source(())).emit(LandEvent::Forgotten { job: name.clone() })?;
            ctx.reply(&serde_json::json!({ "job": name, "cleaned": true }), &format!("{name}: cleaned"))?;
        }
    }
    Ok(())
}
