//! `relay ask`, end to end, in --dry-run (which never starts the child CLI).
//! The `omp` checks run the installed omp, so a renamed flag in an omp
//! release fails here instead of in a caller's request. They skip when omp
//! is not installed.

use std::process::Command;

fn bridge(args: &[&str]) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_relay")).arg("ask").args(args).output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn have(bin: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {bin} >/dev/null")]).status().is_ok_and(|s| s.success())
}

#[test]
fn dry_run_redacts_the_prompt_and_shows_the_contract() {
    let (code, j, err) = bridge(&["claude", "ask", "secret question text", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    let cmd: Vec<&str> = j["command"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(&cmd[..4], ["claude", "-p", "--permission-mode", "plan"]);
    assert!(cmd.contains(&"--session-id"));
    assert!(!cmd.iter().any(|a| a.contains("secret question text")));
    assert_eq!(j["prompt"]["delivery"], "stdin");
    assert!(j["newSessionId"].is_string());
}

#[test]
fn resume_forks() {
    let (code, j, _) = bridge(&["claude", "plan", "docs/plan.md", "--resume", "abc", "--dry-run"]);
    assert_eq!(code, 0);
    let cmd = j["command"].to_string();
    assert!(cmd.contains(r#""--resume","abc","--fork-session""#), "{cmd}");
    assert!(j["newSessionId"].is_null());
}

#[test]
fn refusals_exit_with_their_kind_and_a_reason() {
    let (code, _, err) = bridge(&["claude", "ask", "x", "--continue", "--dry-run"]);
    assert_eq!(code, 5, "refused by the contract");
    assert!(err.contains("Use --resume <id>"));
    let (code, _, err) = bridge(&["omp", "ask", "x", "--resume", "abc", "--dry-run"]);
    assert_eq!(code, 5);
    assert!(err.contains("omp has no --fork-session"));
    let (code, _, err) = bridge(&["--json", "claude", "review", "x", "--dry-run"]);
    assert_eq!(code, 2, "a usage error");
    let e: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(e["kind"], "usage");
    let (code, _, _) = bridge(&["codex", "ask", "x"]);
    assert_eq!(code, 2, "clap's usage errors share the code");
}

#[test]
fn omp_argv_matches_the_installed_omp() {
    if !have("omp") {
        eprintln!("skipped: omp is not installed");
        return;
    }
    let (code, j, err) = bridge(&["omp", "ask", "model smoke, not sent in a dry run", "--model", "opus", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    let cmd: Vec<&str> = j["command"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(&cmd[..5], ["omp", "-p", "--no-title", "--approval-mode", "always-ask"]);
    assert_eq!(j["prompt"]["delivery"], "file");
    assert!(cmd.last().unwrap().starts_with('@'), "the prompt is a file the message names");
    assert_eq!(cmd[cmd.iter().position(|a| *a == "--model").unwrap() + 1], "opus");
    let help = String::from_utf8_lossy(&Command::new("omp").arg("--help").output().unwrap().stdout).into_owned();
    // Every flag the bridge builds, and the escape hatch its resume error names.
    for flag in [
        "--print",
        "--no-title",
        "--approval-mode",
        "always-ask",
        "--model",
        "--no-session",
        "--mode",
        "--session-dir",
        "--resume",
        "--thinking",
        "--append-system-prompt",
        "--auto-approve",
        "--no-extensions",
        "--no-skills",
    ] {
        assert!(help.contains(flag), "omp --help no longer lists {flag}");
    }
}

#[test]
fn claude_argv_matches_the_installed_claude() {
    if !have("claude") {
        eprintln!("skipped: claude is not installed");
        return;
    }
    let help = String::from_utf8_lossy(&Command::new("claude").arg("--help").output().unwrap().stdout).into_owned();
    // Every flag the claude adapter builds, for the bridge and for relay.
    for flag in [
        "--print",
        "--output-format",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
        "--dangerously-skip-permissions",
        "--permission-mode",
        "--max-budget-usd",
        "--append-system-prompt",
        "--model",
        "--effort",
        "--name",
        "--session-id",
        "--resume",
        "--fork-session",
    ] {
        assert!(help.contains(flag), "claude --help no longer lists {flag}");
    }
}
