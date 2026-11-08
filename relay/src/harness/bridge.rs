//! The bridge's safety contract, on top of any [`Harness`]:
//!
//! - [`Permissions::ReadOnly`]: a removed capability where the harness has
//!   one (`claude`), approval-gated where it does not (`omp`);
//! - a resume always forks, and a harness that cannot fork is refused;
//! - a new session gets an id up front where the harness takes one;
//! - `--continue` is never built (the CLI refuses it at parse time too);
//! - the prompt never goes in argv.
//!
//! The call runs in the caller's own environment and account: the
//! harness's home-selecting variables are not set.

use std::path::Path;

use crate::harness::{Harness, Home, Invocation, Permissions, Refusal, Session, Task};

#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub harness: &'static dyn Harness,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    /// Fork this session. `None` starts `new_session_id`.
    pub resume: Option<&'a str>,
    pub new_session_id: &'a str,
    /// Session display name, such as `relay-ask-claude-ask-<stamp>`.
    pub name: &'a str,
    /// Where a prompt file may be written: a [`crate::hostdir::HostDir`] made new for the call.
    pub scratch: &'a Path,
}

pub fn session_name(harness: &dyn Harness, mode: &str, stamp: &str) -> String {
    let short = harness.id().split('-').next().unwrap_or(harness.id());
    format!("relay-ask-{short}-{mode}-{stamp}")
}

impl Call<'_> {
    pub fn build(&self) -> Result<Invocation, Refusal> {
        let session = self.resume.map_or(Session::New(self.new_session_id), Session::Fork);
        let task = Task {
            prompt: self.prompt,
            model: self.model,
            effort: None,
            session,
            permissions: Permissions::ReadOnly,
            briefing: None,
            budget_usd: None,
            name: Some(self.name),
            events: false,
            lean: false,
            scratch: self.scratch,
            inbox: self.scratch,
        };
        let ambient = Home { harness: self.harness.id(), label: "ambient".into(), dir: self.scratch.to_path_buf() };
        let mut inv = self.harness.invocation(&ambient, &task)?;
        inv.env.clear();
        Ok(inv)
    }
}

/// How the prompt reaches the CLI, for the dry run.
pub fn delivery(inv: &Invocation) -> &'static str {
    if inv.stdin.is_some() { "stdin" } else { "file" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::argv;
    use crate::harness::{ALL, by_id};

    fn call(id: &str, resume: Option<&'static str>) -> Result<Invocation, Refusal> {
        Call {
            harness: by_id(id).unwrap(),
            prompt: "hi",
            model: None,
            resume,
            new_session_id: "new-id",
            name: "n",
            scratch: Path::new("/s"),
        }
        .build()
    }

    #[test]
    fn read_only_and_fork_on_resume() {
        let a = argv(&call("claude", Some("old")).unwrap());
        assert!(a.starts_with("-p --permission-mode plan"), "{a}");
        assert!(a.contains("--resume old --fork-session") && !a.contains("--session-id"));
        let fresh = argv(&call("claude", None).unwrap());
        assert!(fresh.contains("--session-id new-id") && !fresh.contains("--resume"));
        let omp = argv(&call("omp", None).unwrap());
        assert!(omp.starts_with("-p --no-title --approval-mode always-ask"), "{omp}");
        assert!(call("omp", Some("old")).unwrap_err().0.contains("no --fork-session"));
    }

    #[test]
    fn runs_in_the_callers_environment_and_never_takes_the_prompt_in_argv() {
        for h in ALL {
            let inv = call(h.id(), None).unwrap();
            assert!(inv.env.is_empty(), "{}", h.id());
            assert!(!inv.args.iter().any(|a| a == "hi"), "{}", h.id());
            assert!(!inv.args.iter().any(|a| a == "--continue" || a == "-c"));
        }
    }
}
