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

impl Env {
    /// The owner's policy file, when the test wrote one (the machine's own is never read).
    fn policy(&self) -> Option<PathBuf> {
        Some(self.root.join("policy.toml")).filter(|p| p.exists())
    }

    fn write_policy(&self, text: &str) {
        std::fs::write(self.root.join("policy.toml"), text).unwrap();
    }
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
    sign_in(&d, label, &format!("{label}0000-0000-4000-8000-000000000000"));
    std::fs::write(
        d.join("usage"),
        format!("Current week (all models): {usage_pct}% used · resets Jan 1, 12am (UTC)\n"),
    )
    .unwrap();
    if limited {
        std::fs::write(d.join("limited"), "").unwrap();
    }
}

/// What a fixture home's `.claude.json` says it is signed in as. Both strings
/// must never appear in anything relay prints.
const SECRET_EMAIL: &str = "owner.secret@test.invalid";

/// Signs the home `dir` in as the account `uuid`.
fn sign_in(dir: &Path, label: &str, uuid: &str) {
    std::fs::write(
        dir.join(".claude.json"),
        format!(
            r#"{{"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{SECRET_EMAIL}","displayName":"{label}","organizationUuid":"org-{label}","planDisplayName":"Ultra","billingType":"stripe_subscription","profileFetchedAt":1}}}}"#
        ),
    )
    .unwrap();
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
        .env_remove("RELAY_POLICY")
        .envs(env.policy().map(|p| ("RELAY_POLICY", p)))
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

#[test]
fn a_spend_cap_is_a_checkpoint_while_progress_is_real() {
    if !have_bwrap() {
        return;
    }
    let env = setup("checkpoint", false);
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    let limits =
        "[limits]\nbudget_growth = 2.0\njob_ceiling_usd = 1.0\nchecker = { harness = \"claude\", budget_usd = 0.5 }\n";
    std::fs::write(env.repo.join("relay.toml"), conf.replace("[limits]\n", limits)).unwrap();
    // `a` is emptier, so it does the work; the check runs on `b`, so no worker grades itself.
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 50, false);
    let checks =
        |label: &str| std::fs::read_to_string(env.home.join(format!(".claude.{label}/checks"))).unwrap_or_default();

    // Each run costs $0.02: over the $0.015 cap, with a commit, so it is checked and goes on at twice the cap.
    ok(&relay(&env, &["queue", "add", "grow", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "grow");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["attempts"], 2, "{j}");
    assert_eq!(j["budget_usd"], 0.03, "{j}");
    let c = &j["checkpoints"][0];
    assert_eq!((c["checkpoint"].as_u64(), c["verdict"].as_str()), (Some(1), Some("continue")), "{j}");
    assert_eq!((c["checker"].as_str(), j["account"].as_str()), (Some("claude:b"), Some("claude:a")), "{j}");
    assert_eq!(j["checkpoints"].as_array().map(Vec::len), Some(1));
    assert_eq!((checks("a").lines().count(), checks("b").lines().count()), (0, 1));
    assert!(result(&env, "grow").contains("checkpoint 1: continue → $0.03"));

    // Nothing committed or changed since the fork: a stop, and no model is asked.
    ok(&relay(&env, &["queue", "add", "idle", "--text", "[idle] Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "idle");
    assert_eq!(j["status"], "failed", "{j}");
    let c = &j["checkpoints"][0];
    assert_eq!(c["verdict"], "stop", "{j}");
    assert!(c["checker"].is_null(), "{j}");
    assert_eq!(checks("b").lines().count(), 1, "no new check ran");
    let board = String::from_utf8_lossy(&relay(&env, &["view", "status"]).stdout).into_owned();
    assert!(board.contains("checkpoint 1: stop — no new commit"), "{board}");
    // The manual path still works from there.
    ok(&relay(&env, &["queue", "resume", "idle", "--message", "Go on.", "--budget", "0.05"]), "resume");
    assert_eq!(wait_for(&env, "idle")["status"], "done");
}

/// The job's status, once it has been `want` (bounded, so a hang fails the test).
fn until(env: &Env, name: &str, want: &str) {
    for _ in 0..300 {
        if job(env, name)["status"] == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{name} never became {want}: {}", job(env, name));
}

fn board(env: &Env) -> String {
    String::from_utf8_lossy(&relay(env, &["view", "status"]).stdout).into_owned()
}

#[test]
fn a_job_starts_when_what_it_waits_for_is_done() {
    if !have_bwrap() {
        return;
    }
    let env = setup("after", false);
    claude_account(&env.home, "a", 10, false);
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    ok(&relay(&env, &["queue", "add", "first", "--text", "One.", "--readonly"]), "queue first");
    until(&env, "first", "running");
    ok(&relay(&env, &["queue", "add", "second", "--text", "Two.", "--readonly", "--after", "first"]), "queue second");
    std::thread::sleep(Duration::from_secs(2));
    let second = job(&env, "second");
    assert_eq!(second["status"], "queued", "it waits while first runs: {second}");
    assert_eq!(second["after"], serde_json::json!(["first"]));
    assert_eq!(second["pending"][0]["job"], "first");
    assert_eq!(second["pending"][0]["state"], "running");
    assert!(board(&env).contains("[after first]"), "{}", board(&env));

    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "first")["status"], "done");
    let second = wait_for(&env, "second");
    assert_eq!(second["status"], "done", "{second}");
    assert!(second["started_at"].as_u64() >= job(&env, "first")["ended_at"].as_u64(), "started after first ended");
    assert_eq!(job(&env, "second")["pending"], serde_json::json!([]));
}

#[test]
fn a_job_whose_dependency_failed_is_blocked_until_released() {
    if !have_bwrap() {
        return;
    }
    let env = setup("blocked", false);
    claude_account(&env.home, "a", 10, false);
    let fail = env.home.join(".claude.a/fail");
    std::fs::write(&fail, "").unwrap();
    ok(&relay(&env, &["queue", "add", "base", "--text", "One.", "--readonly"]), "queue base");
    ok(&relay(&env, &["queue", "add", "late", "--text", "Two.", "--readonly", "--after", "base"]), "queue late");
    ok(&relay(&env, &["queue", "add", "free", "--text", "Three.", "--readonly", "--after", "base,base"]), "queue free");
    until(&env, "base", "failed");
    std::fs::remove_file(&fail).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let late = job(&env, "late");
    assert_eq!((late["status"].as_str(), late["attempts"].as_u64()), (Some("queued"), Some(0)), "{late}");
    assert_eq!(late["pending"][0]["blocked"], true);
    assert!(board(&env).contains("[waits for base (failed)]"), "{}", board(&env));

    let out = relay(&env, &["queue", "release", "late", "--from", "elsewhere"]);
    assert_eq!(out.status.code(), Some(2), "it does not wait for that");
    ok(&relay(&env, &["queue", "release", "late", "--from", "base"]), "release");
    assert_eq!(job(&env, "late")["after"], serde_json::json!([]));
    assert_eq!(wait_for(&env, "late")["status"], "done");
    assert_eq!(job(&env, "free")["status"], "queued", "release was for late only");
    let out = relay(&env, &["queue", "release", "late"]);
    assert_eq!(out.status.code(), Some(4), "nothing left to release");
}

#[test]
fn after_names_only_jobs_that_exist() {
    let env = setup("unknown", false);
    let out = relay(&env, &["queue", "add", "b", "--text", "x", "--after", "nope"]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let out = relay(&env, &["queue", "add", "b", "--text", "x", "--after", "b"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(job(&env, "b").is_null(), "a refused job is not recorded");
}

const POLICY: &str = r#"
[session]
banned_models = { "^minimax-code/" = "MiniMax is banned", "^(haiku|sonnet|opus)$" = "use the full id" }

[profiles.mechanical]
harness = "claude"
model = "claude-haiku-5-5"
effort = "xhigh"
budget = 3
note = "a precise brief"

[profiles.hunt]
harness = "claude"
model = "claude-opus-5-5"
effort = "high"
readonly = true
max_running = 1
note = "costly"

[profiles.daily]
harness = "claude"
readonly = true
max_per_day = 1

[profiles.bad]
harness = "claude"
model = "minimax-code/M3"
"#;

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_profile_fills_what_the_line_leaves_and_flags_win() {
    let env = setup("profile-fill", false);
    env.write_policy(POLICY);
    ok(&relay(&env, &["queue", "add", "plain", "--text", "One.", "--profile", "mechanical"]), "queue plain");
    let j = job(&env, "plain");
    assert_eq!(j["profile"], "mechanical");
    assert_eq!(j["harnesses"], serde_json::json!(["claude"]));
    assert_eq!(j["models"], serde_json::json!({"claude": "claude-haiku-5-5"}));
    assert_eq!(
        (j["effort"].as_str(), j["budget_usd"].as_f64(), j["readonly"].as_bool()),
        (Some("xhigh"), Some(3.0), Some(false))
    );

    let args = [
        "queue",
        "add",
        "mine",
        "--text",
        "Two.",
        "--profile",
        "hunt",
        "--effort",
        "low",
        "--budget",
        "7",
        "--no-readonly",
        "--model",
        "claude-sonnet-5-5",
    ];
    ok(&relay(&env, &args), "queue mine");
    let j = job(&env, "mine");
    assert_eq!(j["models"], serde_json::json!({"claude": "claude-sonnet-5-5"}), "{j}");
    assert_eq!(
        (j["effort"].as_str(), j["budget_usd"].as_f64(), j["readonly"].as_bool()),
        (Some("low"), Some(7.0), Some(false))
    );
    assert_eq!(j["profile"], "hunt", "the job still records its profile");

    ok(&relay(&env, &["queue", "add", "ro", "--text", "Three.", "--profile", "hunt"]), "queue ro");
    assert_eq!(job(&env, "ro")["readonly"], true, "the profile's read-only fills the unset flag");
    assert_eq!(job(&env, "ro")["budget_usd"], 10.0, "no budget anywhere: the project's default");

    let out = relay(&env, &["queue", "add", "pinned", "--text", "Four.", "--profile", "hunt", "--harness", "omp"]);
    assert_eq!(out.status.code(), Some(5), "omp is disabled here: {}", stderr(&out));
}

#[test]
fn an_unknown_or_banned_profile_is_refused() {
    let env = setup("profile-refuse", false);
    let out = relay(&env, &["queue", "add", "a", "--text", "x", "--profile", "nope"]);
    assert_eq!(out.status.code(), Some(3), "no policy and no profiles: {}", stderr(&out));
    env.write_policy(POLICY);
    let out = relay(&env, &["queue", "add", "a", "--text", "x", "--profile", "nope"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("bad, daily, hunt, mechanical"), "{}", stderr(&out));
    let out = relay(&env, &["queue", "add", "a", "--text", "x", "--profile", "bad"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("MiniMax is banned"), "{}", stderr(&out));
    let out = relay(&env, &["queue", "add", "a", "--text", "x", "--profile", "mechanical", "--model", "sonnet"]);
    assert_eq!(out.status.code(), Some(5), "the policy's ban reaches the project: {}", stderr(&out));
    assert!(job(&env, "a").is_null(), "a refused job is not recorded");
}

#[test]
fn profiles_and_the_merged_config_are_shown() {
    let env = setup("profile-show", false);
    let out = relay(&env, &["profiles"]);
    ok(&out, "profiles without a policy");
    assert!(String::from_utf8_lossy(&out.stdout).contains("no profiles"));
    env.write_policy(POLICY);
    let mut conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    conf.push_str("[profiles.hunt]\neffort = \"medium\"\n");
    std::fs::write(env.repo.join("relay.toml"), conf).unwrap();
    let out = relay(&env, &["profiles", "--json"]);
    ok(&out, "profiles");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["profiles"]["hunt"]["effort"], "medium", "the project's field");
    assert_eq!(v["profiles"]["hunt"]["model"], "claude-opus-5-5", "the policy's field");
    assert_eq!(v["profiles"]["hunt"]["note"], "costly");
    assert_eq!(v["profiles"]["bad"]["banned"], "MiniMax is banned");
    let text = String::from_utf8_lossy(&relay(&env, &["profiles"]).stdout).into_owned();
    assert!(text.contains("hunt") && text.contains("costly"), "{text}");
    let out = relay(&env, &["config", "show", "--json"]);
    ok(&out, "config show");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["profiles"]["hunt"]["effort"]["from"], "project");
    assert_eq!(v["profiles"]["hunt"]["model"]["from"], "policy");
    assert_eq!(v["banned_models"]["^minimax-code/"]["from"], "policy");
    let text = String::from_utf8_lossy(&relay(&env, &["config", "show"]).stdout).into_owned();
    assert!(
        text.contains("effort = medium  [project]") && text.contains("model = claude-opus-5-5  [policy]"),
        "{text}"
    );
}

#[test]
fn max_running_holds_a_second_job_of_the_profile() {
    if !have_bwrap() {
        return;
    }
    let env = setup("profile-running", false);
    env.write_policy(POLICY);
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 10, false);
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    std::fs::write(env.home.join(".claude.b/hold"), "").unwrap();
    ok(&relay(&env, &["queue", "add", "h1", "--text", "One.", "--profile", "hunt"]), "queue h1");
    ok(&relay(&env, &["queue", "add", "h2", "--text", "Two.", "--profile", "hunt"]), "queue h2");
    until(&env, "h1", "running");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "h2")["status"], "queued", "a free account does not matter: the profile is full");
    assert!(board(&env).contains("[profile hunt: 1 running (max 1)]"), "{}", board(&env));
    assert!(board(&env).contains("hunt"), "the board names the profile");

    std::fs::remove_file(&hold).unwrap();
    std::fs::remove_file(env.home.join(".claude.b/hold")).unwrap();
    assert_eq!(wait_for(&env, "h1")["status"], "done");
    assert_eq!(wait_for(&env, "h2")["status"], "done", "it starts once the first is done");
}

#[test]
fn max_per_day_counts_starts_from_the_bus_and_the_profile_stays_across_resumes() {
    if !have_bwrap() {
        return;
    }
    let env = setup("profile-daily", false);
    env.write_policy(POLICY);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "d1", "--text", "One.", "--profile", "daily"]), "queue d1");
    assert_eq!(wait_for(&env, "d1")["status"], "done");
    ok(&relay(&env, &["queue", "add", "d2", "--text", "Two.", "--profile", "daily"]), "queue d2");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "d2")["status"], "queued");
    let board = board(&env);
    assert!(board.contains("[profile daily: 1 started in 24h (max 1), next slot "), "{board}");
    // Cleaning the first job away does not give its start back.
    ok(&relay(&env, &["land", "clean", "d1"]), "clean d1");
    ok(&relay(&env, &["queue", "dequeue", "d2"]), "dequeue d2");
    ok(&relay(&env, &["queue", "add", "d3", "--text", "Three.", "--profile", "daily"]), "queue d3");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "d3")["status"], "queued", "the start is counted from the bus, not from the job");
    ok(&relay(&env, &["queue", "dequeue", "d3"]), "dequeue d3");
}

