//! `relay view cost`, end to end: from transcripts in a throwaway home, never running `claude`.

#[path = "support/isolated.rs"]
mod isolated;
use isolated::Isolated;

#[path = "support/executable.rs"]
mod executable;
use executable::write_executable;

fn line(id: &str, model: &str, ts: &str, input: u64, output: u64) -> String {
    let v = serde_json::json!({
        "type": "assistant", "requestId": format!("req_{id}"), "timestamp": ts, "cwd": "/work/demo",
        "message": { "id": id, "model": model, "usage": {
            "input_tokens": 0, "output_tokens": 0, "speed": "standard",
            "iterations": [{ "type": "message", "input_tokens": input, "output_tokens": output,
                "cache_read_input_tokens": 0, "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0 } }],
        } },
    });
    format!("{v}\n")
}

/// A stub `claude` first on `PATH` that leaves a mark and fails: the view must never run it.
#[test]
fn view_cost_reads_the_transcripts_and_never_runs_claude() {
    let home = Isolated::new("cost");
    let h = home.home();
    let bin = h.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ran = h.join("claude-ran");
    write_executable(&bin.join("claude"), &format!("#!/bin/sh\ntouch {}\nexit 1\n", ran.display()));
    let now = jiff::Timestamp::now().to_string();
    let a = line("msg_a", "claude-opus-5-5", &now, 1_000_000, 0);
    // The default home and a signed-out twin hold the same response; another model has no price.
    for (dir, text) in [(".claude", a.clone() + &line("msg_b", "claude-mystery-1", &now, 7, 3)), (".claude.work", a)] {
        let p = h.join(dir).join("projects/-work-demo");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("s.jsonl"), text).unwrap();
    }
    std::fs::create_dir_all(h.join(".config/relay")).unwrap();
    std::fs::write(
        h.join(".config/relay/policy.toml"),
        "[prices]\nweb_search_per_1000 = 10.0\n[prices.models.claude-opus-5-5]\ninput = 4.0\noutput = 20.0\n\
         cache_write_5m = 5.0\ncache_write_1h = 8.0\ncache_read = 0.2\nfast = 2.0\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut cmd = home.command(env!("CARGO_BIN_EXE_relay"));
        #[allow(clippy::disallowed_methods, reason = "the stub goes first on the test's own PATH")]
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let out = cmd.env("PATH", path).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let v: serde_json::Value = serde_json::from_str(&run(&["--json", "view", "cost"])).unwrap();
    assert_eq!(v["today"]["usd"], 4.0, "{v:#}");
    assert_eq!(v["this_week"]["usd"], 4.0);
    assert_eq!(v["rows"].as_array().unwrap().len(), 7);
    assert_eq!(v["dedup"]["messages"], 2);
    assert_eq!(v["dedup"]["copies_dropped"], 1, "the twin's copy");
    assert_eq!(v["unpriced"][0]["model"], "claude-mystery-1");
    assert_eq!(v["unpriced"][0]["tokens"], 10);
    assert_eq!(v["tokens"]["input"], 1_000_007);
    assert!(v["index"]["changed_at"].is_string());
    assert!(h.join(".local/state/relay/cost/index.bin").is_file(), "the index is machine-wide");
    let text = run(&["view", "cost", "--by", "week", "--split", "account"]);
    assert!(text.starts_with("API-equivalent cost"), "{text}");
    assert!(text.contains("week of") && text.contains("+ 10 unpriced tokens (models: claude-mystery-1)"), "{text}");
    assert!(text.lines().any(|l| l.trim_start().starts_with("default") && l.contains("$4.00")), "{text}");
    let help = run(&["view", "cost", "--help"]);
    assert!(help.contains("API-equivalent"), "{help}");
    assert!(!ran.exists(), "view cost ran claude");
}
