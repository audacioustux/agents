//! The one way a test writes a stand-in script it will run: from a child process, so the file is
//! never open for writing in the test process (another test thread that forks meanwhile holds
//! that open until its child execs, and the start then fails "Text file busy", ETXTBSY).
//!
//! Included by `tests/ask.rs` and by `tests/flow.rs` (`#[path]`), so both use this one.
#![allow(dead_code, reason = "each crate that includes it uses part of it")]

use std::path::Path;

/// Writes `script` to `path` as an executable, from a child process. Written here, the file would be
/// open for writing in this process, and any other test thread that forks meanwhile holds that open
/// until its child execs: a start then fails "Text file busy" (ETXTBSY). The child's is closed
/// before this returns, and nothing else ever had it.
pub fn write_executable(path: &Path, script: &str) {
    let status = std::process::Command::new("sh")
        .args(["-c", "printf %s \"$1\" >\"$2\" && chmod 755 \"$2\"", "sh", script])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "{} written", path.display());
}
