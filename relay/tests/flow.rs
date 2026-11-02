//! End to end, with a stub agent: quota readings, account choice, a limit
//! that moves a session (and its transcript) to another account, a build job
//! that commits on its own branch inside the sandbox, and landing that branch.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Env {
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        // RELAY_TEST_KEEP=1 leaves the files for a post-mortem.
        if std::env::var_os("RELAY_TEST_KEEP").is_some() {
            eprintln!("kept {}", self.root.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(args).envs(identity()).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

fn identity() -> [(&'static str, &'static str); 4] {
    [
        ("GIT_AUTHOR_NAME", "relay test"),
        ("GIT_AUTHOR_EMAIL", "relay@test.invalid"),
        ("GIT_COMMITTER_NAME", "relay test"),
        ("GIT_COMMITTER_EMAIL", "relay@test.invalid"),
    ]
}

fn account(home: &Path, label: &str, usage_pct: u32, limited: bool) {
    let d = home.join(format!(".claude.{label}"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".credentials.json"), r#"{"claudeAiOauth":{"refreshToken":"r"}}"#).unwrap();
    std::fs::write(d.join(".claude.json"), format!(r#"{{"oauthAccount":{{"emailAddress":"{label}@test.invalid"}}}}"#))
        .unwrap();
    std::fs::write(
        d.join("usage"),
        format!("Current week (all models): {usage_pct}% used · resets Jan 1, 12am (UTC)\n"),
    )
    .unwrap();
    if limited {
        std::fs::write(d.join("limited"), "").unwrap();
    }
}

fn setup() -> Env {
    let root = std::env::temp_dir().join(format!("relay-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(repo.join(".stub")).unwrap();
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stub-claude.sh"), repo.join(".stub/claude"))
        .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    // `a` looks emptier, so it is picked first, but every session there hits its limit.
    account(&home, "a", 10, true);
    account(&home, "b", 50, false);
    std::fs::write(
        repo.join("relay.toml"),
        format!(
            "name = \"flow\"\n[limits]\nmin_free_disk_gb = 0\n[session]\ncommand = [\"{}\"]\nnice = 0\n",
            repo.join(".stub/claude").display()
        ),
    )
    .unwrap();
    Env { root, home, repo }
}

fn relay(env: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .args(args)
        .current_dir(&env.repo)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", env.root.join("state"))
        .env("XDG_DATA_HOME", env.root.join("data"))
        .env_remove("RELAY_CONFIG")
        .envs(identity())
        .output()
        .unwrap()
}

fn job(env: &Env, name: &str) -> serde_json::Value {
    let out = relay(env, &["view", "status", "--json"]);
    let world: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    world["jobs"][name].clone()
}

fn wait_for(env: &Env, name: &str) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let j = job(env, name);
        if matches!(j["status"].as_str(), Some("done" | "failed" | "stopped")) {
            return j;
        }
        assert!(start.elapsed() < Duration::from_secs(90), "{name} did not finish: {j}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[test]
fn limited_session_moves_with_its_transcript_and_build_lands() {
    if Command::new("bwrap").arg("--version").output().is_err() {
        eprintln!("skipped: bwrap is not installed");
        return;
    }
    let env = setup();

    let probe = relay(&env, &["probe", "--account", "b"]);
    assert!(
        probe.status.success(),
        "probe:\n{}{}",
        String::from_utf8_lossy(&probe.stdout),
        String::from_utf8_lossy(&probe.stderr)
    );

    let q = relay(&env, &["queue", "add", "look", "--text", "Look around.", "--readonly"]);
    assert!(q.status.success(), "{}", String::from_utf8_lossy(&q.stderr));
    let j = wait_for(&env, "look");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["account"], "b");
    assert_eq!(j["attempts"], 2, "first on a, then moved to b");
    let result = String::from_utf8_lossy(&relay(&env, &["view", "result", "look"]).stdout).to_string();
    // Two transcript turns on b: the one written on a came along.
    assert!(result.contains("ran on .claude.b with 2 transcript turns"), "{result}");

    let status = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).to_string();
    assert!(status.contains("limited until"), "{status}");

    let q = relay(&env, &["queue", "add", "build", "--text", "Make a change."]);
    assert!(q.status.success());
    let j = wait_for(&env, "build");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["branch"], "relay/build");
    assert!(!env.repo.join("relay-output.txt").exists(), "the repository itself is untouched");

    let land = relay(&env, &["land", "fetch", "build"]);
    assert!(land.status.success(), "{}", String::from_utf8_lossy(&land.stderr));
    let log = Command::new("git")
        .arg("-C")
        .arg(&env.repo)
        .args(["log", "--format=%s", "-1", "relay/build"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "stub work");

    assert!(relay(&env, &["land", "clean", "build"]).status.success());
    assert!(job(&env, "build").is_null());
}
