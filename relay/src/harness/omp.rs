//! omp (oh-my-pi). Homes are profiles: `~/.omp` itself (`default`) and
//! `~/.omp/profiles/<name>`, selected by `OMP_PROFILE`. One home reaches
//! many providers, metered apart by `omp usage`, so the meters that govern a
//! task are its model's provider's. Its read-only mode is approval-gated
//! (it asks), not a removed capability, and it cannot fork a session.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::harness::claude::strip_ansi;
use crate::harness::report::{Report, duration_seconds};
use crate::harness::{
    Caps, Harness, Home, Invocation, Meter, Meters, Permissions, Reader, Refusal, Session, Task, printable,
};

#[derive(Debug)]
pub struct Omp;

fn root(user_home: &Path) -> PathBuf {
    user_home.join(".omp")
}

fn usable(dir: &Path) -> bool {
    dir.join("agent/agent.db").is_file()
}

/// "Openai Codex" is `openai-codex`, the prefix of its model ids.
pub fn provider_of(section: &str) -> String {
    section.trim().to_lowercase().replace(' ', "-")
}

impl Harness for Omp {
    fn id(&self) -> &'static str {
        "omp"
    }

    fn caps(&self) -> Caps {
        Caps {
            read_only_by_capability: false,
            fork: false,
            preset_session_id: false,
            budget: false,
            quota: true,
            verified: true,
        }
    }

    fn identity(&self) -> &'static str {
        "You are OMP"
    }

    fn homes(&self, user_home: &Path) -> Vec<Home> {
        let root = root(user_home);
        let mut homes: Vec<Home> = std::fs::read_dir(root.join("profiles"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .filter(|(_, d)| usable(d))
            .map(|(label, dir)| Home { harness: "omp", label, dir })
            .collect();
        homes.sort_by(|a, b| a.label.cmp(&b.label));
        if usable(&root) {
            homes.insert(0, Home { harness: "omp", label: "default".into(), dir: root });
        }
        homes
    }

    fn home_paths(&self, user_home: &Path) -> Vec<PathBuf> {
        let root = root(user_home);
        let profiles = std::fs::read_dir(root.join("profiles")).into_iter().flatten().flatten().map(|e| e.path());
        std::iter::once(root.clone()).chain(profiles).collect()
    }

    fn writable(&self, home: &Home, _user_home: &Path) -> Vec<PathBuf> {
        vec![home.dir.clone()]
    }

    fn protected(&self, home: &Home) -> Vec<PathBuf> {
        const CONFIG: [&str; 10] = [
            "agent/config.yml",
            "agent/models.yml",
            "agent/agents",
            "agent/skills",
            "agent/managed-skills",
            "agent/extensions",
            "agent/hooks",
            "agent/commands",
            "natives",
            "security",
        ];
        CONFIG.iter().map(|c| home.dir.join(c)).collect()
    }

    fn invocation(&self, home: &Home, task: &Task<'_>) -> Result<Invocation, Refusal> {
        let mut i = Invocation::new("omp");
        i.args(["-p", "--no-title"]);
        if task.events {
            // Sessions live with the task, not the profile, so a session
            // moves between profiles with nothing to copy.
            i.args(["--mode", "json", "--session-dir"]).arg(task.scratch.join("sessions"));
        }
        if task.lean {
            i.args(["--no-extensions", "--no-skills"]);
        }
        match task.permissions {
            Permissions::Unrestricted => i.arg("--auto-approve"),
            Permissions::EditsOnly => i.args(["--approval-mode", "write"]),
            // omp has no read-only mode; this makes it ask before every tool call.
            Permissions::ReadOnly => i.args(["--approval-mode", "always-ask"]),
        };
        if let Some(m) = task.model {
            i.args(["--model", m]);
        }
        if let Some(e) = task.effort {
            i.args(["--thinking", e]);
        }
        if let Some(b) = task.briefing {
            i.args(["--append-system-prompt", b]);
        }
        match task.session {
            Session::New(_) => {}
            Session::Resume(id) => {
                i.args(["--resume", id]);
            }
            Session::Fork(id) => {
                return Err(Refusal(format!(
                    "omp has no --fork-session; resume would extend the prior session. Pass --fresh, or call \
                     `omp --resume {} --no-session` directly (fork-on-resume gap is your responsibility).",
                    printable(id)
                )));
            }
        }
        // The prompt is a file the message names (`@file`): never argv.
        let file = task.scratch.join("prompt.md");
        i.arg(format!("@{}", file.display()));
        i.files.push((file, task.prompt.to_owned()));
        let profile = (home.label != "default").then(|| home.label.clone().into());
        i.env.push(("OMP_PROFILE".into(), profile));
        Ok(i)
    }

    fn reader(&self) -> Box<dyn Reader> {
        Box::new(JsonReader::default())
    }

    fn quota(&self, home: &Home) -> Option<Invocation> {
        let mut i = Invocation::new("omp");
        i.arg("usage");
        let profile = (home.label != "default").then(|| home.label.clone().into());
        i.env.push(("OMP_PROFILE".into(), profile));
        Some(i)
    }

    /// `omp usage`: a section per provider, `<Provider> — N accounts`, then
    /// windows like `● 7 days ███░░ 21.0% used · resets in 5d20h`. A section
    /// whose accounts are all disabled has no windows, so it reads as unmetered
    /// and [`Self::meters_for`] refuses it.
    fn parse_quota(&self, text: &str, now_s: i64) -> Meters {
        static HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S.*?) — ").expect("regex"));
        static WINDOW: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"●\s+(.+?)\s+[█░]+\s+([\d.]+)% used(?: · resets in (\S+))?").expect("regex"));
        let clean = strip_ansi(text);
        let mut out = Meters::new();
        let mut provider = None;
        for line in clean.lines() {
            if let Some(h) = HEADER.captures(line) {
                provider = Some(provider_of(&h[1]));
            }
            let (Some(p), Some(w)) = (&provider, WINDOW.captures(line)) else { continue };
            let used: f64 = w[2].parse().unwrap_or(0.0);
            let meter =
                Meter { utilization: used / 100.0, resets_at: w.get(3).map(|r| now_s + duration_seconds(r.as_str())) };
            // Several accounts under one provider: the fullest one speaks for it.
            let slot = out.entry(format!("{p}/{}", w[1].trim())).or_default();
            if meter.utilization >= slot.utilization {
                *slot = meter;
            }
        }
        out
    }

    fn meters_for<'m>(&self, model: Option<&str>, meters: &'m Meters) -> Result<Vec<&'m Meter>, Refusal> {
        let Some((provider, _)) = model.and_then(|m| m.split_once('/')) else {
            return Err(Refusal(
                "omp needs a provider-qualified model (provider/name, e.g. openai-codex/gpt-5.2) to know whose quota it draws on".into(),
            ));
        };
        let prefix = format!("{provider}/");
        let found: Vec<&Meter> = meters.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(_, m)| m).collect();
        if found.is_empty() {
            return Err(Refusal(format!("cannot read omp's {provider} quota; not starting blind")));
        }
        Ok(found)
    }

    /// The session is in the task's own store, whichever profile wrote it.
    fn carry(&self, _session: &str, _from: Option<&Home>, _to: &Home, scratch: &Path) -> bool {
        std::fs::read_dir(scratch.join("sessions")).is_ok_and(|mut d| d.next().is_some())
    }
}

