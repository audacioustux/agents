//! End to end, with stub harnesses in a real bubblewrap sandbox: quota
//! readings, account choice across harnesses, a limit that moves a session
//! (with its history on the same harness, afresh across harnesses), a build
//! job committing on its own branch, and landing that branch.

#![allow(clippy::disallowed_methods, reason = "a test reads its own switches and passes its PATH on by config")]

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
             [harness.claude]\nprogram = \"{}\"\n{omp_conf}\
             [session.env]\nPATH = {:?}\n{}",
            stub("claude"),
            // Where this machine keeps bwrap and the rest, and who commits, is config, never the
            // caller's: this runner's PATH, and the test identity (the sandbox hides the test home).
            std::env::var("PATH").unwrap(),
            identity().map(|(k, v)| format!("{k} = {v:?}\n")).concat(),
        ),
    )
    .unwrap();
    Env { root, home, repo }
}

fn relay(env: &Env, args: &[&str]) -> Output {
    relay_cmd(env, Command::new(env!("CARGO_BIN_EXE_relay")).args(args))
}

fn relay_cmd(env: &Env, cmd: &mut Command) -> Output {
    prepared(env, cmd).output().unwrap()
}

/// `cmd` set up as every test runs relay; settings applied after this win.
fn prepared<'c>(env: &Env, cmd: &'c mut Command) -> &'c mut Command {
    cmd.current_dir(&env.repo)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", env.root.join("state"))
        .env("XDG_DATA_HOME", env.root.join("data"))
        .env_remove("RELAY_CONFIG")
        .env_remove("RELAY_POLICY")
        .envs(env.policy().map(|p| ("RELAY_POLICY", p)))
        .env_remove("OMP_PROFILE")
        .envs(identity())
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

/// Whether the sandbox these tests run in is there. Without bubblewrap they fail rather than pass by
/// proving nothing; `RELAY_TEST_SKIP_SANDBOX=1` skips them on purpose, and says so.
fn have_bwrap() -> bool {
    if Command::new("bwrap").arg("--version").output().is_ok() {
        return true;
    }
    assert!(
        std::env::var_os("RELAY_TEST_SKIP_SANDBOX").is_some(),
        "bubblewrap is not installed, so this end-to-end test cannot run; install it, or set RELAY_TEST_SKIP_SANDBOX=1 to skip"
    );
    eprintln!("SKIPPED (RELAY_TEST_SKIP_SANDBOX): bubblewrap is not installed");
    false
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

/// Audit finding 1: relay wrote omp's prompt file in the job's session store, which the session can
/// write, and wrote it by path. A session that leaves a symlink there had the next attempt's brief (or
/// a resume message) written wherever it points. Now the prompt goes in the job's inbox, which the
/// session sees read-only, written through a held handle that never follows a symlink.
#[test]
fn a_symlink_a_session_plants_is_never_written_through() {
    if !have_bwrap() {
        return;
    }
    let env = setup("omp-plant", true);
    omp_default(&env.home, 20, false);
    let victim = env.home.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    ok(&relay(&env, &["queue", "add", "o", "--text", "[plant] Look.", "--readonly", "--harness", "omp"]), "queue");
    assert_eq!(wait_for(&env, "o")["status"], "done");
    let planted = records(&env, "o", "d/prompt.md");
    assert!(planted.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()), "the session planted its link");

    ok(&relay(&env, &["queue", "resume", "o", "--message", "overwrite it"]), "resume");
    let j = wait_for(&env, "o");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious", "relay wrote through the planted symlink");
    assert_eq!(j["status"], "done", "{j}");
    assert!(!env.home.join(".omp/planted-inbox").exists(), "the session could write where its prompt is");
}

/// The acceptance check's output and its `/tmp` were in the session's store too: a link planted there
/// had the host truncate and write the file it named, and bind the directory it named as the check's
/// writable `/tmp`. Now the check works in a directory of its own made fresh, and writes to the inbox.
#[test]
fn a_check_follows_nothing_the_session_planted() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar-plant", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "[hooks]\nmax_decisions = 1\n");
    claude_account(&env.home, "a", 10, false);
    let victim = env.home.join(".claude.a/victim");
    std::fs::write(&victim, "precious").unwrap();
    let check = "echo checked; touch /tmp/escaped";
    let j = wait_for_build(&env, "planted", &["--check", check], "[plant-check] ");
    assert_eq!(j["status"], "done", "{j}");
    assert!(env.home.join(".claude.a/planted-check").exists(), "the session planted its links");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious", "the check's output went through the link");
    assert!(!env.home.join("escaped").exists(), "the check's /tmp was the home the session linked to");
}

