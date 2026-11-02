//! End to end, with stub harnesses in a real bubblewrap sandbox: quota
//! readings, account choice across harnesses, a limit that moves a session
//! (with its history on the same harness, afresh across harnesses), a build
//! job committing on its own branch, and landing that branch.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

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
    // The machine's own git config (commit signing, hooks) is not the test's.
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(identity())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap()
        .success();
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

/// A claude home whose weekly meter resets in about two months.
fn claude_account(home: &Path, label: &str, usage_pct: u32, limited: bool) {
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

/// omp's default profile, with a `stub` provider whose week resets in two days.
fn omp_default(home: &Path, usage_pct: u32, limited: bool) {
    let root = home.join(".omp");
    std::fs::create_dir_all(root.join("agent")).unwrap();
    std::fs::write(root.join("agent/agent.db"), "").unwrap();
    std::fs::write(
        root.join("usage"),
        format!("Usage\n\nStub — 1 account\n  ● someone\n      ● 7 days  ██░░  {usage_pct}.0% used · resets in 2d\n"),
    )
    .unwrap();
    if limited {
        std::fs::write(root.join("limited"), "").unwrap();
    }
}

fn setup(tag: &str, omp: bool) -> Env {
    let root = std::env::temp_dir().join(format!("relay-flow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(repo.join(".stub")).unwrap();
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(here.join("tests/stub-claude.sh"), repo.join(".stub/claude")).unwrap();
    std::fs::copy(here.join("tests/stub-omp.sh"), repo.join(".stub/omp")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let stub = |h: &str| repo.join(".stub").join(h).display().to_string();
    let omp_conf = if omp {
        format!("[harness.omp]\nenabled = true\nprogram = \"{}\"\nmodel = \"stub/m1\"\n", stub("omp"))
    } else {
        "[harness.omp]\nenabled = false\n".to_owned()
    };
    std::fs::write(
        repo.join("relay.toml"),
        format!(
            "name = \"flow\"\n[limits]\nmin_free_disk_gb = 0\n[session]\nnice = 0\n\
             [harness.claude]\nprogram = \"{}\"\n{omp_conf}",
            stub("claude")
        ),
    )
    .unwrap();
    Env { root, home, repo }
}

fn relay(env: &Env, args: &[&str]) -> Output {
    relay_cmd(env, Command::new(env!("CARGO_BIN_EXE_relay")).args(args))
}

fn relay_cmd(env: &Env, cmd: &mut Command) -> Output {
    cmd.current_dir(&env.repo)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", env.root.join("state"))
        .env("XDG_DATA_HOME", env.root.join("data"))
        .env_remove("RELAY_CONFIG")
        .env_remove("OMP_PROFILE")
        .envs(identity())
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what}:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn job(env: &Env, name: &str) -> serde_json::Value {
    let out = relay(env, &["view", "status", "--json"]);
    let world: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    world["jobs"][name].clone()
}

/// The job once it ended, by `view result --wait` (bounded, so a hang fails the test).
fn wait_for(env: &Env, name: &str) -> serde_json::Value {
    let mut cmd = Command::new("timeout");
    cmd.args(["90", env!("CARGO_BIN_EXE_relay"), "view", "result", name, "--wait", "--json"]);
    let out = relay_cmd(env, &mut cmd);
    ok(&out, &format!("waiting for {name}"));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn result(env: &Env, name: &str) -> String {
    String::from_utf8_lossy(&relay(env, &["view", "result", name]).stdout).into_owned()
}

fn have_bwrap() -> bool {
    let ok = Command::new("bwrap").arg("--version").output().is_ok();
    if !ok {
        eprintln!("skipped: bwrap is not installed");
    }
    ok
}

#[test]
fn limited_session_moves_with_its_transcript_and_build_lands() {
    if !have_bwrap() {
        return;
    }
    let env = setup("claude", false);
    // `a` looks emptier, so it is picked first, but every session there hits its limit.
    claude_account(&env.home, "a", 10, true);
    claude_account(&env.home, "b", 50, false);
    ok(&relay(&env, &["probe", "--account", "claude:b"]), "probe");

    ok(&relay(&env, &["queue", "add", "look", "--text", "Look around.", "--readonly"]), "queue");
    let j = wait_for(&env, "look");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["account"], "claude:b");
    assert_eq!(j["attempts"], 2, "first on a, then moved to b");
    // Two transcript turns on b: the one written on a came along.
    let r = result(&env, "look");
    assert!(r.contains("ran on .claude.b with 2 transcript turns"), "{r}");
    let status = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(status.contains("claude:a") && status.contains("limited until"), "{status}");

    ok(&relay(&env, &["queue", "add", "build", "--text", "Make a change."]), "queue");
    let j = wait_for(&env, "build");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["branch"], "relay/build");
    assert!(!env.repo.join("relay-output.txt").exists(), "the repository itself is untouched");

    ok(&relay(&env, &["land", "fetch", "build"]), "land fetch");
    let log = Command::new("git")
        .arg("-C")
        .arg(&env.repo)
        .args(["log", "--format=%s", "-1", "relay/build"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "stub work");
    ok(&relay(&env, &["land", "clean", "build"]), "land clean");
    assert!(job(&env, "build").is_null());
}

#[test]
fn every_harness_is_scheduled_alike_and_a_job_moves_across_harnesses() {
    if !have_bwrap() {
        return;
    }
    let env = setup("mixed", true);
    // omp's stub provider has the most room left per day, so it goes first, and is limited.
    omp_default(&env.home, 20, true);
    claude_account(&env.home, "b", 50, false);
    // The sandbox, for one account of each harness.
    let probe = relay(&env, &["probe"]);
    ok(&probe, "probe");
    let p = String::from_utf8_lossy(&probe.stdout);
    assert!(p.contains("== claude:b") && p.contains("== omp:default"), "{p}");

    ok(&relay(&env, &["queue", "add", "look", "--text", "Look around.", "--readonly"]), "queue");
    let j = wait_for(&env, "look");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["account"], "claude:b");
    assert_eq!(j["attempts"], 2, "first on omp, then on claude");
    // A fresh session on claude: omp's history cannot be read there.
    let r = result(&env, "look");
    assert!(r.contains("ran on .claude.b with 1 transcript turns"), "{r}");
    let status = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(status.contains("omp:default") && status.contains("limited until"), "{status}");

    // A job held to omp waits for it (limited until its reset): it does not slip onto claude.
    ok(&relay(&env, &["queue", "add", "omp-only", "--text", "x", "--readonly", "--harness", "omp"]), "queue");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "omp-only")["status"], "queued");
    ok(&relay(&env, &["queue", "dequeue", "omp-only"]), "dequeue");
    // It never started, so its name is free again; a job that ran keeps its name.
    ok(
        &relay(&env, &["queue", "add", "omp-only", "--text", "y", "--task", "again", "--readonly", "--harness", "omp"]),
        "requeue",
    );
    assert_eq!(
        (job(&env, "omp-only")["status"].as_str(), job(&env, "omp-only")["task"].as_str()),
        (Some("queued"), Some("again"))
    );
    ok(&relay(&env, &["queue", "dequeue", "omp-only"]), "dequeue again");
    let out = relay(&env, &["queue", "add", "look", "--text", "x"]);
    assert_eq!(out.status.code(), Some(4), "a name whose job ran is taken");
    // A bare model needs exactly one harness to belong to.
    let out = relay(&env, &["queue", "add", "vague", "--text", "x", "--model", "m2"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("harness=model"));
}

#[test]
fn an_omp_session_runs_and_resumes_in_its_own_store() {
    if !have_bwrap() {
        return;
    }
    let env = setup("omp", true);
    omp_default(&env.home, 20, false);
    ok(&relay(&env, &["queue", "add", "o", "--text", "Look.", "--readonly", "--harness", "omp"]), "queue");
    let j = wait_for(&env, "o");
    assert_eq!((j["status"].as_str(), j["account"].as_str()), (Some("done"), Some("omp:default")), "{j}");
    let first = j["session_id"].as_str().unwrap().to_owned();
    assert!(first.starts_with("omp-"), "the id omp chose, not relay's: {first}");
    // Resume: the same omp session, from the job's own session store.
    ok(&relay(&env, &["queue", "resume", "o", "--message", "And then?"]), "resume");
    let j = wait_for(&env, "o");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["session_id"].as_str(), Some(first.as_str()));
    let r = result(&env, "o");
    assert!(r.contains("ran on omp:default with 2 transcript turns"), "{r}");
}

#[test]
fn projects_on_one_machine_share_what_they_read_of_an_account() {
    let env = setup("shared", false);
    claude_account(&env.home, "solo", 40, false);
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    std::fs::write(env.repo.join("other.toml"), conf.replace("name = \"flow\"", "name = \"other\"")).unwrap();
    let other = |args: &[&str]| relay(&env, &[&["--config", "other.toml", "--json"], args].concat());
    // Before anyone has read it, the other project has nothing to go on.
    let shown: serde_json::Value = serde_json::from_slice(&other(&["quota", "show"]).stdout).unwrap();
    assert!(shown["accounts"][0]["readAt"].is_null(), "{shown}");
    ok(&relay(&env, &["quota", "refresh"]), "flow reads the account");
    // The other project takes that reading as its own, and does not read again.
    let refreshed = other(&["quota", "refresh", "--stale"]);
    ok(&refreshed, "other refreshes stale readings");
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["read"], serde_json::json!([]), "{refreshed}");
    let shown: serde_json::Value = serde_json::from_slice(&other(&["quota", "show"]).stdout).unwrap();
    let account = &shown["accounts"][0];
    assert!(account["readAt"].is_u64(), "{shown}");
    assert_eq!(account["state"], "ready", "{shown}");
    // It took no reading of its own.
    assert!(!env.root.join("state/relay/other/bus/quota.ndjson").exists());
}