/// `--mode json`: one event per line.
#[derive(Debug, Default)]
struct JsonReader {
    report: Report,
}

impl Reader for JsonReader {
    fn line(&mut self, line: &str) -> Option<Meters> {
        let v: Value = serde_json::from_str(line).ok()?;
        let r = &mut self.report;
        r.lines += 1;
        match v.get("type").and_then(Value::as_str) {
            Some("session") => r.session_id = v.get("id").and_then(Value::as_str).map(str::to_owned),
            Some("agent_end") => r.ended = true,
            Some("message_end") if v.pointer("/message/role").and_then(Value::as_str) == Some("assistant") => {
                let m = &v["message"];
                r.turns += 1;
                r.cost_usd += m.pointer("/usage/cost/total").and_then(Value::as_f64).unwrap_or(0.0);
                let text: Vec<&str> = m
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                if let Some(t) = text.last() {
                    t.trim().clone_into(&mut r.answer);
                }
                let stop = m.get("stopReason").and_then(Value::as_str).unwrap_or("");
                if matches!(stop, "error" | "aborted") {
                    r.is_error = true;
                    let msg = ["errorMessage", "error"]
                        .iter()
                        .find_map(|k| {
                            m.get(*k).and_then(|e| e.as_str().map(str::to_owned).or_else(|| Some(e.to_string())))
                        })
                        .unwrap_or_else(|| stop.to_owned());
                    r.error_text = msg;
                }
            }
            _ => {}
        }
        None
    }