/// Audit finding 4: protected config an account does not have yet was not mounted at all, so its
/// session could create it: a `settings.json` with a hook, a `hooks/` script, or a new omp profile that
/// relay would then discover as an account. Each now gets a read-only placeholder.
#[test]
fn protected_config_an_account_lacks_cannot_be_created_by_its_session() {
    if !have_bwrap() {
        return;
    }
    let env = setup("plant-config", true);
    omp_default(&env.home, 20, false);
    claude_account(&env.home, "a", 50, false);
    let acct = env.home.join(".claude.a");
    assert!(!acct.join("settings.json").exists() && !acct.join("hooks").exists(), "the account has neither");
    ok(&relay(&env, &["probe"]), "probe");
    let text = "[plant-config] Look.";
    ok(&relay(&env, &["queue", "add", "c", "--text", text, "--readonly", "--harness", "claude"]), "queue");
    assert_eq!(wait_for(&env, "c")["status"], "done");
    let settings = std::fs::read_to_string(acct.join("settings.json")).unwrap_or_default();
    assert!(!settings.contains("hooks"), "the session wrote the account's settings.json: {settings}");
    assert!(!acct.join("hooks/pwn.sh").exists(), "the session wrote a hook");

    ok(&relay(&env, &["queue", "add", "o", "--text", "[plant-profile] Look.", "--readonly", "--harness", "omp"]), "q");
    assert_eq!(wait_for(&env, "o")["status"], "done");
    assert!(!env.home.join(".omp/profiles/evil").exists(), "the session made a profile");
    let quota = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(!quota.contains("omp:evil"), "{quota}");
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

#[test]
fn the_board_shows_every_project_with_everywhere_and_outside_any_project() {
    let env = setup("everywhere", false);
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    std::fs::write(env.repo.join("other.toml"), conf.replace("name = \"flow\"", "name = \"other\"")).unwrap();
    // No account is signed in, so both jobs stay queued.
    ok(&relay(&env, &["queue", "add", "ours", "--text", "our task"]), "flow queues");
    ok(&relay(&env, &["--config", "other.toml", "queue", "add", "theirs", "--text", "their task"]), "other queues");
    let text = board(&env);
    assert!(text.contains("our task") && !text.contains("their task"), "one project by default: {text}");
    let every = String::from_utf8_lossy(&relay(&env, &["view", "status", "--everywhere"]).stdout).into_owned();
    let row = |name: &str| every.lines().find(|l| l.contains(name)).unwrap_or_default().trim().to_owned();
    assert!(row("our task").starts_with("flow "), "each job with its project: {every}");
    assert!(row("their task").starts_with("other "), "{every}");
    let json = relay(&env, &["view", "status", "--everywhere", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(v["projects"]["other"]["jobs"]["theirs"].is_object(), "{v}");
    std::fs::remove_file(env.repo.join("relay.toml")).unwrap();
    let outside = relay(&env, &["view", "status"]);
    ok(&outside, "outside a project");
    let text = String::from_utf8_lossy(&outside.stdout).into_owned();
    assert!(text.contains("our task") && text.contains("their task"), "every project: {text}");
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
fn profiles_and_config_show_need_no_project() {
    let env = setup("profile-outside", false);
    std::fs::remove_file(env.repo.join("relay.toml")).unwrap();
    let out = relay(&env, &["profiles"]);
    ok(&out, "profiles outside a project, without a policy");
    assert!(String::from_utf8_lossy(&out.stdout).contains("no profiles"));
    env.write_policy(POLICY);
    let out = relay(&env, &["profiles", "--json"]);
    ok(&out, "profiles outside a project");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["profiles"]["hunt"]["model"], "claude-opus-5-5", "the policy's profile");
    assert_eq!(v["profiles"]["bad"]["banned"], "MiniMax is banned", "the policy's ban");
    let out = relay(&env, &["config", "show", "--json"]);
    ok(&out, "config show outside a project");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["project"].is_null(), "no project config was read: {v}");
    assert_eq!(v["profiles"]["hunt"]["model"]["from"], "policy");
    let text = String::from_utf8_lossy(&relay(&env, &["config", "show"]).stdout).into_owned();
    assert!(text.contains("project  none"), "{text}");
    let out = relay(&env, &["view", "events"]);
    assert!(!out.status.success(), "everything else still needs a project");
    assert!(stderr(&out).contains("no relay.toml here or above"), "{}", stderr(&out));
    ok(&relay(&env, &["view", "status"]), "the board outside a project shows every project");
    assert!(!env.root.join("state").exists(), "the policy views create no project state");
    let missing = env.root.join("missing.toml");
    let out = relay(&env, &["--config", missing.to_str().unwrap(), "profiles"]);
    assert!(!out.status.success(), "a named config that is missing still fails");
    let out = prepared(&env, Command::new(env!("CARGO_BIN_EXE_relay")).args(["config", "show"]))
        .env("RELAY_CONFIG", &missing)
        .output()
        .unwrap();
    assert!(!out.status.success(), "so does a missing $RELAY_CONFIG");
    env.write_policy("[session]\nbanned_models = { \"(\" = \"broken\" }\n");
    let out = relay(&env, &["profiles"]);
    assert!(!out.status.success(), "a broken policy is refused outside a project too");
    assert!(stderr(&out).contains("in policy"), "{}", stderr(&out));
}

#[test]
fn a_project_above_wins_over_the_policy_alone() {
    let env = setup("profile-above", false);
    env.write_policy(POLICY);
    let mut conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    conf.push_str("[profiles.hunt]\neffort = \"medium\"\n");
    std::fs::write(env.repo.join("relay.toml"), conf).unwrap();
    let sub = env.repo.join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    let out = prepared(&env, Command::new(env!("CARGO_BIN_EXE_relay")).args(["config", "show", "--json"]))
        .current_dir(&sub)
        .output()
        .unwrap();
    ok(&out, "config show from a subdirectory");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["profiles"]["hunt"]["effort"]["from"], "project", "the relay.toml above was read: {v}");
    assert!(v["project"].as_str().is_some_and(|p| p.ends_with("relay.toml")), "{v}");
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
    // `[session.env]` is already there (PATH): its variables go under that one header.
    let header = "[session.env]\n";
    let (extra, vars) = match extra.split_once(header) {
        Some((before, after)) => {
            let end = after.find("\n[").map_or(after.len(), |i| i + 1);
            (format!("{before}{}", &after[end..]), &after[..end])
        }
        None => (extra.to_owned(), ""),
    };
    let conf = conf.replace(header, &format!("{header}{vars}"));
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
    assert_eq!((n["parent"].as_str(), n["depth"].as_u64()), (Some("work"), Some(1)), "{n}");
    assert_eq!(asked(), 1, "the follow-up's clean done needed no agent");
}

#[test]
fn a_name_added_again_after_a_dequeue_starts_from_a_clean_slate() {
    if !have_bwrap() {
        return;
    }
    let env = setup("reuse", false);
    configure(&env, "job_ceiling_usd = 5.0\n", "[hooks]\nended = [\"rule\"]\n");
    claude_account(&env.home, "a", 10, false);
    // A held job keeps the name's first job queued behind it.
    std::fs::write(env.home.join(".claude.a/hold"), "").unwrap();
    ok(&relay(&env, &["queue", "add", "blocker", "--text", "[status-done] Hold the line."]), "blocker");
    until(&env, "blocker", "running");
    let prompt = records(&env, "reuse", "prompt.md");
    let mut want = String::new();
    for round in 1..=3 {
        ok(
            &relay(&env, &["queue", "add", "reuse", "--text", &format!("OLD brief {round}"), "--after", "blocker"]),
            "add",
        );
        assert_eq!(std::fs::read_to_string(&prompt).unwrap(), format!("OLD brief {round}"));
        ok(&relay(&env, &["queue", "dequeue", "reuse"]), "dequeue");
    }
    // What the dequeued job's name may have gathered besides its brief: a held-job decision and its log.
    for stale in ["decide.log", "decision.reuse-old.json", "runner.log"] {
        std::fs::write(records(&env, "reuse", stale), "OLD brief stale\n").unwrap();
    }
    want.push_str("[status-done] NEW brief");
    ok(&relay(&env, &["queue", "add", "reuse", "--text", &want]), "add again");
    assert_eq!(std::fs::read_to_string(&prompt).unwrap(), want, "the brief replaced the old one");
    for stale in ["decide.log", "decision.reuse-old.json", "runner.log"] {
        assert!(!records(&env, "reuse", stale).exists(), "{stale} of the old job is gone");
    }
    std::fs::remove_file(env.home.join(".claude.a/hold")).unwrap();
    assert_eq!(wait_for(&env, "reuse")["status"], "done");
    assert_eq!(std::fs::read_to_string(&prompt).unwrap(), want);
    let seen = prompts(&env, "a");
    assert!(seen.contains("NEW brief") && !seen.contains("OLD brief"), "the session got only the new brief: {seen}");
    let d = &decisions(&env, "reuse")[0];
    let file = std::fs::read_dir(records(&env, "reuse", "x").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("reuse.decision.") && n.contains(d["id"].as_str().unwrap())
        })
        .expect("the decision request is stored");
    let req: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(req["brief"], want.as_str(), "the decision brief equals the stored prompt");
    assert_eq!(wait_for(&env, "blocker")["status"], "done");
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
    let review = relay(&env, &["queue", "add", "peek", "--text", "x", "--fork", "base", "--readonly"]);
    assert_eq!(review.status.code(), Some(5), "a review never forks its builder: {}", stderr(&review));
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

/// The job's clone. A job is `running` a moment before its runner has made the clone and recorded it, so
/// this waits (bounded) for the record rather than racing it.
fn workdir(env: &Env, name: &str) -> PathBuf {
    for _ in 0..100 {
        if let Some(dir) = job(env, name)["workdir"].as_str() {
            return PathBuf::from(dir);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{name} has no clone: {}", job(env, name));
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
    assert_eq!(acted, ["old", "old"], "its build outputs, and its /tmp: {report}");
    let scratch = |n: &str| env.root.join("data/relay/flow/scratch").join(n);
    assert!(!scratch("old").exists(), "its scratch directory goes with the outputs");
    assert!(scratch("waited").exists() && scratch("busy").exists(), "a queued job waits for it, a running one is live");
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

/// Audit finding 2: gc reads a clone the session wrote, configuration and all, so it must not run
/// what that configuration names. The session plants a `core.fsmonitor`; gc's pass over its clone
/// (`git status` before it removes it) must not run it on the host.
#[test]
fn gc_never_runs_a_program_the_sessions_clone_names() {
    if !have_bwrap() {
        return;
    }
    let env = setup("gc-fsmonitor", false);
    configure(&env, "", "[gc]\nclones_after_days = 0\n");
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "planted", "--text", "[fsmonitor] Make a change."]), "queue");
    assert_eq!(wait_for(&env, "planted")["status"], "done");
    let dir = workdir(&env, "planted");
    let fsmonitor = Command::new("git").arg("-C").arg(&dir).args(["config", "core.fsmonitor"]).output().unwrap();
    assert!(out_of(&fsmonitor).contains("fsmonitor-ran"), "the session planted its fsmonitor");
    let ran = env.home.join(".claude.a/fsmonitor-ran");
    assert!(!ran.exists(), "nothing ran it before gc");

    let text = out_of(&relay(&env, &["land", "gc"]));
    assert!(text.contains("gc: removed") && !dir.exists(), "gc still checks and removes the clone: {text}");
    assert!(!ran.exists(), "gc ran the clone's core.fsmonitor on the host");
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

fn events(env: &Env) -> Vec<serde_json::Value> {
    let out = relay(env, &["view", "events"]);
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn prompts(env: &Env, account: &str) -> String {
    std::fs::read_to_string(env.home.join(format!(".claude.{account}/prompts"))).unwrap_or_default()
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The first handler's name and answer of each of the job's decisions, oldest first.
fn steps(env: &Env, name: &str) -> Vec<(String, String)> {
    let first = |d: &serde_json::Value, k: &str| d["steps"][0][k]["action"].as_str().unwrap_or("").to_owned();
    decisions(env, name)
        .iter()
        .map(|d| (d["steps"][0]["handler"].as_str().unwrap_or("").to_owned(), first(d, "action")))
        .collect()
}

/// Queues a build job whose brief says it is done, with `extra` flags, and waits for it to end.
fn build(env: &Env, name: &str, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["queue", "add", name, "--text", "[status-done] Make a change."];
    args.extend(extra);
    ok(&relay(env, &args), "queue");
    wait_for(env, name)
}

fn acts(pairs: &[(String, String)]) -> Vec<&str> {
    pairs.iter().map(|(_, a)| a.as_str()).collect()
}

#[test]
fn a_failed_check_resumes_the_session_with_its_output_until_it_passes() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar", false);
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        "[hooks]\nmax_decisions = 2\n[profiles.bar]\nharness = \"claude\"\ncheck = \"false\"\n",
    );
    claude_account(&env.home, "a", 10, false);

    // The stub writes the number of the turn into its output: the first run fails the check, and the
    // resume it is sent (with the failing output) writes turn 2, which passes.
    let check = "grep -Eq ' 2$' relay-output.txt || { echo 'expected the second turn' >&2; exit 3; }";
    let j = build(&env, "fix", &["--check", check]);
    assert_eq!(j["status"], "done", "{j}");
    let bars = j["bars"].as_array().unwrap();
    let results: Vec<&str> = bars.iter().filter_map(|b| b["result"].as_str()).collect();
    assert_eq!(results, ["fail", "pass"], "{j}");
    assert_eq!(
        (bars[0]["exit"].clone(), bars[0]["tail"].clone()),
        (3.into(), "".into()),
        "only the latest run keeps its tail"
    );
    assert_eq!(bars[1]["commit"], rev_parse(&workdir(&env, "fix"), "HEAD"), "the check is of the branch tip");
    assert_eq!(j["acceptance"]["check"], check);
    // The resumed turn reports no `status: done`, so the rule defers and the run stands: it is green.
    let s = steps(&env, "fix");
    assert_eq!(acts(&s), ["resume", "defer"], "{s:?}");
    let sent = prompts(&env, "a");
    assert!(
        sent.contains("acceptance check") && sent.contains("expected the second turn"),
        "the failing tail is the resume message: {sent}"
    );
    let ran: Vec<serde_json::Value> = events(&env).into_iter().filter(|e| e["type"] == "bar-checked").collect();
    assert_eq!(ran.len(), 2);
    assert_eq!(ran[0]["tail"].as_str().map(str::trim), Some("expected the second turn"));
    assert!(board(&env).contains("check pass"), "{}", board(&env));

    // The profile's check is the default for a build job, and a check that never passes ends the job
    // when `max_decisions` is spent: two resumes, then it is failed and says why.
    let j = build(&env, "stuck", &["--profile", "bar"]);
    assert_eq!((j["status"].as_str(), j["acceptance"]["check"].as_str()), (Some("failed"), Some("false")), "{j}");
    let reason = j["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("acceptance check fail") || reason.contains("decisions have acted"), "{reason}");
    assert_eq!(decisions(&env, "stuck").len(), 3, "two resumes and the one that was refused");
    assert_eq!(j["bars"].as_array().unwrap().len(), 3);
}

#[test]
fn a_check_judges_the_committed_tip_in_the_sandbox_without_the_account() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar-box", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "[hooks]\nmax_decisions = 1\n");
    claude_account(&env.home, "a", 10, false);

    // The session leaves a modified tracked file and an untracked one behind. The check sees neither:
    // it reads a clean checkout of the commit, which is the work that would land.
    let clean = "test ! -e untracked.txt && ! grep -q late relay-output.txt";
    let j = wait_for_build(&env, "tip", &["--check", clean], "[uncommitted] ");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["bars"][0]["result"], "pass");
    assert_eq!(j["bars"][0]["commit"], rev_parse(&workdir(&env, "tip"), "HEAD"), "it names the commit it read");
    assert!(workdir(&env, "tip").join("untracked.txt").exists(), "the live clone still has what the check did not see");
    let j = wait_for_build(&env, "wanted", &["--check", "test -e untracked.txt"], "[uncommitted] ");
    assert_eq!(j["status"], "failed", "work that was never committed is not there for the check: {j}");

    // The repository is read-only there, as it was for the session; and the session's account is not
    // there at all: its sign-in is neither readable nor writable by a check.
    let touch = format!("touch {}/escaped", env.repo.display());
    let j = build(&env, "boxed", &["--check", &touch]);
    assert_eq!(j["status"], "failed", "{j}");
    assert!(!env.repo.join("escaped").exists(), "the check wrote to the repository");
    assert!(
        j["bars"].as_array().unwrap().last().unwrap()["tail"]
            .as_str()
            .unwrap_or_default()
            .contains("Read-only file system"),
        "{j}"
    );
    let j = build(&env, "keys", &["--check", "test ! -e \"$HOME/.claude.a/.credentials.json\""]);
    assert_eq!(j["status"], "done", "a check cannot read the account: {j}");
    assert!(env.home.join(".claude.a/.credentials.json").exists());
}

/// Like `build`, with a marker in the brief for the stub.
fn wait_for_build(env: &Env, name: &str, extra: &[&str], marker: &str) -> serde_json::Value {
    let text = format!("{marker}[status-done] Make a change.");
    let mut args = vec!["queue", "add", name, "--text", &text];
    args.extend(extra);
    ok(&relay(env, &args), "queue");
    wait_for(env, name)
}

#[test]
fn a_revert_control_needs_the_check_to_fail_for_real_at_the_base_commit() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar-red", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "");
    claude_account(&env.home, "a", 10, false);

    // Red at the base commit (no output file yet), green on the branch: accepted.
    let j = build(&env, "real", &["--check", "test -f relay-output.txt", "--check-red-first"]);
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!((j["bars"][0]["result"].as_str(), j["bars"][0]["base_exit"].as_i64()), (Some("pass"), Some(1)), "{j}");

    // Green at the base commit too: it shows nothing about the change.
    let j = build(&env, "vacuous", &["--check", "true", "--check-red-first"]);
    assert_eq!(j["status"], "failed", "{j}");
    assert_eq!(
        (j["bars"][0]["result"].as_str(), j["bars"][0]["base_exit"].as_i64()),
        (Some("check-proves-nothing"), Some(0)),
        "{j}"
    );
    assert!(j["reason"].as_str().unwrap_or_default().starts_with("check-proves-nothing"), "{j}");
    assert_eq!(acts(&steps(&env, "vacuous")), ["stop"]);
    assert!(board(&env).contains("check check-proves-nothing"), "{}", board(&env));

    // A base run that only died (command not found: 127) is not a failing test: it proves nothing either way.
    let j = build(&env, "broken", &["--check", "test -f relay-output.txt || nosuchcommand-xyz", "--check-red-first"]);
    assert_eq!(j["status"], "failed", "{j}");
    assert_eq!(
        (j["bars"][0]["result"].as_str(), j["bars"][0]["base_exit"].as_i64()),
        (Some("check-could-not-run"), Some(127)),
        "{j}"
    );
    assert!(j["reason"].as_str().unwrap_or_default().starts_with("check-could-not-run"), "{j}");

    // Declaring a revert control without a check, or a check on a read-only job, is a usage error.
    let out = relay(&env, &["queue", "add", "red", "--text", "x", "--check-red-first"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = relay(&env, &["queue", "add", "ro", "--text", "x", "--readonly", "--check", "true"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn whoever_accepts_a_red_run_it_does_not_end_done() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar-accept", false);
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        "[hooks]\nended = [\"orchestrator\"]\norchestrator_timeout_minutes = 3\n",
    );
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "pushy", "--text", "[status-done] Make a change.", "--check", "false"]), "queue");
    let mut waiting = None;
    for _ in 0..100 {
        let v: serde_json::Value = serde_json::from_slice(&relay(&env, &["decide", "list", "--json"]).stdout).unwrap();
        if let Some(d) = v["pending"].as_array().and_then(|p| p.first()) {
            assert_eq!(d["request"]["check"]["result"], "fail", "the handler is told: {d}");
            waiting = d["id"].as_str().map(str::to_owned);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The refusal is the orchestrator's to hear: exit 5 with the reason, and the decision still waits.
    let id = waiting.expect("the ended decision is parked");
    let out = relay(&env, &["decide", "answer", &id, "--action", "accept"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("acceptance check is fail"), "{}", stderr(&out));
    ok(&relay(&env, &["decide", "answer", &id, "--action", "stop", "--why", "red"]), "stop instead");
    let j = wait_for(&env, "pushy");
    assert_eq!(j["status"], "failed", "{j}");
}

#[test]
fn a_handler_that_accepts_a_red_run_is_refused_and_the_chain_goes_on() {
    if !have_bwrap() {
        return;
    }
    let env = setup("bar-fall", false);
    let script = env.root.join("accept.sh");
    std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho '{\"action\": \"accept\", \"why\": \"looks fine\"}'\n")
        .unwrap();
    Command::new("chmod").arg("755").arg(&script).status().unwrap();
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        &format!("[hooks]\nended = [\"command:{}\", \"rule\"]\nmax_decisions = 2\n", script.display()),
    );
    claude_account(&env.home, "a", 10, false);
    let j = build(&env, "red", &["--check", "false"]);
    assert_eq!(j["status"], "failed", "{j}");
    let first = &decisions(&env, "red")[0];
    let (a, b) = (&first["steps"][0], &first["steps"][1]);
    assert_eq!(a["action"]["action"], "accept", "{first}");
    assert!(a["refused"].as_str().unwrap_or_default().contains("acceptance check is fail"), "told why: {first}");
    assert_eq!((b["handler"].as_str(), b["action"]["action"].as_str()), (Some("rule"), Some("resume")), "{first}");
}

#[test]
fn a_job_that_can_be_resumed_without_end_needs_a_ceiling_and_waits_on_a_gate() {
    let env = setup("gate", false);
    configure(
        &env,
        "",
        "[profiles.cheap]\nescalate_to = \"strong\"\nescalate_after = 2\n[profiles.strong]\nbudget = 1.0\n",
    );
    for args in [
        &["queue", "add", "a", "--text", "x", "--check", "true"][..],
        &["queue", "add", "b", "--text", "x", "--profile", "cheap"][..],
    ] {
        let out = relay(&env, args);
        assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
        assert!(stderr(&out).contains("job_ceiling_usd"), "{}", stderr(&out));
    }
    ok(&relay(&env, &["queue", "add", "plain", "--text", "x"]), "a job that declares nothing needs none");

    configure(&env, "job_ceiling_usd = 100.0\n", "");
    ok(&relay(&env, &["queue", "add", "checked", "--text", "x", "--check", "true"]), "queue");
    // A relay that does not know acceptance would see a dependency on a job that is not there, and
    // never start it; this one drops the gate when it folds, so nothing shows of it.
    let queued: Vec<serde_json::Value> = events(&env).into_iter().filter(|e| e["type"] == "queued").collect();
    let after = |name: &str| queued.iter().find(|e| e["job"] == name).unwrap()["after"].clone();
    assert_eq!(after("checked"), serde_json::json!(["needs-relay-acceptance"]));
    assert!(after("plain").is_null(), "a job that declares nothing carries no gate");
    assert_eq!(job(&env, "checked")["after"], serde_json::json!([]));
    assert!(job(&env, "needs-relay-acceptance").is_null());
    let out = relay(&env, &["queue", "add", "needs-relay-acceptance", "--text", "x"]);
    assert_eq!(out.status.code(), Some(2), "the name is relay's: {}", stderr(&out));
}

/// A build job and a review of it that rejects, on one account.
fn rejected(env: &Env) -> (String, String) {
    claude_account(&env.home, "a", 10, false);
    let b = build(env, "build", &["--model", "claude=claude-opus-5-5"]);
    assert_eq!(b["status"], "done", "{b}");
    let tip = rev_parse(&workdir(env, "build"), "HEAD");
    let out = relay(
        env,
        &[
            "queue",
            "add",
            "rev",
            "--review-of",
            "build",
            "--text",
            "[verdict-reject] Review the change.",
            "--model",
            "claude=claude-sonnet-5-5",
        ],
    );
    ok(&out, "queue the review");
    assert_eq!(wait_for(env, "rev")["status"], "done");
    (tip, rev_parse(&env.repo, "HEAD"))
}

#[test]
fn a_review_sees_the_builders_branch_and_a_reject_resumes_the_builder_with_the_findings() {
    if !have_bwrap() {
        return;
    }
    let env = setup("review", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "");
    claude_account(&env.home, "a", 10, false);
    build(&env, "build", &["--model", "claude=claude-opus-5-5"]);
    let tip = rev_parse(&workdir(&env, "build"), "HEAD");
    let base = rev_parse(&env.repo, "HEAD");

    // Sugar for read-only, after the build, with its branch fetched where the reviewer can read it.
    let out = relay(
        &env,
        &[
            "queue",
            "add",
            "rev",
            "--review-of",
            "build",
            "--text",
            "[verdict-reject] Review the change.",
            "--model",
            "claude=claude-sonnet-5-5",
        ],
    );
    ok(&out, "queue the review");
    let queued = job(&env, "rev");
    assert_eq!(
        (queued["readonly"].clone(), queued["kind"].clone(), queued["after"].clone()),
        (true.into(), "review".into(), serde_json::json!(["build"])),
        "{queued}"
    );
    assert_eq!(queued["acceptance"]["review-of"], "build");
    let j = wait_for(&env, "rev");
    assert_eq!((j["status"].as_str(), j["family"].as_str()), (Some("done"), Some("anthropic")), "{j}");
    assert_eq!(rev_parse(&env.repo, "refs/relay/review/rev"), tip, "the branch is in the repository's refs");
    let told = prompts(&env, "a");
    assert!(
        told.contains("refs/relay/review/rev") && told.contains(&tip) && told.contains(&base),
        "the ref and the diff base are in the preamble: {told}"
    );

    // A reject resumes the builder's session with the findings; the verdict is kept on the builder.
    let b = wait_for(&env, "build");
    assert_eq!((b["status"].as_str(), b["attempts"].as_u64()), (Some("done"), Some(2)), "{b}");
    let v = &b["verdicts"][0];
    assert_eq!(
        (v["reviewer"].as_str(), v["reason"].as_str(), v["commit"].as_str(), v["family"].as_str()),
        (Some("rev"), Some("test-failure"), Some(tip.as_str()), Some("anthropic")),
        "{b}"
    );
    let told = prompts(&env, "a");
    assert!(told.contains("rejected it (test-failure)") && told.contains("parse panics on empty input"), "{told}");
    let new_tip = rev_parse(&workdir(&env, "build"), "HEAD");
    assert_ne!(new_tip, tip);

    // The builder answered, so the reviewer judges the new tip (more of that in its own test).
    let b = wait_verdicts(&env, "build", 2);
    assert_eq!(wait_for(&env, "rev")["attempts"], 2, "the reviewer was resumed");
    let v = &b["verdicts"][1];
    assert_eq!(
        (v["reviewer"].as_str(), v["verdict"].as_str(), v["commit"].as_str()),
        (Some("rev"), Some("accept"), Some(new_tip.as_str())),
        "{b}"
    );
    assert_eq!(job(&env, "build")["attempts"], 2, "an accept resumes nothing: {b}");
    let out = relay(&env, &["land", "fetch", "build"]);
    assert!(!out_of(&out).contains("note:"), "{}", out_of(&out));
    assert!(board(&env).contains("review by rev: accept"), "{}", board(&env));
}

/// The job once it has `n` verdicts, bounded.
fn wait_verdicts(env: &Env, name: &str, n: usize) -> serde_json::Value {
    for _ in 0..150 {
        let j = job(env, name);
        if j["verdicts"].as_array().is_some_and(|v| v.len() >= n) {
            return j;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{name} never had {n} verdicts: {}", job(env, name));
}

#[test]
fn a_builder_that_answers_a_reject_is_judged_again_on_its_new_tip() {
    if !have_bwrap() {
        return;
    }
    let env = setup("review-resume", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "");
    let (tip, _) = rejected(&env);
    wait_for(&env, "build");
    let new_tip = rev_parse(&workdir(&env, "build"), "HEAD");
    assert_ne!(new_tip, tip, "the reject sent the builder back to work");

    // Nobody asks: the reviewer is resumed. Its ref is fetched again, and its prompt names the commit,
    // so a verdict cannot be for one it never saw.
    let b = wait_verdicts(&env, "build", 2);
    assert_eq!(wait_for(&env, "rev")["attempts"], 2);
    let v = &b["verdicts"][1];
    assert_eq!((v["verdict"].as_str(), v["commit"].as_str()), (Some("accept"), Some(new_tip.as_str())), "{b}");
    let told = prompts(&env, "a");
    assert!(told.contains(&format!("at commit {new_tip}")) && told.contains("Judge its branch again"), "{told}");
    assert_eq!(rev_parse(&env.repo, "refs/relay/review/rev"), new_tip);
    // The board shows the verdict for the latest reviewed commit, not the stale reject.
    let shown = board(&env);
    assert!(shown.contains("review by rev: accept") && !shown.contains("review by rev: reject"), "{shown}");
    assert_eq!(job(&env, "build")["attempts"], 2, "an accept resumes nothing");
    let out = relay(&env, &["land", "fetch", "build"]);
    assert!(!out_of(&out).contains("note:"), "{}", out_of(&out));

    // A second reviewer takes the first one's place in the chain; only it is shown.
    ok(
        &relay(&env, &["queue", "add", "rev2", "--review-of", "build", "--text", "[verdict-accept] Review it."]),
        "queue",
    );
    assert_eq!(wait_for(&env, "rev2")["status"], "done");
    let shown = board(&env);
    assert!(shown.contains("review by rev2: accept") && !shown.contains("review by rev:"), "{shown}");
    assert!(shown.contains("reviews build (superseded by rev2)"), "{shown}");

    // A person may still resume a review by hand.
    let seen = prompts(&env, "a").len();
    ok(&relay(&env, &["queue", "resume", "rev", "--message", "[verdict-accept] Look again."]), "resume");
    assert_eq!(wait_for(&env, "rev")["attempts"], 3);
    assert!(prompts(&env, "a")[seen..].contains(&format!("at commit {new_tip}")));

    // `land clean` takes a job's scratch directory with the rest of it.
    let scratch = env.root.join("data/relay/flow/scratch/rev2");
    assert!(scratch.exists());
    ok(&relay(&env, &["land", "clean", "rev2"]), "land clean");
    assert!(!scratch.exists(), "the scratch directory went with the job");

    // gc takes a review's ref once the review is over.
    configure(&env, "", "[gc]\noutputs_after_hours = 0\n");
    let out = relay(&env, &["land", "gc"]);
    ok(&out, "gc");
    assert!(out_of(&out).contains("removed refs/relay/review/rev"), "{}", out_of(&out));
    assert_eq!(rev_parse(&env.repo, "refs/relay/review/rev"), "");
}

/// A reviewer that keeps rejecting is asked again only while it can still record its verdict
/// (`hooks.max_decisions` caps the decisions on a job), so the loop stops and the board is the last verdict.
#[test]
fn a_reject_loop_stops_where_the_reviewer_can_no_longer_record_a_verdict() {
    if !have_bwrap() {
        return;
    }
    let env = setup("review-loop", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "[hooks]\nmax_decisions = 2\n");
    claude_account(&env.home, "a", 10, false);
    std::fs::write(env.home.join(".claude.a/rereview"), "reject").unwrap();
    let b = build(&env, "build", &["--model", "claude=claude-opus-5-5"]);
    assert_eq!(b["status"], "done");
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "rev",
                "--review-of",
                "build",
                "--text",
                "[verdict-reject] Review the change.",
                "--model",
                "claude=claude-sonnet-5-5",
            ],
        ),
        "queue the review",
    );
    // reject, builder again, reject again, builder a third time; then the reviewer has had its two
    // decisions, so it is not resumed on the third tip (its verdict would be dropped, leaving a stale one).
    let mut settled = None;
    for _ in 0..200 {
        let (b, r) = (job(&env, "build"), job(&env, "rev"));
        let idle = ["done", "failed"].contains(&b["status"].as_str().unwrap_or_default())
            && ["done", "failed"].contains(&r["status"].as_str().unwrap_or_default());
        if idle && b["attempts"] == 3 {
            settled = Some((b, r));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let (b, r) = settled.unwrap_or_else(|| panic!("never settled: {}", board(&env)));
    std::thread::sleep(Duration::from_secs(1));
    let (b2, r2) = (job(&env, "build"), job(&env, "rev"));
    assert_eq!(
        (b2["attempts"].clone(), r2["attempts"].clone()),
        (b["attempts"].clone(), r["attempts"].clone()),
        "it stopped: {}",
        board(&env)
    );
    assert_eq!((b2["attempts"].as_u64(), r2["attempts"].as_u64()), (Some(3), Some(2)), "{}", board(&env));
    let verdicts = b2["verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 2, "every run that could record a verdict did: {b2}");
    // The board shows the last recorded verdict, for the commit it judged.
    assert!(board(&env).contains("review by rev: reject: other"), "{}", board(&env));
    assert!(r2["reason"].as_str().unwrap_or_default().contains("build resumed"), "its last verdict was acted on: {r2}");
}

/// `hooks.max_decisions` bounds one chain's rejects of a builder; a replaced chain's verdicts left the board,
/// so the replacement counts from nothing (the job's ceiling still bounds what the builder may spend).
#[test]
fn a_replacement_chain_counts_its_rejects_from_nothing() {
    if !have_bwrap() {
        return;
    }
    let env = setup("review-bound", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "[hooks]\nmax_decisions = 1\n");
    rejected(&env);
    assert_eq!(wait_for(&env, "build")["attempts"], 2, "the first reject resumed it");
    ok(
        &relay(
            &env,
            &["queue", "add", "rev2", "--review-of", "build", "--text", "[verdict-reject] Review the change."],
        ),
        "queue",
    );
    let r = wait_for(&env, "rev2");
    assert!(r["reason"].as_str().unwrap_or_default().contains("build resumed"), "{r}");
    let b = wait_for(&env, "build");
    assert_eq!(b["attempts"], 3, "the new chain's first reject sent the builder back: {b}");
    let verdicts = b["verdicts"].as_array().unwrap();
    assert!(verdicts.iter().all(|v| v["reviewer"] == "rev2"), "only the live chain is on the board: {b}");
}

#[test]
fn a_ceiling_stops_a_reject_from_resuming_a_builder() {
    if !have_bwrap() {
        return;
    }
    let env = setup("review-ceiling", false);
    // One run costs the stub 0.02 and a builder's cap is $10: another run would pass this ceiling.
    configure(&env, "job_ceiling_usd = 5.0\n", "");
    claude_account(&env.home, "a", 10, false);
    build(&env, "build", &[]);
    ok(
        &relay(&env, &["queue", "add", "rev", "--review-of", "build", "--text", "[verdict-reject] Review the change."]),
        "queue",
    );
    let r = wait_for(&env, "rev");
    assert!(r["reason"].as_str().unwrap_or_default().contains("ceiling"), "{r}");
    assert_eq!(job(&env, "build")["attempts"], 1);
}

#[test]
fn a_profile_that_asks_for_an_independent_family_refuses_a_same_family_reviewer() {
    if !have_bwrap() {
        return;
    }
    let env = setup("family", true);
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        "[profiles.careful]\nharness = \"claude\"\nmodel = \"claude-opus-5-5\"\nindependent = \"family\"\n\
         [profiles.loose]\nharness = \"claude\"\nmodel = \"claude-opus-5-5\"\n",
    );
    claude_account(&env.home, "a", 10, false);
    build(&env, "built", &["--profile", "careful"]);
    build(&env, "easy", &["--profile", "loose"]);

    // Same family as the builder (opus and sonnet are both Anthropic's): refused, and not recorded.
    let out = relay(
        &env,
        &[
            "queue",
            "add",
            "same",
            "--review-of",
            "built",
            "--text",
            "x",
            "--harness",
            "claude",
            "--model",
            "claude-sonnet-5-5",
        ],
    );
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("anthropic") && stderr(&out).contains("independent"), "{}", stderr(&out));
    assert!(job(&env, "same").is_null());

    // Another family is allowed, and so is the same one for a profile that declares nothing.
    ok(
        &relay(&env, &["queue", "add", "other", "--review-of", "built", "--text", "x", "--harness", "omp"]),
        "another family",
    );
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "free",
                "--review-of",
                "easy",
                "--text",
                "x",
                "--harness",
                "claude",
                "--model",
                "claude-sonnet-5-5",
            ],
        ),
        "no independence declared",
    );
    ok(&relay(&env, &["queue", "dequeue", "other"]), "dequeue");
    let free = wait_for(&env, "free");
    assert_eq!(
        (free["status"].as_str(), free["family"].as_str()),
        (Some("done"), Some("anthropic")),
        "recorded even where it need not differ: {free}"
    );

    // Where the reviewer might run on another family the queue cannot know; a session that starts on
    // the same one fails at once and says why, spending nothing.
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "late",
                "--review-of",
                "built",
                "--text",
                "x",
                "--harness",
                "claude,omp",
                "--model",
                "claude=claude-sonnet-5-5",
            ],
        ),
        "queue",
    );
    let late = wait_for(&env, "late");
    assert_eq!(late["status"], "failed", "{late}");
    assert!(late["reason"].as_str().unwrap_or_default().contains("independent review refused"), "{late}");
    assert_eq!(late["cost_usd"], 0.0);
}