#[test]
fn a_resumed_job_keeps_its_profile() {
    if !have_bwrap() {
        return;
    }
    let env = setup("profile-resume", false);
    env.write_policy(POLICY);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "m1", "--text", "One.", "--profile", "mechanical"]), "queue m1");
    assert_eq!(wait_for(&env, "m1")["status"], "done");
    ok(&relay(&env, &["queue", "resume", "m1", "--message", "Go on."]), "resume");
    assert_eq!(job(&env, "m1")["profile"], "mechanical");
    assert_eq!(wait_for(&env, "m1")["profile"], "mechanical");
}

#[test]
fn two_homes_signed_in_as_one_account_share_a_seat_and_the_board_says_so() {
    if !have_bwrap() {
        return;
    }
    let env = setup("twins", false);
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 10, false);
    let uuid = "76de9d00-1111-4222-8333-444455556666";
    sign_in(&env.home.join(".claude.a"), "a", uuid);
    sign_in(&env.home.join(".claude.b"), "b", uuid);
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    std::fs::write(env.home.join(".claude.b/hold"), "").unwrap();
    ok(&relay(&env, &["queue", "add", "first", "--text", "One.", "--readonly"]), "queue first");
    until(&env, "first", "running");
    ok(&relay(&env, &["queue", "add", "second", "--text", "Two.", "--readonly"]), "queue second");
    std::thread::sleep(Duration::from_secs(2));
    let second = job(&env, "second");
    assert_eq!(second["status"], "queued", "the twin has no seat while its other half runs: {second}");

    let table = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    let json = String::from_utf8_lossy(&relay(&env, &["quota", "show", "--json"]).stdout).into_owned();
    assert!(table.contains("same sign-in as claude:a") && table.contains("Ultra #"), "{table}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["accounts"][1]["sameAs"], "claude:a", "{json}");
    assert_eq!(v["accounts"][0]["identity"]["plan"], "Ultra");
    assert_eq!(v["accounts"][0]["identity"]["id"], v["accounts"][1]["identity"]["id"]);
    for secret in [SECRET_EMAIL, uuid, "76de9d00"] {
        assert!(!table.contains(secret) && !json.contains(secret), "{secret} leaked:\n{table}\n{json}");
    }
    std::fs::remove_file(env.home.join(".claude.b/hold")).unwrap();
    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "first")["status"], "done");
    assert_eq!(wait_for(&env, "second")["status"], "done");
}

