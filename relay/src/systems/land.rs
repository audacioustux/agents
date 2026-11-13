//! land: brings a finished job's branch back and clears up after it.
//! Emits: forgotten, collected. It never checks out, merges or pushes: putting work on
//! the main branch stays with whoever runs the project's gate.

mod gc;

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
    /// Remove what finished jobs no longer need: build outputs, old clones, old logs ([gc] in relay.toml).
    Gc {
        /// Say what would be removed; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// A note when `job` has been put up for review but its tip has no accepting verdict. Never a block:
/// landing is the operator's step. A job nobody reviews, and nobody has been asked to, gets none.
fn unreviewed(ctx: &Ctx, world: &crate::world::World, job: &crate::world::Job, branch: &str) -> Option<String> {
    let asked = !job.verdicts.is_empty()
        || world.jobs().values().any(|j| j.review_of() == Some(job.name.as_str()) && j.unchained.is_none());
    if !asked {
        return None;
    }
    let tip = Ctx::git(&ctx.cfg.repo).args(["rev-parse", "--verify", &format!("refs/heads/{branch}")]).output().ok()?;
    let tip = String::from_utf8_lossy(&tip.stdout).trim().to_owned();
    let accepted = job.verdicts.iter().any(|v| v.verdict == crate::bus::Verdict::Accept && v.commit == tip);
    (!accepted).then(|| {
        let short = tip.get(..12).unwrap_or(&tip);
        match job.verdicts.last() {
            Some(v) => format!(
                "no review has accepted {short}; the latest, by {} at {}, was {}",
                v.reviewer,
                v.commit.get(..12).unwrap_or(&v.commit),
                v.summary()
            ),
            None => format!("no review has accepted {short} yet"),
        }
    })
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    if let Cmd::Gc { dry_run } = cmd {
        return gc::run(ctx, dry_run);
    }
    // A clean must not race a resume or a dispatch of the same job.
    let _lock = ctx.jobs_lock()?;
    let world = ctx.world()?;
    match cmd {
        Cmd::Fetch { name } => {
            let Some(j) = world.jobs().get(&name) else { fail!(NotFound, "no job named {name}") };
            let (Some(dir), Some(branch)) = (&j.workdir, &j.branch) else {
                if j.branch.is_some() {
                    fail!(
                        Conflict,
                        "{name}'s clone was removed by gc; its commits are in the repository as refs/relay/rescue/{name} \
                         unless they were fetched before"
                    )
                }
                fail!(Conflict, "{name} has no branch (a read-only job?)")
            };
            let st = Ctx::git(&ctx.cfg.repo)
                .args(["fetch", "--quiet"])
                .arg(dir.path())
                .arg(format!("{branch}:refs/heads/{branch}"))
                .status()?;
            if !st.success() {
                bail!("git fetch failed");
            }
            let log = Ctx::git(&ctx.cfg.repo).args(["log", "--oneline", &format!("HEAD..{branch}")]).output()?;
            let log = String::from_utf8_lossy(&log.stdout);
            let commits: Vec<&str> = log.lines().collect();
            let note = unreviewed(ctx, &world, j, branch);
            let text = match &note {
                Some(n) => format!("{}\nnote: {n}", log.trim_end()),
                None => log.to_string(),
            };
            ctx.reply(&serde_json::json!({ "job": name, "branch": branch, "commits": commits, "note": note }), &text)?;
        }
        Cmd::Gc { .. } => unreachable!("handled above"),
        Cmd::Clean { name } => {
            let Some(j) = world.jobs().get(&name) else { fail!(NotFound, "no job named {name}") };
            if j.status.is_live() {
                fail!(Conflict, "{name} is {}; stop it first", j.status.as_str());
            }
            // What waits on it, through any edge of the dependency graph, would lose what it waits on.
            let waiters = world.waiters(&name);
            if !waiters.is_empty() {
                fail!(
                    Conflict,
                    "{} wait on {name} (relay view status says how): cleaning it would release them onto work no \
                     review accepted, or block them for good; clean {name} once they have started, or release \
                     them first (relay queue release NAME), or detach its review (relay queue unreview)",
                    waiters.join(", ")
                );
            }
            if let Some(dir) = j.workdir.as_ref().map(crate::ctx::SessionRepo::path).filter(|d| d.exists()) {
                // Only ever a directory relay made.
                if !dir.starts_with(&ctx.clones) || dir == ctx.clones {
                    bail!("{} is outside relay's clones; refusing", dir.display());
                }
                crate::hostdir::remove_tree(dir).with_context(|| format!("removing {}", dir.display()))?;
            }
            ctx.wipe_job_files(&name)?;
            // The ref a review read its branch by is the review's own, and goes with it.
            let _ = Ctx::git(&ctx.cfg.repo).args(["update-ref", "-d", &format!("refs/relay/review/{name}")]).output();
            ctx.port(&Source(())).emit(LandEvent::Forgotten { job: name.clone() })?;
            ctx.reply(&serde_json::json!({ "job": name, "cleaned": true }), &format!("{name}: cleaned"))?;
        }
    }
    Ok(())
}