#[test]
fn stats_fold_the_events_per_profile_and_model_and_show_labels_only() {
    if !have_bwrap() {
        return;
    }
    let env = setup("stats", false);
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        "[hooks]\nmax_decisions = 2\n[profiles.worker]\nharness = \"claude\"\nmodel = \"claude-haiku-5-5\"\n",
    );
    claude_account(&env.home, "a", 10, false);
    build(&env, "one", &["--profile", "worker", "--check", "test -f relay-output.txt"]);
    build(&env, "two", &["--profile", "worker", "--check", "false"]);
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "look",
                "--review-of",
                "one",
                "--text",
                "[verdict-accept] Review it.",
                "--model",
                "claude=claude-sonnet-5-5",
            ],
        ),
        "queue",
    );
    wait_for(&env, "look");

    let out = relay(&env, &["view", "stats", "--json"]);
    ok(&out, "stats");
    assert!(!out_of(&out).contains(SECRET_EMAIL), "{}", out_of(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["project"], "flow");
    let worker = v["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["profile"] == "worker")
        .expect("a group for the profile")
        .clone();
    assert_eq!(worker["model"], "claude-haiku-5-5");
    assert_eq!(
        (worker["jobs"]["total"].clone(), worker["jobs"]["done"].clone(), worker["jobs"]["failed"].clone()),
        (2.into(), 1.into(), 1.into()),
        "{worker}"
    );
    assert_eq!(
        (worker["checks"]["pass"].clone(), worker["checks"]["fail"].clone()),
        (1.into(), 3.into()),
        "two's check failed, and again after each of its two resumes: {worker}"
    );
    assert_eq!(worker["reviews"]["accept"], 1, "a review counts for the job it judged: {worker}");
    assert_eq!(worker["decisions"]["resume"], 2, "{worker}");
    assert_eq!((worker["resumes"].clone(), worker["retries"].clone()), (2.into(), 2.into()), "{worker}");
    assert!(worker["spend_usd"].as_f64().unwrap() > 0.0);
    assert!(worker["queue_wait_median_s"].is_number() && worker["run_median_s"].is_number(), "{worker}");
    assert_eq!(v["total"]["jobs"]["total"], 3);
    let accounts = v["accounts"].as_array().unwrap();
    assert_eq!((accounts.len(), accounts[0]["account"].as_str()), (1, Some("claude:a")));
    assert!((accounts[0]["run_share"].as_f64().unwrap() - 1.0).abs() < 1e-9);

    // The window counts jobs by when they were queued.
    let windowed: serde_json::Value =
        serde_json::from_slice(&relay(&env, &["view", "stats", "--json", "--since", "1h"]).stdout).unwrap();
    assert_eq!(windowed["total"]["jobs"]["total"], 3);
    let text = out_of(&relay(&env, &["view", "stats"]));
    assert!(
        text.contains("worker")
            && text.contains("claude:a")
            && text.contains("checks:")
            && !text.contains(SECRET_EMAIL),
        "{text}"
    );
    assert_eq!(relay(&env, &["view", "stats", "--since", "soon"]).status.code(), Some(2));
}

#[test]
fn a_profile_escalates_a_job_to_another_after_enough_failed_ends() {
    if !have_bwrap() {
        return;
    }
    let env = setup("escalate", false);
    configure(
        &env,
        "job_ceiling_usd = 100.0\n",
        "[profiles.cheap]\nharness = \"claude\"\nmodel = \"claude-haiku-5-5\"\nescalate_to = \"strong\"\nescalate_after = 2\n\
         [profiles.strong]\nharness = \"claude\"\nmodel = \"claude-opus-5-5\"\n",
    );
    claude_account(&env.home, "a", 10, false);
    std::fs::write(env.home.join(".claude.a/fail"), "").unwrap();
    ok(&relay(&env, &["queue", "add", "hard", "--text", "Make a change.", "--profile", "cheap"]), "queue");
    let j = wait_for(&env, "hard");
    assert_eq!(
        (j["status"].as_str(), j["profile"].as_str(), j["failed_ends"].as_u64()),
        (Some("failed"), Some("cheap"), Some(1)),
        "one failed end is not enough: {j}"
    );

    // The second failed end reaches the profile's number: the rule requeues it under the other.
    ok(&relay(&env, &["queue", "resume", "hard", "--message", "try again"]), "resume");
    let mut j = job(&env, "hard");
    for _ in 0..300 {
        j = job(&env, "hard");
        if j["profile"] == "strong" && j["status"] == "failed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!((j["profile"].as_str(), j["status"].as_str()), (Some("strong"), Some("failed")), "{j}");
    assert_eq!(
        (j["models"]["claude"].clone(), j["failed_ends"].clone(), j["attempts"].clone()),
        ("claude-opus-5-5".into(), 1.into(), 3.into()),
        "the count starts afresh under the new profile: {j}"
    );
    let ds = decisions(&env, "hard");
    assert_eq!(acts(&steps(&env, "hard")), ["defer", "requeue"], "{ds:?}");
    assert!(ds[1]["steps"][0]["why"].as_str().unwrap_or_default().contains("escalates to strong"));

    // A target the config cannot follow is an error at load, never at the moment of failure.
    env.write_policy("[profiles.strong]\nescalate_to = \"cheap\"\nescalate_after = 1\n");
    assert!(stderr(&relay(&env, &["profiles"])).contains("escalation cycle"));
    env.write_policy("[profiles.strong]\nescalate_to = \"nobody\"\nescalate_after = 1\n");
    assert!(stderr(&relay(&env, &["profiles"])).contains("not a profile"));
}

/// An omp default whose one meter stands at `pct` and resets `resets` from now (`10h`, `2d`).
fn omp_meter(home: &Path, pct: u32, resets: &str) {
    omp_default(home, 0, false);
    let usage =
        format!("Usage\n\nStub — 1 account\n  ● someone\n      ● 7 days  ██░░  {pct}.0% used · resets in {resets}\n");
    std::fs::write(home.join(".omp/usage"), usage).unwrap();
}

/// What the dispatcher logged, where it says why a job is held.
fn dispatch_log(env: &Env) -> String {
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "dispatch.log") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(&env.root.join("state"), &mut out);
    out
}

#[test]
fn a_meter_about_to_reset_has_the_looser_floor_and_quota_says_so() {
    if !have_bwrap() {
        return;
    }
    let near = "floor_near_reset = { within_hours = 12, floor = 0.98 }\n";
    let stated = |env: &Env| {
        ok(&relay(env, &["quota", "refresh"]), "refresh");
        String::from_utf8_lossy(&relay(env, &["quota", "show"]).stdout).into_owned()
    };

    // 91% of a meter that resets in 10h: under the near-reset floor, so the job runs.
    let env = setup("near-ok", true);
    configure(&env, near, "");
    omp_meter(&env.home, 91, "10h");
    let table = stated(&env);
    assert!(table.contains("floor 90%, 98% within 12h of a reset"), "{table}");
    assert!(table.contains("ready") && table.contains("(floor 98%, resets in 10h)"), "{table}");
    let json: serde_json::Value = serde_json::from_slice(&relay(&env, &["quota", "show", "--json"]).stdout).unwrap();
    assert_eq!(json["accounts"][0]["floor"], 0.98, "{json}");
    assert_eq!(json["accounts"][0]["floorWhy"], "floor 98%, resets in 10h", "{json}");
    ok(&relay(&env, &["queue", "add", "use-it", "--text", "Look.", "--readonly"]), "queue");
    assert_eq!(wait_for(&env, "use-it")["status"], "done");

    // Past the near-reset floor it is held, and the reason names that floor.
    let env = setup("near-full", true);
    configure(&env, near, "");
    omp_meter(&env.home, 99, "10h");
    ok(&relay(&env, &["queue", "add", "full", "--text", "Look.", "--readonly"]), "queue");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "full")["status"], "queued");
    let held = dispatch_log(&env);
    assert!(held.contains("at floor: 99%") && held.contains("(floor 98%, resets in 10h)"), "{held}");

    // The same 91% with the reset two days off is held at the ordinary floor, and says nothing of another.
    let env = setup("near-far", true);
    configure(&env, near, "");
    omp_meter(&env.home, 91, "2d");
    ok(&relay(&env, &["queue", "add", "far", "--text", "Look.", "--readonly"]), "queue");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "far")["status"], "queued");
    let held = dispatch_log(&env);
    assert!(held.contains("at floor: 91%") && !held.contains("floor 98%"), "{held}");

    // Without the setting, nothing changes: 91% is held however soon it resets.
    let env = setup("near-off", true);
    omp_meter(&env.home, 91, "10h");
    ok(&relay(&env, &["queue", "add", "off", "--text", "Look.", "--readonly"]), "queue");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(job(&env, "off")["status"], "queued");
    assert!(!stated(&env).contains("within"), "no mention of a policy that is not set");

    // The floor is validated: above limits.floor, at most 1.
    for bad in [
        "{ within_hours = 12, floor = 0.9 }",
        "{ within_hours = 12, floor = 1.1 }",
        "{ within_hours = 0, floor = 0.95 }",
    ] {
        let env = setup("near-bad", true);
        configure(&env, &format!("floor_near_reset = {bad}\n"), "");
        let out = relay(&env, &["quota", "show"]);
        assert!(!out.status.success() && stderr(&out).contains("floor_near_reset"), "{bad}: {}", stderr(&out));
    }
}