#[test]
fn a_session_that_lapsed_is_signed_out_until_a_reading_succeeds() {
    if !have_bwrap() {
        return;
    }
    let env = setup("lapsed", false);
    claude_account(&env.home, "a", 10, false);
    let usage = env.home.join(".claude.a/usage");
    let good = std::fs::read_to_string(&usage).unwrap();
    std::fs::write(&usage, "Your session has expired · Please run /login\n").unwrap();
    ok(&relay(&env, &["quota", "refresh"]), "refresh");
    let table = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(table.contains("signed out since"), "{table}");
    ok(&relay(&env, &["queue", "add", "look", "--text", "Look.", "--readonly"]), "queue");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "look")["status"], "queued", "a signed-out account is never chosen");

    std::fs::write(&usage, good).unwrap();
    ok(&relay(&env, &["quota", "refresh"]), "refresh again");
    let table = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(!table.contains("signed out") && table.contains("ready"), "{table}");
    assert_eq!(wait_for(&env, "look")["status"], "done");
}

#[test]
fn a_home_with_no_sign_in_is_never_chosen() {
    if !have_bwrap() {
        return;
    }
    let env = setup("unsigned", false);
    claude_account(&env.home, "a", 0, false);
    claude_account(&env.home, "b", 50, false);
    std::fs::remove_file(env.home.join(".claude.a/.claude.json")).unwrap();
    ok(&relay(&env, &["queue", "add", "look", "--text", "Look.", "--readonly"]), "queue");
    let j = wait_for(&env, "look");
    assert_eq!(j["account"], "claude:b", "a has more room but is not signed in: {j}");
    let table = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(table.contains("claude:a") && table.contains("not signed in"), "{table}");
}

