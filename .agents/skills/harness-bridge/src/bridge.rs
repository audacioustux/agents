//! The bridge's safety contract, enforced in the argv it builds:
//!
//! - read-only by capability (`--permission-mode plan`) for `claude` and
//!   `puku-cli`; approval-gated (`--approval-mode always-ask`) for `omp`,
//!   which has no read-only mode;
//! - a resume always forks (`--fork-session`); `omp` cannot fork, so its
//!   resume is refused;
//! - a new session gets an id chosen up front;
//! - `--continue` is never built (callers are refused it at parse time).

use crate::{Agent, Delivery, PROMPT_ARGV_LIMIT_BYTES, stdin_envelope};

#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub agent: Agent,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    /// Resume this session (forked). `None` starts `new_session_id`.
    pub resume: Option<&'a str>,
    pub new_session_id: &'a str,
    /// Session display name, such as `harness-bridge-claude-ask-<stamp>`.
    pub name: &'a str,
}

/// A command ready to run. With an envelope, it goes on the child's stdin
/// and the prompt is nowhere in `args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub bin: &'static str,
    pub args: Vec<String>,
    pub envelope: Option<String>,
}

/// Control characters stripped, so a hostile value cannot forge log lines or terminal escapes.
fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_control() { ' ' } else { c }).collect()
}

pub fn session_name(agent: Agent, mode: &str, stamp: &str) -> String {
    format!("harness-bridge-{}-{mode}-{stamp}", agent.label())
}

impl Call<'_> {
    pub fn delivery(&self) -> Delivery {
        Delivery::for_prompt(self.prompt)
    }

    /// The argv for this call, or why the agent cannot honour the contract.
    pub fn build(&self) -> Result<Built, String> {
        let delivery = self.delivery();
        if delivery == Delivery::Stdin && !self.agent.takes_stdin() {
            return Err(format!(
                "prompt is {} bytes (over {PROMPT_ARGV_LIMIT_BYTES}); {} does not declare stdin support, \
                 refusing to fall back to argv (which would risk E2BIG)",
                self.prompt.len(),
                self.agent
            ));
        }
        match self.agent {
            Agent::Claude | Agent::PukuCli => Ok(self.plan_mode(delivery)),
            Agent::Omp => self.omp(),
        }
    }

    fn plan_mode(&self, delivery: Delivery) -> Built {
        let mut args: Vec<String> = ["-p", "--permission-mode", "plan"].map(String::from).to_vec();
        if delivery == Delivery::Stdin {
            args.extend(
                [
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--replay-user-messages",
                ]
                .map(String::from),
            );
        }
        if let Some(m) = self.model {
            args.extend(["--model".into(), m.into()]);
        }
        args.extend(["--name".into(), self.name.into()]);
        match self.resume {
            Some(id) => args.extend(["--resume".into(), id.into(), "--fork-session".into()]),
            None => args.extend(["--session-id".into(), self.new_session_id.into()]),
        }
        let envelope = match delivery {
            Delivery::Stdin => Some(stdin_envelope(self.prompt)),
            Delivery::Argv => {
                args.push(self.prompt.into());
                None
            }
        };
        Built { bin: self.agent.bin(), args, envelope }
    }

    /// omp has no `--name`, `--fork-session`, `--session-id` or stream-json input.
    fn omp(&self) -> Result<Built, String> {
        if let Some(id) = self.resume {
            return Err(format!(
                "omp has no --fork-session; resume would extend the prior session. Pass --fresh, or call \
                 `omp --resume {} --no-session` directly (fork-on-resume gap is your responsibility).",
                printable(id)
            ));
        }
        let mut args: Vec<String> = ["-p", "--approval-mode", "always-ask"].map(String::from).to_vec();
        if let Some(m) = self.model {
            args.extend(["--model".into(), m.into()]);
        }
        args.push(self.prompt.into());
        Ok(Built { bin: self.agent.bin(), args, envelope: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(agent: Agent, prompt: &str, resume: Option<&'static str>) -> Built {
        Call { agent, prompt, model: None, resume, new_session_id: "new-id", name: "n" }.build().unwrap()
    }

    #[test]
    fn plan_mode_and_fork_on_resume() {
        for agent in [Agent::Claude, Agent::PukuCli] {
            let b = call(agent, "hi", Some("old"));
            let a = b.args.join(" ");
            assert!(a.starts_with("-p --permission-mode plan"), "{a}");
            assert!(a.contains("--resume old --fork-session"));
            assert!(!a.contains("--session-id"));
            assert!(!b.args.iter().any(|x| x == "--continue" || x == "-c"));
        }
    }

    #[test]
    fn fresh_sessions_get_their_id() {
        let a = call(Agent::Claude, "hi", None).args.join(" ");
        assert!(a.contains("--session-id new-id"));
        assert!(!a.contains("--resume"));
    }

    #[test]
    fn stdin_path_keeps_the_contract_and_keeps_the_prompt_out_of_argv() {
        let big = "x".repeat(PROMPT_ARGV_LIMIT_BYTES + 1);
        let b = call(Agent::Claude, &big, Some("old"));
        assert!(b.envelope.is_some());
        assert!(!b.args.iter().any(|a| a.len() > 1000));
        let a = b.args.join(" ");
        assert!(a.contains("--permission-mode plan") && a.contains("--fork-session"));
        assert!(a.contains("--input-format stream-json") && a.contains("--replay-user-messages"));
        let small = call(Agent::Claude, "hi", None).args.join(" ");
        assert!(!small.contains("stream-json"));
    }

    #[test]
    fn omp_is_approval_gated_and_refuses_what_it_cannot_honour() {
        let b =
            Call { agent: Agent::Omp, prompt: "hi", model: Some("opus"), resume: None, new_session_id: "x", name: "n" }
                .build()
                .unwrap();
        assert_eq!(b.args, ["-p", "--approval-mode", "always-ask", "--model", "opus", "hi"]);
        let resume = Call {
            agent: Agent::Omp,
            prompt: "hi",
            model: None,
            resume: Some("id\x1b[2J"),
            new_session_id: "x",
            name: "n",
        }
        .build()
        .unwrap_err();
        assert!(resume.contains("no --fork-session") && !resume.contains('\x1b'), "{resume}");
        let big = "x".repeat(PROMPT_ARGV_LIMIT_BYTES + 1);
        let err = Call { agent: Agent::Omp, prompt: &big, model: None, resume: None, new_session_id: "x", name: "n" }
            .build()
            .unwrap_err();
        assert!(err.contains("does not declare stdin support"));
    }
}