#[test]
fn a_checkpoint_reads_the_worktree_without_running_the_clones_filters() {
    if !have_bwrap() {
        return;
    }
    // The worker's clone is its own to write: a filter driver in its configuration and attributes
    // must not run on the host when relay reads the tree (`git add -A`) for the end-of-run evidence.
    let env = setup("filter", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "[hooks]\nended = [\"rule\"]\n");
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "work", "--text", "[status-done] [filter] Make a change."]), "queue");
    let j = wait_for(&env, "work");
    assert_eq!(j["status"], "done", "{j}");
    let prompt = prompts(&env, "a");
    assert!(prompt.contains("[filter]"), "the stub saw its flag: {prompt}");
    let marker = env.home.join(".claude.a/filter-ran");
    assert!(!marker.exists(), "a filter from the clone's configuration ran on the host");
}

/// What `stat -f` says the filesystem under `path` is, as seen from the host.
fn fs_type(path: &Path) -> String {
    let out = Command::new("stat").args(["-f", "-c", "%T"]).arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn a_jobs_tmp_is_a_directory_on_disk_that_is_counted_and_collected_and_hidden_directories_are_read_only() {
    if !have_bwrap() {
        return;
    }
    let env = setup("tmp", false);
    configure(&env, "disk_sample_minutes = 0.02\n", "[sandbox]\nshm_mb = 4\n");
    claude_account(&env.home, "a", 10, false);
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    ok(&relay(&env, &["queue", "add", "look", "--text", "[tmpcheck] Look around.", "--readonly"]), "queue");

    // The scratch directory is on disk, under the project's data directory, and what the job wrote under
    // /tmp is in it, so the job's disk use and the board's size column count it.
    let scratch = env.root.join("data/relay/flow/scratch/look");
    let fill = scratch.join("relay-fill");
    for _ in 0..200 {
        if fill.exists() && job(&env, "look")["disk_bytes"].as_u64().unwrap_or(0) >= 8_000_000 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::metadata(&fill).map_or(0, |m| m.len()), 8 * 1024 * 1024, "the write landed on the host");
    let j = job(&env, "look");
    assert!(j["disk_bytes"].as_u64().unwrap_or(0) >= 8_000_000, "the job's disk use counts /tmp: {j}");
    assert!(board(&env).contains("8."), "the size column shows it: {}", board(&env));
    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "look")["status"], "done");

    let said = |f: &str| std::fs::read_to_string(env.home.join(".claude.a").join(f)).unwrap_or_default();
    assert_eq!(said("tmp-fs").trim(), fs_type(&scratch), "inside, /tmp is the filesystem the scratch directory is on");
    if fs_type(&scratch) != "tmpfs" {
        assert_ne!(said("tmp-fs").trim(), "tmpfs");
    }
    assert_eq!(said("hidden-write").trim(), "refused", "a hidden directory is read-only, not RAM");
    assert!(!env.root.join("state/probe-write").exists());
    assert_eq!(said("shm-write").trim(), "wrote", "/dev/shm takes a small write");
    assert_eq!(said("shm-over").trim(), "refused", "/dev/shm holds no more than sandbox.shm_mb (4 MiB here)");

    // gc takes the directory of a job that has ended, with the clone's build outputs, and says so.
    configure(&env, "", "[gc]\noutputs_after_hours = 0\n");
    let out = relay(&env, &["land", "gc"]);
    ok(&out, "gc");
    assert!(out_of(&out).contains("scratch/look"), "{}", out_of(&out));
    assert!(!scratch.exists(), "the scratch directory is gone");
}