/// Adds `limits` under `[limits]` and appends `extra` to the project's config.
fn configure(env: &Env, limits: &str, extra: &str) {
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    let conf = conf.replace("[limits]\n", &format!("[limits]\n{limits}"));
    std::fs::write(env.repo.join("relay.toml"), format!("{conf}{extra}")).unwrap();
}

/// The job's decisions, oldest first.
fn decisions(env: &Env, name: &str) -> Vec<serde_json::Value> {
    let out = relay(env, &["view", "status", "--json"]);
    let world: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut ds: Vec<serde_json::Value> = world["decisions"]
        .as_object()
        .map(|m| m.values().filter(|d| d["job"] == name).cloned().collect())
        .unwrap_or_default();
    ds.sort_by_key(|d| d["requested_at"].as_u64());
    ds
}

fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap_or_default().lines().count()
}

fn commits(dir: &serde_json::Value) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir.as_str().unwrap())
        .args(["log", "--format=%s"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_run_end_is_decided_by_the_rule_or_an_agent_that_queues_a_follow_up() {
    if !have_bwrap() {
        return;
    }
    let env = setup("ended", false);
    configure(
        &env,
        "job_ceiling_usd = 5.0\n",
        "[hooks]\nended = [\"rule\", \"agent:triage\"]\n[profiles.triage]\nharness = \"claude\"\nbudget = 0.5\n\
         [profiles.cheap]\nharness = \"claude\"\nmodel = \"claude-haiku-5-5\"\nbudget = 0.5\n",
    );
    claude_account(&env.home, "a", 10, false);
    let asked = || lines(&env.home.join(".claude.a/decisions"));

    // A clean done: the rule accepts it, and no agent is asked.
    ok(&relay(&env, &["queue", "add", "clean", "--text", "[status-done] Make a change."]), "queue");
    assert_eq!(wait_for(&env, "clean")["status"], "done");
    let d = &decisions(&env, "clean")[0];
    assert_eq!(
        (d["steps"][0]["handler"].as_str(), d["action"]["action"].as_str()),
        (Some("rule"), Some("accept")),
        "{d}"
    );
    assert_eq!(asked(), 0);

    // No `status: done` in the report: the agent queues a follow-up after it, under its profile.
    let answer =
        r#"{"action": "follow-up", "brief": "[status-done] Check the change.", "profile": "cheap", "why": "check it"}"#;
    std::fs::write(env.home.join(".claude.a/decision"), answer).unwrap();
    ok(&relay(&env, &["queue", "add", "work", "--text", "Make a change."]), "queue");
    let j = wait_for(&env, "work");
    assert_eq!(j["status"], "done", "{j}");
    assert!(j["reason"].as_str().unwrap_or_default().contains("follow-up work-next queued"), "{j}");
    let d = &decisions(&env, "work")[0];
    let steps: Vec<&str> = d["steps"].as_array().unwrap().iter().filter_map(|s| s["handler"].as_str()).collect();
    assert_eq!(steps, ["rule", "agent:triage"], "{d}");
    assert!(j["cost_usd"].as_f64().unwrap() > 0.025, "the agent's spend counts: {j}");
    let n = wait_for(&env, "work-next");
    assert_eq!(n["status"], "done", "{n}");
    assert_eq!((n["profile"].as_str(), n["after"].clone()), (Some("cheap"), serde_json::json!(["work"])), "{n}");
    assert_eq!(n["models"]["claude"], "claude-haiku-5-5");
    assert_eq!(asked(), 1, "the follow-up's clean done needed no agent");
}

#[test]
fn the_orchestrator_answers_a_parked_decision_and_its_silence_falls_through() {
    if !have_bwrap() {
        return;
    }
    let env = setup("orchestrator", false);
    let hooks = "[hooks]\nended = [\"orchestrator\", \"rule\"]\norchestrator_timeout_minutes = 1.0\n";
    configure(&env, "job_ceiling_usd = 5.0\n", hooks);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "park", "--text", "Make a change."]), "queue");
    let mut cmd = Command::new("timeout");
    cmd.args(["60", env!("CARGO_BIN_EXE_relay"), "decide", "list", "--wait", "--timeout", "50", "--json"]);
    let out = relay_cmd(&env, &mut cmd);
    ok(&out, "decide list --wait");
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let p = &listed["pending"][0];
    assert_eq!((p["job"].as_str(), p["point"].as_str()), (Some("park"), Some("ended")), "{listed}");
    assert!(p["request"]["allowed"].as_array().unwrap().contains(&"accept".into()), "{listed}");
    let id = p["id"].as_str().unwrap();
    let b = board(&env);
    assert!(b.contains("decisions waiting") && b.contains(id), "{b}");
    assert_eq!(job(&env, "park")["status"], "checking", "a parked run holds no seat");

    // Only the point's actions, within policy, are carried out.
    let bad = relay(&env, &["decide", "answer", id, "--action", "continue"]);
    assert_eq!(bad.status.code(), Some(5), "{}", stderr(&bad));
    ok(&relay(&env, &["decide", "answer", id, "--action", "stop", "--why", "not what I asked"]), "answer");
    let j = wait_for(&env, "park");
    assert_eq!((j["status"].as_str(), j["reason"].as_str()), (Some("failed"), Some("not what I asked")), "{j}");
    assert_eq!(relay(&env, &["decide", "answer", id, "--action", "accept"]).status.code(), Some(4));

    // Unanswered past the timeout: the next handler decides.
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    let conf = conf.replace("orchestrator_timeout_minutes = 1.0", "orchestrator_timeout_minutes = 0.05");
    std::fs::write(env.repo.join("relay.toml"), conf).unwrap();
    ok(&relay(&env, &["queue", "add", "quiet", "--text", "[status-done] Make a change."]), "queue");
    assert_eq!(wait_for(&env, "quiet")["status"], "done");
    let d = &decisions(&env, "quiet")[0];
    let steps: Vec<(&str, &str)> = d["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["handler"].as_str().unwrap(), s["action"]["action"].as_str().unwrap()))
        .collect();
    assert_eq!(steps, [("orchestrator", "defer"), ("rule", "accept")], "{d}");
}