    fn report(&self) -> &Report {
        &self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{argv, home, task};

    #[test]
    fn argv_shape() {
        let s = Path::new("/s");
        let i = Omp
            .invocation(&home("omp", "work"), &task("brief", Session::Resume("abc"), Permissions::Unrestricted, s))
            .unwrap();
        assert_eq!(
            argv(&i),
            "-p --no-title --mode json --session-dir /s/sessions --no-extensions --no-skills --auto-approve --model m \
             --thinking high --append-system-prompt b --resume abc @/s/prompt.md"
        );
        assert_eq!(i.files, vec![(PathBuf::from("/s/prompt.md"), "brief".to_owned())]);
        assert!(i.env.contains(&("OMP_PROFILE".into(), Some("work".into()))));
        let d =
            Omp.invocation(&home("omp", "default"), &task("p", Session::New("x"), Permissions::ReadOnly, s)).unwrap();
        assert!(d.env.contains(&("OMP_PROFILE".into(), None)), "the default profile unsets it");
        assert!(argv(&d).contains("--approval-mode always-ask"));
        let fork =
            Omp.invocation(&home("omp", "default"), &task("p", Session::Fork("id\x1b[2J"), Permissions::ReadOnly, s));
        let e = fork.unwrap_err().0;
        assert!(e.contains("no --fork-session") && !e.contains('\x1b'));
    }

    #[test]
    fn usage_meters_by_provider() {
        let text = "Usage · fetched 1.9s ago\n\nAnthropic — 0 accounts\n  ✗ someone · x — disabled 74d19h ago: Refresh token expired\n\n\
Openai Codex — 1 account\n  ● someone · plan: plus\n      ● 5 hours  ░░░░  0.0% used · resets in 4h59m\n      ● 7 days   ██░░  21.0% used · resets in 5d20h\n";
        let m = Omp.parse_quota(text, 1_000);
        assert_eq!(m.len(), 2);
        assert!((m["openai-codex/7 days"].utilization - 0.21).abs() < 1e-9);
        assert_eq!(m["openai-codex/5 hours"].resets_at, Some(1_000 + 4 * 3600 + 59 * 60));
        assert_eq!(Omp.meters_for(Some("openai-codex/gpt-5.2"), &m).unwrap().len(), 2);
        assert!(Omp.meters_for(Some("anthropic/claude-opus"), &m).unwrap_err().0.contains("not starting blind"));
        assert!(Omp.meters_for(Some("opus"), &m).is_err(), "an unqualified model cannot be metered");
    }

    #[test]
    fn json_events() {
        let mut r = Omp.reader();
        for l in [
            r#"{"type":"session","id":"s9"}"#,
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","usage":{"cost":{"total":0.01}},"content":[{"type":"text","text":"working"}]}}"#,
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop","usage":{"cost":{"total":0.02}},"content":[{"type":"text","text":"FINAL done"}]}}"#,
            r#"{"type":"agent_end"}"#,
        ] {
            r.line(l);
        }
        let rep = r.report();
        assert_eq!((rep.session_id.as_deref(), rep.answer.as_str(), rep.turns), (Some("s9"), "FINAL done", 2));
        assert!(rep.ended && !rep.is_error && (rep.cost_usd - 0.03).abs() < 1e-9);
        let mut e = Omp.reader();
        e.line(r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"429 Too Many Requests"}}"#);
        assert!(e.report().is_error && e.report().error_text.contains("429"));
    }
}