/// Growth on, and a checker on omp: with no omp account signed in, the checker has no seat to take.
fn checker_on_omp(env: &Env, wait_minutes: f64) {
    configure(
        env,
        "budget_growth = 2.0\njob_ceiling_usd = 1.0\nchecker = { harness = \"omp\", budget_usd = 0.5 }\n",
        &format!("[hooks]\nseat_wait_minutes = {wait_minutes}\n"),
    );
}

#[test]
fn a_run_that_ended_done_over_its_budget_ends_done_with_no_checker_seat_and_its_review_starts() {
    if !have_bwrap() {
        return;
    }
    let env = setup("overran", false);
    checker_on_omp(&env, 0.02);
    claude_account(&env.home, "a", 10, false);
    // $0.02 against a $0.015 cap, after its FINAL said `status: done`: there is nothing left to authorize.
    ok(
        &relay(&env, &["queue", "add", "build", "--text", "[status-done] Make a change.", "--budget", "0.015"]),
        "queue",
    );
    let out =
        relay(&env, &["queue", "add", "rev", "--review-of", "build", "--text", "[verdict-accept] Review the change."]);
    ok(&out, "queue the review");
    let j = wait_for(&env, "build");
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["attempts"], 1, "{j}");
    assert_eq!(j["checkpoints"].as_array().map(Vec::len), Some(0), "no checkpoint was asked: {j}");
    assert_eq!(j["overran"], serde_json::json!([0.02, 0.015]), "{j}");
    let overran: Vec<_> = events(&env).into_iter().filter(|e| e["type"] == "overran").collect();
    assert_eq!(overran.len(), 1, "{overran:?}");
    assert_eq!((overran[0]["job"].as_str(), overran[0]["spent_usd"].as_f64()), (Some("build"), Some(0.02)));
    assert!(board(&env).contains("over budget: $0.02"), "{}", board(&env));
    assert!(result(&env, "build").contains("over budget: spent $0.02"), "{}", result(&env, "build"));
    // The chained review waited on a done job, and now runs.
    let r = wait_for(&env, "rev");
    assert_eq!(r["status"], "done", "{r}");
    assert!(job(&env, "build")["verdicts"][0]["verdict"] == "accept", "{}", job(&env, "build"));
}

