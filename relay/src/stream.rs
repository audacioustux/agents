//! Reading a `claude -p --output-format stream-json --verbose` stream, and
//! deciding what a finished session means. Pure, so it is tested on recorded lines.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::bus::Windows;

/// What the stream told us, built up line by line.
#[derive(Debug, Default)]
pub struct Observed {
    /// The latest quota reading carried by a `rate_limit_event`.
    pub windows: Option<Windows>,
    /// The last `result` event.
    pub last: Option<Value>,
    /// The last `result` that was not a wake-up after the report
    /// (a background task or monitor can add a later result).
    pub report: Option<Value>,
    /// The last text the main agent (not a subagent) wrote, for when the
    /// final message was a tool call and `result` is empty.
    pub last_text: Option<String>,
    pub lines: usize,
}

impl Observed {
    /// Feeds one line. Returns a fresh quota reading if the line carried one.
    pub fn line(&mut self, line: &str) -> Option<&Windows> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return None };
        self.lines += 1;
        match v.get("type").and_then(Value::as_str) {
            Some("rate_limit_event") => {
                let w = v.pointer("/rate_limit_info/unifiedWindows")?;
                self.windows = Some(serde_json::from_value(w.clone()).ok()?);
                return self.windows.as_ref();
            }
            Some("assistant") if v.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                let text: Vec<&str> = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                if !text.is_empty() {
                    self.last_text = Some(text.join("\n"));
                }
            }
            Some("result") => {
                if v.pointer("/origin/kind").and_then(Value::as_str) != Some("task-notification") {
                    self.report = Some(v.clone());
                }
                self.last = Some(v);
            }
            _ => {}
        }
        None
    }

    /// The text to show as the session's answer.
    pub fn answer(&self) -> String {
        let from_result = self.report.as_ref().and_then(|r| r.get("result")).and_then(Value::as_str).unwrap_or("");
        if from_result.trim().is_empty() { self.last_text.clone().unwrap_or_default() } else { from_result.to_owned() }
    }

    pub fn cost_usd(&self) -> f64 {
        self.last.as_ref().and_then(|r| r.get("total_cost_usd")).and_then(Value::as_f64).unwrap_or(0.0)
    }

    pub fn turns(&self) -> u32 {
        self.last
            .as_ref()
            .and_then(|r| r.get("num_turns"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    }
}

/// What to do with a session that has ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Done,
    /// Ask once more on the same account, for example for the missing final report.
    FollowUp(String),
    /// The account is out of quota until `until` (unix seconds, if stated).
    Limited {
        until: Option<i64>,
    },
    SignedOut,
    /// Nothing usable came out. Worth one more attempt.
    NoOutput,
    /// Ended by its spend cap. Never retried on its own: spending more is the owner's call.
    Budget,
    Failed(String),
}

static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(hit your (weekly |session |5-hour )?limit|usage limit|rate limit|limit reached|out of extra usage|usage_limit_reached)").unwrap()
});
static AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(failed to authenticate|oauth (session|token) (has )?expired|not logged in|please run /login|invalid api key)").unwrap()
});

#[derive(Debug, Clone, Copy)]
pub struct Ending {
    pub exit_ok: bool,
    pub timed_out: bool,
    pub silent: bool,
}