#[test]
fn a_command_decides_at_a_cap_a_bad_answer_defers_and_a_big_budget_is_bounded() {
    if !have_bwrap() {
        return;
    }
    let env = setup("command", false);
    let script = env.root.join("decide.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\ncat \"$1\"\n").unwrap();
    Command::new("chmod").arg("755").arg(&script).status().unwrap();
    let (answer, fallback) = (env.root.join("answer.json"), env.root.join("fallback.json"));
    std::fs::write(&fallback, r#"{"action": "continue", "why": "the second says go"}"#).unwrap();
    let (sh, first, second) = (script.display(), answer.display(), fallback.display());
    let chain = format!("[hooks]\ncap = [\"rule\", \"command:{sh} {first}\", \"command:{sh} {second}\"]\n");
    configure(&env, "job_ceiling_usd = 1.0\nbudget_growth = 2.0\n", &chain);
    claude_account(&env.home, "a", 10, false);

    std::fs::write(&answer, r#"{"action": "continue", "budget": 0.03, "why": "the script says go"}"#).unwrap();
    ok(&relay(&env, &["queue", "add", "go", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "go");
    assert_eq!((j["status"].as_str(), j["budget_usd"].as_f64()), (Some("done"), Some(0.03)), "{j}");
    let c = &j["checkpoints"][0];
    assert_eq!(c["verdict"], "continue", "{j}");
    assert!(c["checker"].as_str().unwrap().starts_with("command:"), "{j}");

    std::fs::write(&answer, "not json").unwrap();
    ok(&relay(&env, &["queue", "add", "garbled", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "garbled");
    assert_eq!(
        (j["status"].as_str(), j["checkpoints"][0]["verdict"].as_str()),
        (Some("done"), Some("continue")),
        "{j}"
    );
    let d = &decisions(&env, "garbled")[0];
    let steps: Vec<(&str, &str)> = d["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["action"]["action"].as_str().unwrap(), s["why"].as_str().unwrap()))
        .collect();
    assert!(steps[1].0 == "defer" && steps[1].1.contains("malformed"), "the bad answer passed it on: {d}");
    assert_eq!(steps[2].0, "continue", "{d}");

    std::fs::write(&answer, r#"{"action": "continue", "budget": 100}"#).unwrap();
    ok(&relay(&env, &["queue", "add", "greedy", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "greedy");
    assert_eq!((j["status"].as_str(), j["budget_usd"].as_f64()), (Some("done"), Some(0.03)), "bounded: {j}");
    let d = &decisions(&env, "greedy")[0];
    assert_eq!(d["action"]["budget"].as_f64(), Some(0.03), "recorded as granted: {d}");
}

#[test]
fn a_fork_resumes_its_parents_session_on_its_account_from_its_branch() {
    if !have_bwrap() {
        return;
    }
    let env = setup("fork", false);
    claude_account(&env.home, "a", 10, false);
    // `b` is emptier, so only the fork's preference keeps the child on `a`.
    claude_account(&env.home, "b", 5, false);
    ok(&relay(&env, &["queue", "add", "base", "--text", "Make a change.", "--account", "claude:a"]), "queue");
    let parent = wait_for(&env, "base");
    assert_eq!(parent["status"], "done", "{parent}");
    ok(&relay(&env, &["queue", "add", "child", "--text", "Go on.", "--fork", "base", "--after", "base"]), "fork");
    let child = wait_for(&env, "child");
    assert_eq!((child["status"].as_str(), child["account"].as_str()), (Some("done"), Some("claude:a")), "{child}");
    let sid = parent["session_id"].as_str().unwrap();
    let argv = std::fs::read_to_string(env.home.join(".claude.a/argv")).unwrap();
    assert!(argv.contains(&format!("--resume {sid} --fork-session")), "{argv}");
    assert_ne!(child["session_id"].as_str(), Some(sid), "a session of its own: {child}");
    assert!(result(&env, "child").contains("with 2 transcript turns"), "{}", result(&env, "child"));
    assert_eq!(commits(&child["workdir"]).lines().count(), 3, "init, base's work, its own");

    let none = relay(&env, &["queue", "add", "orphan", "--text", "x", "--fork", "nobody"]);
    assert_eq!(none.status.code(), Some(3));
}

#[test]
fn a_build_after_a_build_starts_from_its_branch_and_diverging_bases_fail() {
    if !have_bwrap() {
        return;
    }
    let env = setup("base", false);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "one", "--text", "Make a change."]), "queue");
    assert_eq!(wait_for(&env, "one")["status"], "done");
    ok(&relay(&env, &["queue", "add", "two", "--text", "Another.", "--after", "one"]), "queue");
    let two = wait_for(&env, "two");
    assert_eq!(two["status"], "done", "{two}");
    assert_eq!(commits(&two["workdir"]).lines().count(), 3, "it holds one's work: {}", commits(&two["workdir"]));

    ok(&relay(&env, &["queue", "add", "other", "--text", "Elsewhere."]), "queue");
    assert_eq!(wait_for(&env, "other")["status"], "done");
    ok(&relay(&env, &["queue", "add", "three", "--text", "Both.", "--after", "one,other"]), "queue");
    let three = wait_for(&env, "three");
    assert_eq!(three["status"], "failed", "{three}");
    assert!(three["reason"].as_str().unwrap().contains("diverge"), "{three}");
}

#[test]
fn a_job_held_too_long_is_decided_and_forced_where_policy_allows() {
    if !have_bwrap() {
        return;
    }
    let env = setup("held", false);
    let script = env.root.join("decide.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"action\": \"force\", \"why\": \"urgent\"}'\n")
        .unwrap();
    Command::new("chmod").arg("755").arg(&script).status().unwrap();
    let hooks = format!(
        "[hooks]\nheld = [\"rule\", \"command:{}\"]\nheld_after_minutes = 0.0\nallow_force = true\n\
         [profiles.solo]\nharness = \"claude\"\nmax_running = 1\n",
        script.display()
    );
    configure(&env, "job_ceiling_usd = 5.0\n", &hooks);
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 10, false);
    let holds = [env.home.join(".claude.a/hold"), env.home.join(".claude.b/hold")];
    for h in &holds {
        std::fs::write(h, "").unwrap();
    }
    ok(&relay(&env, &["queue", "add", "s1", "--text", "One.", "--readonly", "--profile", "solo"]), "queue s1");
    until(&env, "s1", "running");
    ok(&relay(&env, &["queue", "add", "s2", "--text", "Two.", "--readonly", "--profile", "solo"]), "queue s2");
    until(&env, "s2", "running");
    let d = &decisions(&env, "s2")[0];
    assert_eq!((d["point"].as_str(), d["action"]["action"].as_str()), (Some("held"), Some("force")), "{d}");
    for h in &holds {
        std::fs::remove_file(h).unwrap();
    }
    assert_eq!(wait_for(&env, "s1")["status"], "done");
    assert_eq!(wait_for(&env, "s2")["status"], "done");
}

/// Where relay keeps the job's records (its log, answer and brief).
fn records(env: &Env, name: &str, ext: &str) -> PathBuf {
    env.root.join("state/relay/flow/jobs").join(format!("{name}.{ext}"))
}

fn workdir(env: &Env, name: &str) -> PathBuf {
    PathBuf::from(job(env, name)["workdir"].as_str().unwrap())
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The repository's commit subjects on `rev`, newest first; empty when there is no such ref.
fn subjects(env: &Env, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(&env.repo)
        .args(["log", "--format=%s", rev])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    out_of(&out)
}

#[test]
fn gc_empties_the_clones_of_ended_jobs_nothing_holds_and_only_those() {
    if !have_bwrap() {
        return;
    }
    let env = setup("gc-outputs", false);
    configure(&env, "max_running = 1\n", "[gc]\noutputs_after_hours = 0\n[session.env]\nCARGO_INCREMENTAL = \"0\"\n");
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "old", "--text", "Make a change."]), "queue old");
    assert_eq!(wait_for(&env, "old")["status"], "done");
    ok(&relay(&env, &["queue", "add", "waited", "--text", "Make a change."]), "queue waited");
    assert_eq!(wait_for(&env, "waited")["status"], "done");
    let cargo = std::fs::read_to_string(env.home.join(".claude.a/cargo")).unwrap();
    assert_eq!(cargo.lines().collect::<Vec<_>>(), ["0", "0"], "[session.env] reaches every job: {cargo}");

    // A running job holds the only seat, so a job queued after `waited` stays queued and keeps it.
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    ok(&relay(&env, &["queue", "add", "busy", "--text", "Make a change."]), "queue busy");
    until(&env, "busy", "running");
    ok(&relay(&env, &["queue", "add", "next", "--text", "Review it.", "--after", "waited"]), "queue next");
    for name in ["old", "waited", "busy"] {
        let dir = workdir(&env, name);
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/big"), vec![0u8; 50_000]).unwrap();
    }

    let dry = relay(&env, &["land", "gc", "--dry-run"]);
    ok(&dry, "dry run");
    let text = out_of(&dry);
    assert!(text.contains("gc: would remove") && text.contains("old/target"), "{text}");
    assert!(workdir(&env, "old").join("target/debug/big").exists(), "a dry run changes nothing");

    let pass = relay(&env, &["land", "gc", "--json"]);
    ok(&pass, "gc");
    let report: serde_json::Value = serde_json::from_slice(&pass.stdout).unwrap();
    let acted: Vec<&str> = report["actions"].as_array().unwrap().iter().filter_map(|a| a["job"].as_str()).collect();
    assert_eq!(acted, ["old"], "{report}");
    assert!(report["freed_bytes"].as_u64().unwrap() >= 50_000, "{report}");
    let old = workdir(&env, "old");
    assert!(!old.join("target").exists(), "its build outputs are gone");
    assert!(old.join("relay-output.txt").exists(), "its source stays");
    assert_eq!(subjects(&env, "HEAD").lines().count(), 1);
    let tip = Command::new("git").arg("-C").arg(&old).args(["rev-parse", "--verify", "relay/old"]).output().unwrap();
    assert!(tip.status.success(), "and its branch");
    assert_eq!(job(&env, "old")["collected"], "outputs");
    assert!(board(&env).contains("(build outputs removed)"), "{}", board(&env));
    assert!(workdir(&env, "waited").join("target/debug/big").exists(), "a queued job waits for it");
    assert!(workdir(&env, "busy").join("target/debug/big").exists(), "a running job's clone is never touched");
    let events = out_of(&relay(&env, &["view", "events"]));
    assert!(events.contains("\"type\":\"collected\"") && events.contains("\"what\":\"outputs\""), "{events}");

    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "busy")["status"], "done");
}

#[test]
fn a_done_clone_is_removed_with_its_unfetched_commits_rescued_and_the_job_can_resume() {
    if !have_bwrap() {
        return;
    }
    let env = setup("gc-clones", false);
    configure(&env, "", "[gc]\nclones_after_days = 0\n");
    claude_account(&env.home, "a", 10, false);
    for name in ["lost", "landed"] {
        ok(&relay(&env, &["queue", "add", name, "--text", "Make a change."]), "queue");
        assert_eq!(wait_for(&env, name)["status"], "done");
    }
    ok(&relay(&env, &["land", "fetch", "landed"]), "land fetch");
    let dirs = ["lost", "landed"].map(|n| workdir(&env, n));

    let dry = out_of(&relay(&env, &["land", "gc", "--dry-run"]));
    assert!(dry.contains("would remove") && workdir(&env, "lost").exists(), "{dry}");
    assert!(subjects(&env, "refs/relay/rescue/lost").is_empty(), "a dry run rescues nothing");

    let text = out_of(&relay(&env, &["land", "gc"]));
    assert!(text.contains("rescued lost's unfetched commits into refs/relay/rescue/lost"), "{text}");
    assert!(text.contains("gc: removed") && text.contains("freed"), "{text}");
    assert!(dirs.iter().all(|d| !d.exists()), "both clones are gone");
    assert_eq!(subjects(&env, "refs/relay/rescue/lost").trim(), "stub work\ninit", "its commit is safe");
    assert!(subjects(&env, "refs/relay/rescue/landed").is_empty(), "a fetched branch needs no rescue");
    assert!(subjects(&env, "relay/landed").contains("stub work"));
    assert_eq!(job(&env, "lost")["collected"], "clone");
    let fetch = relay(&env, &["land", "fetch", "lost"]);
    assert_eq!(fetch.status.code(), Some(4), "{}", stderr(&fetch));
    assert!(stderr(&fetch).contains("refs/relay/rescue/lost"), "{}", stderr(&fetch));

    ok(&relay(&env, &["queue", "resume", "lost", "--message", "Go on."]), "resume");
    assert_eq!(wait_for(&env, "lost")["status"], "done");
    let log = commits(&job(&env, "lost")["workdir"]);
    assert_eq!(log.matches("stub work").count(), 2, "the resumed clone started from the rescued work: {log}");
}

#[test]
fn an_old_log_is_gzipped_and_still_read_by_view_result() {
    if !have_bwrap() {
        return;
    }
    let env = setup("gc-logs", false);
    configure(&env, "", "[gc]\nlogs_gzip_after_days = 0\n");
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "talk", "--text", "Look.", "--readonly"]), "queue");
    assert_eq!(wait_for(&env, "talk")["status"], "done");
    let plain = std::fs::read_to_string(records(&env, "talk", "log.ndjson")).unwrap();
    ok(&relay(&env, &["land", "gc"]), "gc");
    assert!(!records(&env, "talk", "log.ndjson").exists());
    let gz = records(&env, "talk", "log.ndjson.gz");
    let back = Command::new("gzip").arg("-dc").arg(&gz).output().unwrap();
    assert_eq!(out_of(&back), plain, "gzip gives the log back whole");

    std::fs::remove_file(records(&env, "talk", "result.md")).unwrap();
    let r = result(&env, "talk");
    assert!(r.contains("log.ndjson.gz"), "{r}");
    assert!(r.contains("ran on .claude.a"), "the answer is read out of the compressed log: {r}");
    let json: serde_json::Value =
        serde_json::from_slice(&relay(&env, &["view", "result", "talk", "--json"]).stdout).unwrap();
    assert!(json["log"].as_str().unwrap().ends_with("talk.log.ndjson.gz"), "{json}");
}