#[test]
fn a_checkpoint_that_needs_a_checker_waits_for_a_seat_and_goes_on_when_one_frees() {
    if !have_bwrap() {
        return;
    }
    let env = setup("seat-wait", true);
    checker_on_omp(&env, 2.0);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "grow", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    until(&env, "grow", "checking");
    // No omp account is signed in yet: the job waits at its checkpoint, not failed.
    std::thread::sleep(Duration::from_secs(5));
    let j = job(&env, "grow");
    assert_eq!(j["status"], "checking", "still waiting for a seat: {j}");
    assert_eq!(j["checkpoints"].as_array().map(Vec::len), Some(0), "{j}");
    // A seat appears (its reading is taken for the wait), and the check goes on.
    omp_default(&env.home, 20, false);
    let j = wait_for(&env, "grow");
    assert_eq!(j["status"], "done", "{j}");
    let c = &j["checkpoints"][0];
    assert_eq!((c["verdict"].as_str(), c["checker"].as_str()), (Some("continue"), Some("omp:default")), "{j}");
    assert_eq!(std::fs::read_to_string(env.home.join(".omp/checks")).unwrap_or_default().lines().count(), 1);
}

#[test]
fn a_checkpoint_that_waits_in_vain_for_a_seat_stops_and_names_the_bound() {
    if !have_bwrap() {
        return;
    }
    let env = setup("seat-timeout", false);
    checker_on_omp(&env, 0.03);
    claude_account(&env.home, "a", 10, false);
    ok(&relay(&env, &["queue", "add", "grow", "--text", "Make a change.", "--budget", "0.015"]), "queue");
    let j = wait_for(&env, "grow");
    assert_eq!(j["status"], "failed", "{j}");
    let c = &j["checkpoints"][0];
    assert_eq!(c["verdict"], "stop", "{j}");
    let why = c["why"].as_str().unwrap_or_default();
    assert!(why.contains("hooks.seat_wait_minutes = 0.03"), "the reason names the bound: {why}");
    assert!(j["reason"].as_str().unwrap_or_default().contains("hooks.seat_wait_minutes"), "{j}");
}

