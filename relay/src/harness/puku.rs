//! puku-cli, a CLI with Claude Code's argv shape. Not installed where this
//! adapter was written, so it is unverified: its single home, its output
//! (`--output-format json`, one result object) and its session store come
//! from how lanes ran it. Runners should use it only when told to.

use std::path::{Path, PathBuf};

use crate::harness::claude::{StreamReader, claude_like_args};
use crate::harness::{Caps, Harness, Home, Invocation, Meter, Meters, Reader, Refusal, Task};

const STATE: &str = ".puku-cli";

#[derive(Debug)]
pub struct PukuCli;

impl Harness for PukuCli {
    fn id(&self) -> &'static str {
        "puku-cli"
    }

    fn caps(&self) -> Caps {
        Caps {
            read_only_by_capability: true,
            fork: true,
            preset_session_id: true,
            budget: true,
            quota: false,
            verified: false,
        }
    }

    fn identity(&self) -> &'static str {
        "You are Puku"
    }

    /// One home, the user's own sign-in, when the CLI is installed.
    fn homes(&self, user_home: &Path) -> Vec<Home> {
        if !crate::harness::installed(self) {
            return Vec::new();
        }
        vec![Home { harness: "puku-cli", label: "default".into(), dir: user_home.join(STATE) }]
    }

    fn home_paths(&self, user_home: &Path) -> Vec<PathBuf> {
        vec![user_home.join(STATE)]
    }

    fn writable(&self, home: &Home, _user_home: &Path) -> Vec<PathBuf> {
        vec![home.dir.clone()]
    }

    fn invocation(&self, _home: &Home, task: &Task<'_>) -> Result<Invocation, Refusal> {
        let mut i = Invocation::new("puku-cli");
        claude_like_args(&mut i, task, &["json"], "--dangerously-skip-permissions");
        Ok(i)
    }

    fn reader(&self) -> Box<dyn Reader> {
        Box::new(StreamReader::default())
    }

    /// No quota command is known: the home is unmetered, and a limit shows
    /// up only as a failed session.
    fn meters_for<'m>(&self, _model: Option<&str>, _meters: &'m Meters) -> Result<Vec<&'m Meter>, Refusal> {
        Ok(Vec::new())
    }

    /// One home, so a session never moves; it resumes where it is.
    fn carry(&self, _session: &str, from: Option<&Home>, to: &Home, _scratch: &Path) -> bool {
        from.is_some_and(|f| f == to)
    }
}
