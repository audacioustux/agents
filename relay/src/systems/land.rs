//! land: brings a finished job's branch back and clears up after it.
//! Emits: forgotten. It never checks out, merges or pushes: putting work on
//! the main branch stays with whoever runs the project's gate.

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use crate::bus::LandEvent;
use crate::ctx::Ctx;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Copy the job's branch into the repository as a branch of the same name. No checkout.
    Fetch { name: String },
    /// Delete a finished job's clone and records.
    Clean { name: String },
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    let world = ctx.world()?;
    match cmd {
        Cmd::Fetch { name } => {
            let j = world.jobs.get(&name).with_context(|| format!("no job named {name}"))?;
            let (Some(dir), Some(branch)) = (&j.workdir, &j.branch) else {
                bail!("{name} has no branch (a read-only job?)")
            };
            let st = Ctx::git(&ctx.cfg.repo)
                .args(["fetch", "--quiet"])
                .arg(dir)
                .arg(format!("{branch}:refs/heads/{branch}"))
                .status()?;
            if !st.success() {
                bail!("git fetch failed");
            }
            Ctx::git(&ctx.cfg.repo).args(["log", "--oneline", &format!("HEAD..{branch}")]).status()?;
        }
        Cmd::Clean { name } => {
            let j = world.jobs.get(&name).with_context(|| format!("no job named {name}"))?;
            if j.status.is_live() {
                bail!("{name} is {}; stop it first", j.status.as_str());
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
            ctx.port::<LandEvent>().emit(LandEvent::Forgotten { job: name.clone() })?;
            out!("{name}: cleaned");
        }
    }
    Ok(())
}