/// Builder `build` on account a, then its review `rev` on account b that rejects; the builder is held
/// (account a's `hold`) once the reject resumes it, so the chain can be changed while it works.
/// Returns the hold file of account a.
fn rejected_while_builder_is_held(env: &Env) -> PathBuf {
    configure(env, "job_ceiling_usd = 100.0\n", "");
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 10, false);
    // A re-review that is not told otherwise rejects again: what the old reviewer would do.
    std::fs::write(env.home.join(".claude.b/rereview"), "reject").unwrap();
    let b = build(env, "build", &["--account", "claude:a"]);
    assert_eq!(b["status"], "done", "{b}");
    let hold = env.home.join(".claude.a/hold");
    std::fs::write(&hold, "").unwrap();
    ok(
        &relay(
            env,
            &[
                "queue",
                "add",
                "rev",
                "--review-of",
                "build",
                "--account",
                "claude:b",
                "--text",
                "[verdict-reject] Old brief.",
            ],
        ),
        "queue the review",
    );
    assert_eq!(wait_for(env, "rev")["status"], "done");
    wait_status(env, "build", "running");
    hold
}

/// The job once its status is `status`, bounded.
fn wait_status(env: &Env, name: &str, status: &str) -> serde_json::Value {
    for _ in 0..150 {
        let j = job(env, name);
        if j["status"] == status {
            return j;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{name} never was {status}: {}", job(env, name));
}

#[test]
fn a_new_review_replaces_the_chain_and_only_it_re_reviews() {
    if !have_bwrap() {
        return;
    }
    let env = setup("chain-replace", false);
    let hold = rejected_while_builder_is_held(&env);
    assert_eq!(job(&env, "build")["attempts"], 2, "the reject resumed the builder");

    // The orchestrator settles the points itself and chains a reviewer with the brief it wants.
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "rev2",
                "--review-of",
                "build",
                "--account",
                "claude:b",
                "--text",
                "[verdict-accept] New brief.",
            ],
        ),
        "queue the replacement",
    );
    let old = job(&env, "rev");
    assert_eq!(old["unchained"]["by"], "rev2", "the board says who replaced it: {old}");
    assert!(board(&env).contains("reviews build (superseded by rev2)"), "{}", board(&env));
    assert_eq!(job(&env, "build")["verdicts"], serde_json::json!([]), "the old chain's verdicts left the board");

    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "build")["status"], "done");
    let r2 = wait_for(&env, "rev2");
    assert_eq!(r2["status"], "done", "{r2}");
    std::thread::sleep(Duration::from_secs(2));

    // The old reviewer would reject the new head again on its old brief and resume the builder; it did not.
    let (rev, build_now) = (job(&env, "rev"), job(&env, "build"));
    assert_eq!((rev["attempts"].as_u64(), build_now["attempts"].as_u64()), (Some(1), Some(2)), "{}", board(&env));
    let verdicts = build_now["verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 1, "{build_now}");
    assert_eq!((verdicts[0]["reviewer"].as_str(), verdicts[0]["verdict"].as_str()), (Some("rev2"), Some("accept")));
    assert_eq!(verdicts[0]["commit"].as_str(), Some(rev_parse(&workdir(&env, "build"), "HEAD").as_str()));
    // Its past verdict is still in the event log.
    let logged = events(&env);
    assert!(
        logged.iter().any(|e| e["type"] == "reviewed" && e["job"] == "rev" && e["verdict"] == "reject"),
        "the log keeps what the old reviewer said"
    );
    assert!(logged.iter().any(|e| e["type"] == "unchained" && e["job"] == "rev" && e["by"] == "rev2"));
}

#[test]
fn a_review_brief_changes_what_the_re_review_is_told() {
    if !have_bwrap() {
        return;
    }
    let env = setup("chain-brief", false);
    let hold = rejected_while_builder_is_held(&env);

    // As the orchestrator did: stop the builder, and resume it by hand with its own decision.
    ok(&relay(&env, &["queue", "stop", "build"]), "stop");
    wait_status(&env, "build", "stopped");
    std::fs::remove_file(&hold).unwrap();
    let brief = env.root.join("review-brief.md");
    std::fs::write(&brief, "[verdict-accept] Judge only the migration; the empty-input panic is settled.").unwrap();
    let seen = prompts(&env, "b").len();
    let out = relay(
        &env,
        &[
            "queue",
            "resume",
            "build",
            "--message",
            "Decision: the empty-input panic is accepted.",
            "--review-brief",
            brief.to_str().unwrap(),
        ],
    );
    ok(&out, "resume with a review brief");
    assert_eq!(wait_for(&env, "build")["status"], "done");
    let b = wait_verdicts(&env, "build", 2);
    assert_eq!(wait_for(&env, "rev")["attempts"], 2, "the chained reviewer judged the new head");
    let told = prompts(&env, "b")[seen..].to_owned();
    assert!(
        told.contains("Your brief for this review has been replaced") && told.contains("Judge only the migration"),
        "{told}"
    );
    // The old brief would have rejected again (the account's re-review answer); the new one accepts.
    assert_eq!(b["verdicts"][1]["verdict"], "accept", "{b}");
    assert_eq!(job(&env, "build")["attempts"], 3);
    let on_file = std::fs::read_to_string(env.root.join("state/relay/flow/jobs/rev.prompt.md")).unwrap();
    assert!(on_file.contains("Judge only the migration"), "a reviewer started afresh reads the new brief: {on_file}");

    // Nothing chained: nothing to rebrief.
    let out = relay(&env, &["queue", "resume", "rev", "--message", "x", "--review-brief", brief.to_str().unwrap()]);
    assert!(!out.status.success(), "a review has no review chained to it: {}", stderr(&out));
}

#[test]
fn unreview_detaches_the_chain_and_nothing_re_reviews() {
    if !have_bwrap() {
        return;
    }
    let env = setup("chain-unreview", false);
    let hold = rejected_while_builder_is_held(&env);
    ok(&relay(&env, &["queue", "unreview", "build"]), "unreview");
    assert_eq!(job(&env, "rev")["unchained"], serde_json::json!({ "by": null }));
    assert!(board(&env).contains("reviews build (detached)"), "{}", board(&env));
    let out = relay(&env, &["queue", "unreview", "build"]);
    assert_eq!(out.status.code(), Some(4), "nothing left to detach: {}", stderr(&out));

    std::fs::remove_file(&hold).unwrap();
    assert_eq!(wait_for(&env, "build")["status"], "done");
    std::thread::sleep(Duration::from_secs(2));
    let (rev, b) = (job(&env, "rev"), job(&env, "build"));
    assert_eq!((rev["attempts"].as_u64(), b["attempts"].as_u64()), (Some(1), Some(2)), "{}", board(&env));
    assert_eq!(b["verdicts"], serde_json::json!([]));
    let out = relay(&env, &["land", "fetch", "build"]);
    assert!(!out_of(&out).contains("note:"), "nobody reviews it now: {}", out_of(&out));
}