#[test]
fn a_running_job_over_its_disk_ceiling_is_stopped_and_can_be_resumed() {
    if !have_bwrap() {
        return;
    }
    let env = setup("disk-stop", false);
    // 10 KB: any clone is over it.
    configure(&env, "job_disk_gb = 0.00001\n", "");
    claude_account(&env.home, "a", 10, false);
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    ok(&relay(&env, &["queue", "add", "grows", "--text", "Make a change."]), "queue");
    let j = wait_for(&env, "grows");
    assert_eq!(j["status"], "stopped", "{j}");
    let why = j["reason"].as_str().unwrap();
    assert!(why.starts_with("disk: clone ") && why.ends_with(" over 10K"), "{why}");
    assert!(j["disk_bytes"].as_u64().unwrap() > 10_000, "the size is on the record: {j}");
    assert!(workdir(&env, "grows").exists(), "the clone is kept for a resume");

    std::fs::remove_file(&hold).unwrap();
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    std::fs::write(env.repo.join("relay.toml"), conf.replace("job_disk_gb = 0.00001", "job_disk_gb = 40")).unwrap();
    ok(&relay(&env, &["queue", "resume", "grows", "--message", "Go on."]), "resume");
    assert_eq!(wait_for(&env, "grows")["status"], "done");
}

#[test]
fn a_disk_hook_may_let_a_job_go_on() {
    if !have_bwrap() {
        return;
    }
    let env = setup("disk-wait", false);
    let script = env.root.join("decide.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"action\": \"wait\", \"why\": \"plenty left\"}'\n")
        .unwrap();
    Command::new("chmod").arg("755").arg(&script).status().unwrap();
    configure(&env, "job_disk_gb = 0.00001\n", &format!("[hooks]\ndisk = [\"command:{}\"]\n", script.display()));
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "big", "--text", "Make a change."]), "queue");
    let j = wait_for(&env, "big");
    assert_eq!(j["status"], "done", "{j}");
    let d = &decisions(&env, "big")[0];
    assert_eq!((d["point"].as_str(), d["action"]["action"].as_str()), (Some("disk"), Some("wait")), "{d}");
}
