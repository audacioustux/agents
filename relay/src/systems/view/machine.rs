//! Every project on this machine, for the boards: the one relay runs in loaded
//! as usual, the others peeked at. Their checkpoints are theirs: read, never written.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::config::Config;
use crate::ctx::Ctx;
use crate::systems::quota::{AccountsView, accounts_among};
use crate::world::World;

/// Where a board reads: the project relay runs in, if any, among every project on this machine.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    /// The project's config, or the owner's policy alone outside any project.
    pub cfg: &'a Config,
    pub home: Option<&'a Ctx>,
    /// `state_root`: one directory per project.
    pub root: &'a Path,
}

impl Scope<'_> {
    pub fn read(&self) -> Result<Machine> {
        Machine::read(self.home, self.root)
    }
}

/// One project's fold, as this machine's state holds it.
#[derive(Debug)]
pub struct Project {
    pub name: String,
    /// Its jobs directory: briefs, logs and answers.
    pub jobs: PathBuf,
    pub world: World,
    /// The project relay runs in: its config applies to it.
    pub own: bool,
}

/// Every project's fold, read in one go.
#[derive(Debug, Default)]
pub struct Machine {
    /// The project relay runs in first, then the others by name.
    pub projects: Vec<Project>,
    /// Each project whose state could not be read, with why.
    pub unreadable: Vec<String>,
}

impl Machine {
    /// Ours (`home`), whose failure to load is the read's, then every other project under `root`,
    /// one that cannot be read named in `unreadable`.
    pub fn read(home: Option<&Ctx>, root: &Path) -> Result<Self> {
        let mut m = Self::default();
        if let Some(ctx) = home {
            let world = ctx.world()?;
            m.projects.push(Project { name: ctx.cfg.name.clone(), jobs: ctx.jobs.clone(), world, own: true });
        }
        // A state root that cannot be listed fails the read, so a board keeps its last good one.
        let mut dirs = crate::ctx::try_project_dirs(root).with_context(|| format!("listing {}", root.display()))?;
        dirs.retain(|d| home.is_none_or(|c| *d != c.state));
        dirs.sort();
        for d in dirs {
            let name = d.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            match World::peek(&d.join("bus"), &d.join("world.json")) {
                Ok(world) => m.projects.push(Project { name, jobs: d.join("jobs"), world, own: false }),
                Err(e) => m.unreadable.push(format!("{name}: {e:#}")),
            }
        }
        Ok(m)
    }

    /// The projects a board lists: ours alone, or `everywhere` all of them (all of them, too, outside any project).
    pub fn shown(&self, everywhere: bool) -> impl Iterator<Item = &Project> {
        let any_own = self.projects.iter().any(|p| p.own);
        self.projects.iter().filter(move |p| everywhere || p.own || !any_own)
    }

    /// Every account's room, as all the projects know it, for every model their jobs ask for.
    pub fn accounts(&self, cfg: &Config) -> AccountsView {
        let empty = World::default();
        let own = self.projects.iter().find(|p| p.own).map_or(&empty, |p| &p.world);
        accounts_among(cfg, own, self.projects.iter().filter(|p| !p.own).map(|p| &p.world))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_root_that_cannot_be_listed_fails_the_read_and_a_missing_one_is_empty() {
        let d = crate::config::tempdir::Dir::new();
        let missing = Machine::read(None, &d.path().join("none")).unwrap();
        assert!(missing.projects.is_empty() && missing.unreadable.is_empty());
        let file = d.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let err = Machine::read(None, &file).unwrap_err();
        assert!(format!("{err:#}").contains("listing"), "{err:#}");
    }
}
