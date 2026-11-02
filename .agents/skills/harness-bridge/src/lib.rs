//! What this repository knows about sibling agent CLIs, in one place.
//!
//! - [`bridge`]: a one-shot call under a read-only safety contract. This is
//!   what the `harness-bridge` binary runs.
//! - [`headless`]: an unattended Claude Code session for a runner that
//!   supplies its own boundary (relay runs these inside bubblewrap).
//!
//! Every table here is a claim about software this repository does not
//! control. Check it against the installed CLI (`--dry-run`, and the tests
//! that run the live binary) when adding an entry and after every upgrade.

pub mod bridge;
pub mod headless;
pub mod prompt;

use std::fmt;
use std::str::FromStr;

/// A sibling CLI the bridge can call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    PukuCli,
    Omp,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::Claude, Self::PukuCli, Self::Omp];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::PukuCli => "puku-cli",
            Self::Omp => "omp",
        }
    }

    pub fn bin(self) -> &'static str {
        self.name()
    }

    /// How the prompt addresses the model.
    pub fn identity(self) -> &'static str {
        match self {
            Self::Claude => "You are Claude",
            Self::PukuCli => "You are Puku",
            Self::Omp => "You are OMP",
        }
    }

    /// Whether the CLI accepts `--input-format stream-json --replay-user-messages`
    /// with one user-message envelope on stdin. Without it, a prompt too large
    /// for argv is refused rather than sent and failed by the kernel.
    pub fn takes_stdin(self) -> bool {
        !matches!(self, Self::Omp)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::PukuCli => "puku",
            Self::Omp => "omp",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Agent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<_> = Self::ALL.iter().map(|a| a.name()).collect();
            format!("unknown agent: {s}. Supported: {}", names.join(", "))
        })
    }
}

/// Prompts above this many bytes go on stdin: argv risks `E2BIG`.
pub const PROMPT_ARGV_LIMIT_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Argv,
    Stdin,
}

impl Delivery {
    /// Measured in bytes: emoji and CJK take 2-4 bytes each on the wire.
    pub fn for_prompt(prompt: &str) -> Self {
        if prompt.len() > PROMPT_ARGV_LIMIT_BYTES { Self::Stdin } else { Self::Argv }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Argv => "argv",
            Self::Stdin => "stdin",
        }
    }
}

/// The one user message `--input-format stream-json` reads from stdin.
pub fn stdin_envelope(prompt: &str) -> String {
    let msg = serde_json::json!({ "type": "user", "message": { "role": "user", "content": prompt } });
    format!("{msg}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_counts_bytes_not_chars() {
        let emoji = "😀".repeat(40_000); // 40 K chars, 160 KB
        assert_eq!(Delivery::for_prompt(&emoji), Delivery::Stdin);
        assert_eq!(Delivery::for_prompt("small"), Delivery::Argv);
    }

    #[test]
    fn envelope_escapes_json() {
        let e = stdin_envelope("a \"quote\"\nnewline \\ back");
        let v: serde_json::Value = serde_json::from_str(e.trim_end()).unwrap();
        assert_eq!(v["message"]["content"], "a \"quote\"\nnewline \\ back");
        assert!(e.ends_with('\n'));
    }

    #[test]
    fn agents_parse() {
        assert_eq!("puku-cli".parse::<Agent>(), Ok(Agent::PukuCli));
        assert!("codex".parse::<Agent>().unwrap_err().contains("Supported: claude, puku-cli, omp"));
    }
}