#[test]
fn a_head_that_moves_during_a_review_makes_its_verdict_stale_and_the_new_head_is_reviewed() {
    if !have_bwrap() {
        return;
    }
    let env = setup("chain-stale", false);
    configure(&env, "job_ceiling_usd = 100.0\n", "");
    claude_account(&env.home, "a", 10, false);
    claude_account(&env.home, "b", 10, false);
    let b = build(&env, "build", &["--account", "claude:a"]);
    assert_eq!(b["status"], "done", "{b}");
    let old_head = rev_parse(&workdir(&env, "build"), "HEAD");

    let hold = env.home.join(".claude.b/hold");
    std::fs::write(&hold, "").unwrap();
    ok(
        &relay(
            &env,
            &[
                "queue",
                "add",
                "rev",
                "--review-of",
                "build",
                "--account",
                "claude:b",
                "--text",
                "[verdict-reject] Review.",
            ],
        ),
        "queue the review",
    );
    wait_status(&env, "rev", "running");
    // The builder is worked on while the review reads the old head.
    ok(&relay(&env, &["queue", "resume", "build", "--message", "More work."]), "resume");
    assert_eq!(wait_for(&env, "build")["status"], "done");
    let new_head = rev_parse(&workdir(&env, "build"), "HEAD");
    assert_ne!(new_head, old_head);
    std::fs::remove_file(&hold).unwrap();

    let b = wait_verdicts(&env, "build", 2);
    let (first, second) = (&b["verdicts"][0], &b["verdicts"][1]);
    assert_eq!(
        (first["verdict"].as_str(), first["commit"].as_str(), first["stale_head"].as_str()),
        (Some("reject"), Some(old_head.as_str()), Some(new_head.as_str())),
        "the verdict is recorded, as stale: {b}"
    );
    assert_eq!(
        (second["verdict"].as_str(), second["commit"].as_str(), second["stale_head"].as_str()),
        (Some("accept"), Some(new_head.as_str()), None),
        "the chain reviewed the new head: {b}"
    );
    assert_eq!(wait_for(&env, "rev")["attempts"], 2);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(job(&env, "build")["attempts"], 2, "a stale reject resumes nobody: {}", board(&env));
    assert!(prompts(&env, "b").contains("moved while you were judging it"), "{}", prompts(&env, "b"));
    let r = events(&env).into_iter().find(|e| e["type"] == "reviewed").unwrap();
    assert_eq!(r["stale_head"], new_head, "{r}");
}

/// A caller's `PATH`: `dirs`, then the system's own (git, bwrap and the rest relay itself runs).
fn caller_path(dirs: &[&Path]) -> std::ffi::OsString {
    let system = ["/usr/bin", "/bin"].map(PathBuf::from);
    std::env::join_paths(dirs.iter().map(|d| d.to_path_buf()).chain(system)).unwrap()
}

/// relay run as every test runs it, from a shell whose `PATH` is `path`, with `extra` variables.
fn relay_from(env: &Env, path: &std::ffi::OsStr, extra: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_relay"));
    prepared(env, cmd.args(args)).env("PATH", path).envs(extra.iter().copied()).output().unwrap()
}

/// The harnesses `quota show --json` says relay may use, and those it names as missing their program.
fn harnesses_seen(out: &Output) -> (Vec<String>, Vec<String>) {
    ok(out, "quota show");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names = |key: &str| -> Vec<String> {
        v[key].as_array().into_iter().flatten().filter_map(|h| h.as_str().map(str::to_owned)).collect()
    };
    (names("harnesses"), names("missing"))
}

/// The project's `[harness.omp]` says only its model: relay must find the program itself.
fn omp_by_name(env: &Env) {
    let conf = std::fs::read_to_string(env.repo.join("relay.toml")).unwrap();
    let conf = conf.replace("[harness.omp]\nenabled = false\n", "[harness.omp]\nmodel = \"stub/m1\"\n");
    std::fs::write(env.repo.join("relay.toml"), conf).unwrap();
}

#[test]
fn a_harness_found_once_is_found_from_a_shell_whose_path_lacks_it() {
    let env = setup("programs", false);
    omp_by_name(&env);
    // omp lives where a version manager puts it, on the operator's PATH and nowhere else.
    let bin = env.root.join("mise/installs/omp/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(env.repo.join(".stub/omp"), bin.join("omp")).unwrap();
    let (full, bare) = (caller_path(&[&bin]), caller_path(&[]));
    let both = vec!["claude".to_owned(), "omp".to_owned()];
    assert_eq!(harnesses_seen(&relay_from(&env, &full, &[], &["--json", "quota", "show"])).0, both);
    // From a shell without it, relay still knows where omp is: the board, the queue and the
    // dispatcher it wakes all see both harnesses.
    assert_eq!(harnesses_seen(&relay_from(&env, &bare, &[], &["--json", "quota", "show"])), (both, vec![]));
    let held = relay_from(&env, &bare, &[], &["queue", "add", "held", "--text", "x", "--readonly", "--harness", "omp"]);
    ok(&held, "a job held to omp, queued from a shell without omp");
    // A bare model has two harnesses to belong to, whichever shell names it.
    let vague = relay_from(&env, &bare, &[], &["queue", "add", "vague", "--text", "x", "--model", "m2"]);
    assert!(!vague.status.success() && stderr(&vague).contains("harness=model"), "{}", stderr(&vague));
}

#[test]
fn a_configured_harness_whose_program_is_missing_is_shown_as_such_and_never_dropped() {
    use std::os::unix::fs::PermissionsExt;
    let env = setup("no-program", false);
    omp_by_name(&env);
    let bin = env.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = caller_path(&[&bin]);
    let look = |what: &str| {
        let (usable, missing) = harnesses_seen(&relay_from(&env, &path, &[], &["--json", "quota", "show"]));
        assert_eq!((usable, missing), (vec!["claude".to_owned()], vec!["omp".to_owned()]), "{what}");
        let text = String::from_utf8_lossy(&relay_from(&env, &path, &[], &["quota", "show"]).stdout).into_owned();
        assert!(text.lines().any(|l| l.contains("omp") && l.contains("program not found")), "{what}: {text}");
    };
    look("nowhere on this machine");
    // A file of that name that cannot be run is not the program either.
    std::fs::write(bin.join("omp"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(bin.join("omp"), std::fs::Permissions::from_mode(0o644)).unwrap();
    look("not executable");
    // A job held to it is refused, and says why.
    let out = relay_from(&env, &path, &[], &["queue", "add", "o", "--text", "x", "--readonly", "--harness", "omp"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("program not found"), "{}", stderr(&out));
}

/// The environment of the dispatcher serving `env`'s project, once one runs (bounded).
fn dispatcher_environ(env: &Env) -> Vec<String> {
    let config = env.repo.join("relay.toml").canonicalize().unwrap();
    let wanted = |cmdline: &[u8]| {
        let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).collect();
        args.windows(2).any(|w| w == [b"dispatch".as_slice(), b"serve".as_slice()])
            && args.iter().any(|a| *a == config.as_os_str().as_encoded_bytes())
    };
    for _ in 0..100 {
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let dir = entry.path();
            if std::fs::read(dir.join("cmdline")).is_ok_and(|c| wanted(&c)) {
                let environ = std::fs::read(dir.join("environ")).unwrap_or_default();
                return environ.split(|&b| b == 0).map(|v| String::from_utf8_lossy(v).into_owned()).collect();
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no dispatcher for {}", config.display());
}

#[test]
fn whoever_wakes_the_dispatcher_chooses_none_of_its_environment_nor_a_jobs() {
    if !have_bwrap() {
        return;
    }
    let env = setup("environ", false);
    configure(&env, "", "[session.env]\nFROM_CONFIG = \"yes\"\n");
    claude_account(&env.home, "a", 10, false);
    let theirs = env.root.join("callers-own-bin");
    std::fs::create_dir_all(&theirs).unwrap();
    let caller = [("RELAY_LEAK", "1"), ("TZ", "Pacific/Kiritimati"), ("GIT_WORK_TREE", "/nonexistent")];
    let out =
        relay_from(&env, &caller_path(&[&theirs]), &caller, &["queue", "add", "env", "--text", "[env]", "--readonly"]);
    ok(&out, "queue from an odd shell");
    let leaked = |vars: &[String], what: &str| {
        for v in vars {
            let name = v.split('=').next().unwrap_or_default();
            assert!(!["RELAY_LEAK", "TZ", "GIT_WORK_TREE"].contains(&name), "{what} inherited {v}");
            assert!(!v.contains("callers-own-bin"), "{what} inherited {v}");
        }
        assert!(vars.iter().any(|v| v == "FROM_CONFIG=yes"), "{what} lacks session.env: {vars:?}");
    };
    leaked(&dispatcher_environ(&env), "the dispatcher");
    assert_eq!(wait_for(&env, "env")["status"], "done");
    let job: Vec<String> =
        std::fs::read_to_string(env.home.join(".claude.a/env")).unwrap().lines().map(str::to_owned).collect();
    leaked(&job, "the job");
}

#[test]
fn outside_a_project_the_accounts_pane_is_machine_wide_and_calls_no_account_ready() {
    let env = setup("machine-wide", false);
    claude_account(&env.home, "solo", 40, false);
    ok(&relay(&env, &["quota", "refresh"]), "a reading");
    ok(&relay(&env, &["quota", "disable", "claude:solo"]), "disabled in the one project that uses it");
    let inside = String::from_utf8_lossy(&relay(&env, &["quota", "show"]).stdout).into_owned();
    assert!(inside.lines().any(|l| l.contains("claude:solo") && l.contains("disabled")), "{inside}");
    std::fs::remove_file(env.repo.join("relay.toml")).unwrap();
    let outside = board(&env);
    let accounts = outside.split_once("accounts").map(|(_, a)| a).unwrap_or_default();
    assert!(accounts.contains("machine-wide"), "{outside}");
    assert!(accounts.lines().any(|l| l.contains("claude:solo") && l.contains("40%")), "the shared reading: {outside}");
    assert!(!accounts.contains("ready"), "no project's rules judged it: {outside}");
}