/// `final_marker` empty means any answer counts as a report.
pub fn verdict(obs: &Observed, end: Ending, final_marker: &str, now_s: i64) -> Verdict {
    if end.silent {
        return Verdict::Failed("printed nothing for too long; ended".into());
    }
    if end.timed_out {
        return Verdict::Failed("ran past its time limit; ended".into());
    }
    let Some(last) = &obs.last else {
        return if obs.lines == 0 { Verdict::NoOutput } else { Verdict::Failed("ended without a result".into()) };
    };
    let text = last.get("result").and_then(Value::as_str).unwrap_or("");
    let subtype = last.get("subtype").and_then(Value::as_str).unwrap_or("");
    // `is_error` can be true with subtype "success" (an auth failure is reported that way).
    let is_error = last.get("is_error").and_then(Value::as_bool).unwrap_or(false) || !matches!(subtype, "" | "success");
    if subtype == "error_max_budget_usd" {
        return Verdict::Budget;
    }
    if is_error {
        // Match only the message fields: a uuid elsewhere in the event can contain "429".
        let head: String = text.chars().take(400).collect();
        let api_error = last.get("api_error").and_then(Value::as_str).unwrap_or("");
        let status_429 = last.get("api_error_status").and_then(Value::as_u64) == Some(429);
        if LIMIT.is_match(&head) || LIMIT.is_match(api_error) || status_429 {
            let until = obs
                .windows
                .as_ref()
                .and_then(|w| w.values().filter(|w| w.utilization >= 1.0).filter_map(|w| w.resets_at).max())
                .or_else(|| crate::usage::parse_reset(&head, now_s));
            return Verdict::Limited { until };
        }
        if AUTH.is_match(&head) {
            return Verdict::SignedOut;
        }
        return Verdict::Failed(format!(
            "{}: {}",
            if subtype.is_empty() { "error" } else { subtype },
            head.lines().next().unwrap_or("")
        ));
    }
    if !end.exit_ok {
        return Verdict::Failed("the agent exited with an error after its result".into());
    }
    let answer = obs.answer();
    if !final_marker.is_empty() && !answer.contains(final_marker) {
        return Verdict::FollowUp(format!(
            "Your last message has no {final_marker} section, so the job is not finished. Ending your turn ends \
             the job: nothing waits for you. Finish any remaining work now, then end with the {final_marker} section."
        ));
    }
    Verdict::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: &str = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794150000,"rateLimitType":"seven_day","unifiedWindows":{"seven_day":{"utilization":0.23,"resetsAt":1794150000,"status":"allowed"}}},"session_id":"s"}"#;

    fn obs(lines: &[&str]) -> Observed {
        let mut o = Observed::default();
        for l in lines {
            o.line(l);
        }
        o
    }

    const OK: Ending = Ending { exit_ok: true, timed_out: false, silent: false };

    #[test]
    fn reads_quota_windows() {
        let o = obs(&[RATE]);
        let w = o.windows.unwrap();
        assert!((w["seven_day"].utilization - 0.23).abs() < 1e-9);
        assert_eq!(w["seven_day"].resets_at, Some(1_794_150_000));
    }

    #[test]
    fn done_needs_the_final_marker() {
        let done = obs(&[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"work\nFINAL\nall good","num_turns":3,"total_cost_usd":0.5}"#,
        ]);
        assert_eq!(verdict(&done, OK, "FINAL", 0), Verdict::Done);
        assert_eq!(done.turns(), 3);
        let early = obs(&[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"I'll continue once the build finishes"}"#,
        ]);
        assert!(matches!(verdict(&early, OK, "FINAL", 0), Verdict::FollowUp(_)));
        assert_eq!(verdict(&early, OK, "", 0), Verdict::Done);
    }

    #[test]
    fn a_wake_up_after_the_report_does_not_hide_it() {
        let o = obs(&[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"FINAL: shipped"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","origin":{"kind":"task-notification"}}"#,
        ]);
        assert_eq!(verdict(&o, OK, "FINAL", 0), Verdict::Done);
        assert_eq!(o.answer(), "FINAL: shipped");
    }

    #[test]
    fn empty_result_falls_back_to_last_main_agent_text() {
        let o = obs(&[
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"FINAL main"}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"text","text":"subagent words"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":""}"#,
        ]);
        assert_eq!(o.answer(), "FINAL main");
    }

    #[test]
    fn limits_auth_budget_and_errors() {
        let weekly = obs(&[
            r#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your weekly limit · resets Nov 6, 5am (UTC)"}"#,
        ]);
        assert!(matches!(verdict(&weekly, OK, "FINAL", 1_793_000_000), Verdict::Limited { until: Some(_) }));
        let api = obs(&[
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"","api_error":"usage_limit_reached","api_error_status":429}"#,
        ]);
        assert_eq!(verdict(&api, OK, "FINAL", 0), Verdict::Limited { until: None });
        let auth = obs(&[
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Failed to authenticate: OAuth session expired and could not be refreshed"}"#,
        ]);
        assert_eq!(verdict(&auth, OK, "FINAL", 0), Verdict::SignedOut);
        let budget = obs(&[r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":""}"#]);
        assert_eq!(verdict(&budget, OK, "FINAL", 0), Verdict::Budget);
        let uuid_429 = obs(&[
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"tool crashed","uuid":"a429b"}"#,
        ]);
        assert!(matches!(verdict(&uuid_429, OK, "FINAL", 0), Verdict::Failed(_)));
        assert_eq!(verdict(&obs(&[]), OK, "FINAL", 0), Verdict::NoOutput);
    }
}
