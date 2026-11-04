//! What a command tells its caller. With `--json`, one JSON value on stdout
//! and failures as JSON on stderr, for an agent to act on; otherwise text.
//! Either way a failure's exit code says what kind it was:
//!
//! | code | kind | meaning |
//! | --- | --- | --- |
//! | 1 | `error` | anything else: an I/O failure, a broken install |
//! | 2 | `usage` | the command line itself is wrong |
//! | 3 | `not-found` | no such job or account |
//! | 4 | `conflict` | the job is in the wrong state, or the name is taken |
//! | 5 | `refused` | a rule says no: a banned model, a disabled harness, a contract |

use std::fmt;
use std::io::Write as _;
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usage,
    NotFound,
    Conflict,
    Refused,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Conflict => 4,
            Self::Refused => 5,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::NotFound => "not-found",
            Self::Conflict => "conflict",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug)]
pub struct Fail {
    pub kind: Kind,
    pub msg: String,
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Fail {}

/// Returns a [`Fail`] of the given kind: `fail!(NotFound, "no job named {name}")`.
macro_rules! fail {
    ($kind:ident, $($arg:tt)*) => {
        return Err(crate::reply::Fail { kind: crate::reply::Kind::$kind, msg: format!($($arg)*) }.into())
    };
}

/// Prints `value` with `--json`, else `text`.
pub fn reply(json: bool, value: &serde_json::Value, text: &str) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        writeln!(out, "{value}")?;
    } else if !text.is_empty() {
        writeln!(out, "{}", text.trim_end())?;
    }
    Ok(())
}

/// The exit code for a command's result, after saying what went wrong.
pub fn finish(result: anyhow::Result<u8>, json: bool, prefix: &str) -> ExitCode {
    let e = match result {
        Ok(code) => return ExitCode::from(code),
        Err(e) => e,
    };
    // `relay ... | head` closing the pipe early is not a failure.
    if e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe) {
        return ExitCode::SUCCESS;
    }
    let kind = e.downcast_ref::<Fail>().map(|f| f.kind);
    if json {
        let body = serde_json::json!({ "error": format!("{e:#}"), "kind": kind.map_or("error", Kind::name) });
        eprintln!("{body}");
    } else {
        eprintln!("{prefix}: {e:#}");
    }
    ExitCode::from(kind.map_or(1, Kind::code))
}
