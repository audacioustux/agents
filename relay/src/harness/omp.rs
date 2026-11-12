//! omp (oh-my-pi). Homes are profiles: `~/.omp` itself (`default`) and
//! `~/.omp/profiles/<name>`, selected by `OMP_PROFILE`. One home reaches
//! many providers, metered apart by `omp usage`, so the meters that govern a
//! task are its model's provider's. Its read-only mode is approval-gated
//! (it asks), not a removed capability, and it cannot fork a session. It runs
//! on `bun` (`#!/usr/bin/env bun`), its helper program.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use jiff::tz::TimeZone;
use regex::Regex;
use serde_json::Value;

use crate::harness::claude::strip_ansi;
use crate::harness::report::{Report, duration_seconds};
use crate::harness::{
    Caps, Harness, Home, Invocation, Kind, Meter, Meters, Permissions, Reader, Refusal, Session, Task, Window,
    printable,
};

#[derive(Debug)]
pub struct Omp;

fn root(user_home: &Path) -> PathBuf {
    user_home.join(".omp")
}

fn usable(dir: &Path) -> bool {
    dir.join("agent/agent.db").is_file()
}

/// What a window `omp usage` names under a provider measures: its length (`7 days`, `General 5
/// Hour`, `Gemini (Weekly)`) and, when the name says one, the group of models it counts alone
/// (`Gemini`, `Claude & GPT (shared)`). `General` is the provider's whole allowance. A name that
/// says no length is a window of its own, of a length not said, scoped by that name.
pub fn window_of(label: &str) -> Meter {
    static LENGTH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\(?\b(?:(\d+)\s*(hour|day|week|month)s?|(hourly|daily|weekly|monthly))\b\)?").expect("regex")
    });
    let label = label.trim();
    let Some(c) = LENGTH.captures(label) else {
        return Meter::new(Window::Other(None), 0.0, None).scope(label);
    };
    let unit = |u: &str| match u.to_ascii_lowercase().as_str() {
        "hour" | "hourly" => 1,
        "day" | "daily" => 24,
        "week" | "weekly" => 7 * 24,
        _ => 30 * 24,
    };
    let hours = match (c.get(1), c.get(2), c.get(3)) {
        (Some(n), Some(u), _) => n.as_str().parse::<u32>().ok().and_then(|n| n.checked_mul(unit(u.as_str()))),
        (_, _, Some(period)) => Some(unit(period.as_str())),
        _ => None,
    };
    let window = hours.map_or(Window::Other(None), Window::of_hours);
    let whole = c.get(0).map_or(0..0, |m| m.range());
    let rest = format!("{} {}", &label[..whole.start], &label[whole.end..]);
    let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    let meter = Meter::new(window, 0.0, None);
    if rest.is_empty() || rest.eq_ignore_ascii_case("general") { meter } else { meter.scope(rest) }
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
        Caps { read_only_by_capability: false, fork: false, preset_session_id: false, budget: false, quota: true }
    }

    /// omp is a `cli.js` with `#!/usr/bin/env bun`, so every run needs a `bun` it can start from any
    /// directory: found and recorded with omp, and first on its runs' `PATH`.
    fn helpers(&self) -> &'static [&'static str] {
        &["bun"]
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

    fn protected_kinds(&self, home: &Home) -> Vec<(PathBuf, Kind)> {
        const FILES: [&str; 2] = ["agent/config.yml", "agent/models.yml"];
        const DIRS: [&str; 8] = [
            "agent/agents",
            "agent/skills",
            "agent/managed-skills",
            "agent/extensions",
            "agent/hooks",
            "agent/commands",
            "natives",
            "security",
        ];
        // The default profile is `~/.omp`, which holds every other profile: a session there that could
        // make `profiles/<name>` would have relay discover it as an account.
        let profiles = (home.label == "default").then(|| home.dir.join("profiles"));
        let files = FILES.iter().map(|c| (home.dir.join(c), Kind::File));
        files
            .chain(DIRS.iter().map(|c| (home.dir.join(c), Kind::Dir)))
            .chain(profiles.map(|p| (p, Kind::Dir)))
            .collect()
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
        // The prompt is a file the message names (`@file`): never argv. It is the host's, so it goes in
        // the inbox, never the session store the session writes.
        let file = task.inbox.join("prompt.md");
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
    fn parse_quota(&self, text: &str, now_s: i64, _local: &TimeZone) -> Meters {
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
            let meter = Meter {
                utilization: used / 100.0,
                resets_at: w.get(3).map(|r| now_s + duration_seconds(r.as_str())),
                ..window_of(&w[1]).provider(p.as_str())
            };
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
        let found: Vec<&Meter> = meters.values().filter(|m| m.provider.as_deref() == Some(provider)).collect();
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
    /// `*_update` events are deltas of a message or tool call that its
    /// `*_end` event repeats in full; kept, they made one docs job's log 85 MB.
    fn keep(&self, line: &str) -> bool {
        #[derive(serde::Deserialize)]
        struct Kind<'a> {
            #[serde(borrow, rename = "type")]
            kind: Option<&'a str>,
        }
        serde_json::from_str::<Kind<'_>>(line).ok().and_then(|k| k.kind).is_none_or(|k| !k.ends_with("_update"))
    }

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
             --thinking high --append-system-prompt b --resume abc @/inbox/prompt.md"
        );
        assert_eq!(i.files, vec![(PathBuf::from("/inbox/prompt.md"), "brief".to_owned())]);
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
        let m = Omp.parse_quota(text, 1_000, &TimeZone::UTC);
        assert_eq!(m.len(), 2);
        assert!((m["openai-codex/7 days"].utilization - 0.21).abs() < 1e-9);
        assert_eq!(m["openai-codex/5 hours"].resets_at, Some(1_000 + 4 * 3600 + 59 * 60));
        assert_eq!(Omp.meters_for(Some("openai-codex/gpt-5.2"), &m).unwrap().len(), 2);
        assert!(Omp.meters_for(Some("anthropic/claude-opus"), &m).unwrap_err().0.contains("not starting blind"));
        assert!(Omp.meters_for(Some("opus"), &m).is_err(), "an unqualified model cannot be metered");
        let week = &m["openai-codex/7 days"];
        assert_eq!(
            (week.window, week.provider.as_deref(), week.scope.as_deref()),
            (Window::Week, Some("openai-codex"), None)
        );
    }

    /// Every window name `omp usage` printed on this machine, by its length and the group it counts.
    #[test]
    fn a_window_says_its_length_and_its_group() {
        for (label, window, scope) in [
            ("5 hours", Window::FiveHours, None),
            ("7 days", Window::Week, None),
            ("General 5 Hour", Window::FiveHours, None),
            ("General 7 Day", Window::Week, None),
            ("Claude & GPT (shared) (Weekly)", Window::Week, Some("Claude & GPT (shared)")),
            ("Gemini (Weekly)", Window::Week, Some("Gemini")),
            ("Daily", Window::Other(Some(24)), None),
            ("tokens", Window::Other(None), Some("tokens")),
        ] {
            let m = window_of(label);
            assert_eq!((m.window, m.scope.as_deref()), (window, scope), "{label}");
        }
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
        let r = JsonReader::default();
        assert!(!r.keep(r#"{"type":"tool_execution_update","x":1}"#) && !r.keep(r#"{"type":"message_update"}"#));
        assert!(r.keep(r#"{"type":"message_end"}"#) && r.keep("not json"));
    }
}
