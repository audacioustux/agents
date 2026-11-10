//! `relay ask`, end to end, in --dry-run (which never starts the child CLI).
//! The `omp`, `claude` and `puku` checks run the installed CLI, so a renamed flag in a
//! release fails here instead of in a caller's request. They skip, saying why, when it is
//! not installed or does not run (a shim whose runtime is gone). Every run here, relay's and
//! each CLI's, is in a throwaway home ([`isolated::Isolated`]), so none can reach the
//! developer's sign-ins or relay state.

fn bridge(args: &[&str]) -> (i32, serde_json::Value, String) {
    let home = Isolated::new("ask");
    let mut cmd = home.command(env!("CARGO_BIN_EXE_relay"));
    let out = cmd.current_dir(env!("CARGO_MANIFEST_DIR")).arg("ask").args(args).output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[path = "support/isolated.rs"]
mod isolated;
use isolated::Isolated;

#[path = "support/executable.rs"]
mod executable;
use executable::write_executable;

/// `bin --help`, asked where it can reach no real sign-in ([`Isolated::ask`]); otherwise why there
/// is none to ask, and the check skips saying so. Every check of an installed CLI here goes through it.
fn installed_help(bin: &str) -> Result<String, String> {
    Isolated::new(bin.rsplit('/').next().unwrap_or(bin)).ask(bin, &["--help"]).map(|(_, help)| help)
}

/// A script written here starts at once, while other threads fork (it failed "Text file busy" in one run
/// of 30 when `a_cli_with_long_help_is_read_while_it_writes` wrote its stand-in itself).
#[test]
fn a_script_written_here_starts_while_other_threads_fork() {
    let d = Isolated::new("busy");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let forker = {
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::process::Command::new("true").status();
            }
        })
    };
    let started = (0..200).map(|i| {
        let script = d.home().join(format!("s{i}"));
        write_executable(&script, "#!/bin/sh\nexit 0\n");
        std::process::Command::new(&script).status().map_err(|e| e.to_string())
    });
    let failed: Vec<String> = started.filter_map(Result::err).collect();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    forker.join().unwrap();
    assert!(failed.is_empty(), "{} of 200 did not start: {:?}", failed.len(), failed.first());
}

/// Review of #40 (round 6): parallel tests in one process shared one `(tag, pid)` home, and a new
/// or dropped instance removed another's live home.
#[test]
fn each_throwaway_home_is_its_own_and_outlives_another_dropped() {
    let a = Isolated::new("same-tag");
    let b = Isolated::new("same-tag");
    assert_ne!(a.home(), b.home(), "two instances with one tag get distinct homes");
    std::fs::write(a.home().join("mine"), "a").unwrap();
    let a_home = a.home().to_path_buf();
    drop(b);
    assert!(a_home.join("mine").exists(), "dropping one instance leaves the other's home alone");
    drop(a);
    assert!(!a_home.exists(), "an instance removes its own home");
}

/// Review of #40 (round 6): stdout was read only after the CLI exited, so help longer than a
/// pipe's buffer blocked it until the 20 s deadline and the check was skipped as "did not answer".
#[test]
fn a_cli_with_long_help_is_read_while_it_writes() {
    let d = Isolated::new("long-help");
    let cli = d.home().join("verbose-cli");
    write_executable(&cli, "#!/bin/sh\nhead -c 300000 /dev/zero | tr '\\0' a\n");
    let started = std::time::Instant::now();
    let help = installed_help(cli.to_str().unwrap()).expect("a verbose CLI still answers");
    assert_eq!(help.len(), 300_000, "the whole help is read");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "well inside the deadline");
}

/// Review of #40: the installed-CLI checks probed `puku-cli --version` with the caller's own
/// `HOME`, `PUKU_CONFIG_DIR` and `APP_NAME`, and puku renews an expired sign-in on start, so a test
/// run could refresh the developer's real one. A stand-in CLI records what each probe gives it.
#[test]
fn an_installed_cli_is_probed_where_it_can_reach_no_real_sign_in() {
    let d = Isolated::new("probe-fixture");
    let seen = d.home().join("seen");
    let cli = d.home().join("fake-cli");
    write_executable(&cli, &format!("#!/bin/sh\nenv >>'{}'\n", seen.display()));
    let help = installed_help(cli.to_str().unwrap());
    assert!(help.is_ok(), "the stand-in runs: {help:?}");
    let env = std::fs::read_to_string(&seen).unwrap();
    let vars: Vec<(&str, &str)> = env.lines().filter_map(|l| l.split_once('=')).collect();
    assert!(vars.len() >= 10, "the probe ran: {env}");
    let home = vars.iter().find(|(k, _)| *k == "HOME").unwrap().1;
    assert!(home.contains("relay-isolated-") && !home.contains("probe-fixture"), "a throwaway home: {home}");
    for (k, v) in &vars {
        let pinned = ["CLAUDE_CONFIG_DIR", "PUKU_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"];
        if pinned.contains(k) {
            assert!(v.starts_with(home), "{k}={v} is outside the throwaway home");
        }
        let allowed = ["PATH", "HOME", "TMPDIR", "LANG", "APP_NAME", "XDG_CACHE_HOME", "PWD", "SHLVL", "_", "OLDPWD"];
        assert!(pinned.contains(k) || allowed.contains(k), "{k} was inherited from the caller");
    }
    assert!(vars.contains(&("APP_NAME", "relay-isolated")));
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
    // Every puku-cli start but --version renews an expired sign-in: this one can reach none.
    let help = match installed_help("puku-cli") {
        Ok(help) => help,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
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
