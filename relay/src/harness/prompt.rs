//! The bridge's prompts. The mode shapes only the prompt; the contract is
//! the same in every mode.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ask,
    Plan,
    Adversarial,
    Review,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::Ask, Self::Plan, Self::Adversarial, Self::Review];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Plan => "plan",
            Self::Adversarial => "adversarial",
            Self::Review => "review",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|m| m.as_str() == s).ok_or_else(|| {
            let names: Vec<_> = Self::ALL.iter().map(|m| m.as_str()).collect();
            format!("unknown mode: {s}. Supported: {}", names.join(", "))
        })
    }
}

pub const SUBJECT_LIMIT: usize = 20_000;
pub const DIFF_LIMIT: usize = 1_000_000;

const RULES: &str = "Use any resumed session history only as optional background; ignore it if unrelated.\n\
Do not edit files. Do not implement fixes. Do not run destructive commands.\n\
Be direct, skeptical, and specific. Prefer concrete risks and actionable changes over generic advice.";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Subject {
    pub text: String,
    pub path: Option<PathBuf>,
    pub truncated: bool,
}

/// If `subject` names a file (relative to `cwd`), its first [`SUBJECT_LIMIT`] bytes.
pub fn read_subject(subject: &str, cwd: &Path) -> Subject {
    let target = cwd.join(subject);
    if subject.is_empty() || !target.is_file() {
        return Subject::default();
    }
    let mut buf = Vec::with_capacity(SUBJECT_LIMIT);
    let read = std::fs::File::open(&target).and_then(|f| f.take(SUBJECT_LIMIT as u64).read_to_end(&mut buf));
    if read.is_err() {
        return Subject::default();
    }
    Subject {
        truncated: buf.len() == SUBJECT_LIMIT,
        text: String::from_utf8_lossy(&buf).into_owned(),
        path: Some(target),
    }
}

#[derive(Debug)]
pub struct Prompt<'a> {
    pub mode: Mode,
    pub subject: &'a str,
    pub subject_text: &'a str,
    pub extra: &'a str,
    pub base: &'a str,
    pub head: &'a str,
    pub diff: &'a str,
    pub identity: &'a str,
}

impl Prompt<'_> {
    pub fn render(&self) -> String {
        // A fence inside the subject must not close ours.
        let safe = self.subject_text.replace("```", "`\u{200b}``");
        let target = if self.subject_text.is_empty() {
            self.subject.to_owned()
        } else {
            format!("{}\n\nTarget file contents (bounded):\n```text\n{safe}\n```", self.subject)
        };
        let suffix =
            if self.extra.is_empty() { String::new() } else { format!("\n\nAdditional instructions:\n{}", self.extra) };
        let head = format!(
            "{}. Treat this as a one-shot task; do not assume ongoing context beyond the prompt below.\n\n{RULES}\n\n",
            self.identity
        );
        match self.mode {
            Mode::Ask => format!("{head}Question or task:\n{target}{suffix}"),
            Mode::Plan => format!(
                "{head}Review this plan for correctness, missing steps, unclear assumptions, sequencing risk, test coverage, and overengineering:\n{target}{suffix}"
            ),
            Mode::Adversarial => format!(
                "{head}Perform an adversarial review of this target. Attack assumptions, hidden coupling, failure modes, security/operability risks, and weak tests. Return the strongest objections first, then suggested changes:\n{target}{suffix}"
            ),
            Mode::Review => format!(
                "{head}Perform a read-only adversarial code review for {}..{}. Return findings by severity with evidence and a merge verdict.\n\nDiff:\n{}{suffix}",
                self.base, self.head, self.diff
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p<'a>(mode: Mode, subject: &'a str, text: &'a str) -> Prompt<'a> {
        Prompt {
            mode,
            subject,
            subject_text: text,
            extra: "be brief",
            base: "a",
            head: "b",
            diff: "D",
            identity: "You are Claude",
        }
    }

    #[test]
    fn modes_embed_the_subject_and_their_instruction() {
        let ask = p(Mode::Ask, "why?", "").render();
        assert!(ask.starts_with("You are Claude. Treat this as a one-shot task"));
        assert!(ask.contains("Question or task:\nwhy?\n\nAdditional instructions:\nbe brief"));
        assert!(p(Mode::Plan, "x", "").render().contains("Review this plan for correctness"));
        assert!(p(Mode::Adversarial, "x", "").render().contains("Attack assumptions"));
        assert!(p(Mode::Review, "", "").render().contains("code review for a..b. Return findings"));
    }

    #[test]
    fn a_fence_in_the_subject_cannot_escape() {
        let r = p(Mode::Ask, "f.md", "before\n```\nafter").render();
        assert_eq!(r.matches("```").count(), 2, "{r}");
    }

    #[test]
    fn subject_files_are_bounded() {
        let d = std::env::temp_dir().join(format!("hb-subject-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("big"), "x".repeat(SUBJECT_LIMIT + 5)).unwrap();
        let s = read_subject("big", &d);
        assert!(s.truncated && s.text.len() == SUBJECT_LIMIT);
        assert_eq!(read_subject("missing", &d), Subject::default());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
