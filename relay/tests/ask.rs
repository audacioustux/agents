//! `relay ask`, end to end, in --dry-run (which never starts the child CLI).
//! The `omp`, `claude` and `puku` checks run the installed CLI, so a renamed flag in a
//! release fails here instead of in a caller's request. They skip, saying
//! why, when it is not installed or does not run (a shim whose runtime is gone).

use std::process::Command;

fn bridge(args: &[&str]) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_relay")).arg("ask").args(args).output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// `bin --help`, when `bin` is an installed program that runs; otherwise why there is none to ask.
/// A program on PATH that cannot start (a mise shim for an omp whose `bun` is not installed) is
/// no installed CLI: its help says nothing about its flags, so the checks skip rather than report
/// a renamed flag that is not one.
fn installed_help(bin: &str) -> Result<String, String> {
    let on_path =
        Command::new("sh").args(["-c", &format!("command -v {bin} >/dev/null")]).status().is_ok_and(|s| s.success());
    if !on_path {
        return Err(format!("{bin} is not installed"));
    }
    let out = Command::new(bin).arg("--help").output().map_err(|e| format!("{bin} does not start: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(format!("{bin} is on PATH but does not run: {why}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
    let help = match installed_help("omp") {
        Ok(help) => help,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let (code, j, err) = bridge(&["omp", "ask", "model smoke, not sent in a dry run", "--model", "opus", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    let cmd: Vec<&str> = j["command"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(&cmd[..5], ["omp", "-p", "--no-title", "--approval-mode", "always-ask"]);
    assert_eq!(j["prompt"]["delivery"], "file");
    assert!(cmd.last().unwrap().starts_with('@'), "the prompt is a file the message names");
    assert_eq!(cmd[cmd.iter().position(|a| *a == "--model").unwrap() + 1], "opus");
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
    let help = match installed_help("claude") {
        Ok(help) => help,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
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
        "--disallowedTools",
    ] {
        assert!(help.contains(flag), "claude --help no longer lists {flag}");
    }
}

#[test]
fn puku_argv_matches_the_installed_puku() {
    let (code, j, err) = bridge(&["puku", "ask", "secret question text", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    let cmd: Vec<&str> = j["command"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(&cmd[..4], ["puku-cli", "-p", "--permission-mode", "plan"]);
    assert_eq!(j["prompt"]["delivery"], "stdin");
    // Only whether it is on PATH: `installed_help` would ask it in the caller's own home.
    let on_path =
        Command::new("sh").args(["-c", "command -v puku-cli >/dev/null"]).status().is_ok_and(|s| s.success());
    if !on_path {
        eprintln!("skipped: puku-cli is not installed");
        return;
    }
    // In an empty home: every puku-cli start renews an expired sign-in, and this one must touch none.
    let home = std::env::temp_dir().join(format!("relay-ask-puku-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new("puku-cli")
        .arg("--help")
        .env("HOME", &home)
        .env("PUKU_CONFIG_DIR", home.join(".puku-cli"))
        .env_remove("APP_NAME")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let help = String::from_utf8_lossy(&out.stdout).into_owned();
    // Every flag the puku adapter builds: claude's, with --god-mode for --dangerously-skip-permissions.
    for flag in [
        "--print",
        "--output-format",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
        "--god-mode",
        "--permission-mode",
        "--max-budget-usd",
        "--append-system-prompt",
        "--model",
        "--effort",
        "--name",
        "--session-id",
        "--resume",
        "--fork-session",
        "--disallowedTools",
    ] {
        assert!(help.contains(flag), "puku-cli --help no longer lists {flag}");
    }
}
