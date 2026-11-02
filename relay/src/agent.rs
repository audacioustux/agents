//! How a headless Claude Code session is started. Kept apart so it can move
//! into the shared agent-CLI library (harness-bridge) unchanged.

use std::ffi::OsString;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start<'a> {
    Fresh(&'a str),
    Resume(&'a str),
}

/// What the session may do. `Unrestricted` skips the CLI's permission checks,
/// so it exists only for sessions inside the sandbox, where the mounts are the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permissions {
    Unrestricted,
    ReadOnly,
    EditsOnly,
}

#[derive(Debug)]
pub struct Headless<'a> {
    pub command: &'a [String],
    pub start: Start<'a>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub budget_usd: f64,
    pub briefing: &'a str,
    pub permissions: Permissions,
    pub extra_args: &'a [String],
}

impl Headless<'_> {
    /// The argv. The prompt is not in it: it goes on stdin.
    pub fn argv(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = self.command.iter().map(OsString::from).collect();
        a.extend(
            [
                "-p",
                // stream-json keeps every result (a later wake-up cannot overwrite
                // the report) and needs --verbose in -p mode.
                "--output-format",
                "stream-json",
                "--verbose",
                // No MCP servers, user settings, plugins or skill listings: each
                // costs context on every turn, and a queued task does not use them.
                "--strict-mcp-config",
                "--setting-sources",
                "project,local",
                "--disable-slash-commands",
            ]
            .map(OsString::from),
        );
        match self.permissions {
            Permissions::Unrestricted => a.push("--dangerously-skip-permissions".into()),
            Permissions::ReadOnly => a.extend(["--permission-mode".into(), "plan".into()]),
            Permissions::EditsOnly => a.extend(["--permission-mode".into(), "acceptEdits".into()]),
        }
        a.extend(["--max-budget-usd".into(), format!("{}", self.budget_usd).into()]);
        a.extend(["--append-system-prompt".into(), self.briefing.into()]);
        // Model and effort go on resumes too, or a resume runs on the account's default model.
        if let Some(m) = self.model {
            a.extend(["--model".into(), m.into()]);
        }
        if let Some(e) = self.effort {
            a.extend(["--effort".into(), e.into()]);
        }
        // The id is chosen up front, so even a session killed before its first
        // output can be resumed. --continue is never used: it picks the latest session globally.
        match self.start {
            Start::Fresh(id) => a.extend(["--session-id".into(), id.into()]),
            Start::Resume(id) => a.extend(["--resume".into(), id.into()]),
        }
        a.extend(self.extra_args.iter().map(OsString::from));
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_keeps_model_and_never_continues() {
        let cmd = ["claude".to_owned()];
        let h = Headless {
            command: &cmd,
            start: Start::Resume("abc"),
            model: Some("sonnet"),
            effort: Some("high"),
            budget_usd: 2.5,
            briefing: "b",
            permissions: Permissions::ReadOnly,
            extra_args: &[],
        };
        let argv: Vec<String> = h.argv().into_iter().map(|s| s.into_string().unwrap()).collect();
        let joined = argv.join(" ");
        assert!(joined.contains("--resume abc"));
        assert!(joined.contains("--model sonnet"));
        assert!(joined.contains("--effort high"));
        assert!(joined.contains("--permission-mode plan"));
        assert!(joined.contains("--max-budget-usd 2.5"));
        assert!(!argv.iter().any(|a| a == "--continue" || a == "-c" || a == "--dangerously-skip-permissions"));
    }
}
