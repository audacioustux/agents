//! What a finished run means, the same way for every harness. Each
//! harness's [`crate::harness::Reader`] fills a [`Report`]; [`verdict`] judges it.

use std::sync::LazyLock;

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use regex::Regex;

use crate::harness::Meters;

/// A run's output, in harness-neutral terms.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// The id the harness gave the session, once it said.
    pub session_id: Option<String>,
    /// Lines that parsed as events.
    pub lines: usize,
    /// A final result arrived.
    pub ended: bool,
    /// The final answer: the last report that was not a wake-up after it,
    /// or the main agent's last words when that report is empty.
    pub answer: String,
    /// The texts of the wake-up results after `answer`'s, in order: a report can be written in one.
    pub wakeups: Vec<String>,
    pub is_error: bool,
    /// The error's own message fields only (never whole events: an id can contain "429").
    pub error_text: String,
    pub api_status: Option<u64>,
    /// The CLI ended the run at its spend cap.
    pub budget_hit: bool,
    pub cost_usd: f64,
    pub turns: u32,
    /// The latest quota reading the stream carried.
    pub meters: Option<Meters>,
}

impl Report {
    /// The report to keep: the latest result carrying `marker`, else [`Report::answer`]. With no
    /// marker, the answer: a wake-up's "ok" must not replace it.
    pub fn chosen(&self, marker: &str) -> &str {
        if marker.is_empty() {
            return &self.answer;
        }
        self.wakeups.iter().rev().find(|w| w.contains(marker)).unwrap_or(&self.answer)
    }
}

/// How the run ended, from the runner's side.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Ending {
    pub exit_ok: bool,
    pub timed_out: bool,
    pub silent: bool,
    /// The runner ended it for spending past its cap.
    pub over_budget: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Done,
    /// Ask once more on the same session, for example for the missing final report.
    FollowUp(String),
    /// Out of quota until `until` (unix seconds), if stated.
    Limited {
        until: Option<i64>,
    },
    SignedOut,
    /// Nothing usable came out. Worth one more attempt.
    NoOutput,
    /// Ended at its spend cap. Retried on its own only past a progress check, when the
    /// owner allowed the budget to grow (`limits.budget_growth`): spending more is their call.
    Budget,
    Failed(String),
}

static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(hit your (weekly |session |5-hour )?limit|usage limit|rate.?limit|limit reached|out of extra usage|usage_limit_reached|quota exceeded|resource.?exhausted|\b429\b|too many requests)").expect("regex")
});
static AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(failed to authenticate|oauth (session|token) (has )?expired|refresh token expired|not logged in|please run /login|re-?login|invalid api key|unauthorized|\b401\b)").expect("regex")
});

/// `final_marker` empty means any answer counts as the report. A read-only job
/// has no commits left to finish, so any non-empty answer is its result.
pub fn verdict(r: &Report, end: Ending, final_marker: &str, readonly: bool, now_s: i64) -> Verdict {
    if end.over_budget || r.budget_hit {
        return Verdict::Budget;
    }
    // A finished report stands however the process ended after it: a background child kept
    // alive past the silence or time limit, or an exit error after the result, loses nothing.
    let answered = readonly && !r.answer.trim().is_empty();
    let reported = final_marker.is_empty() || r.answer.contains(final_marker) || answered;
    if r.ended && !r.is_error && reported {
        return Verdict::Done;
    }
    if end.silent {
        return Verdict::Failed("printed nothing for too long; ended".into());
    }
    if end.timed_out {
        return Verdict::Failed("ran past its time limit; ended".into());
    }
    if !r.ended && !r.is_error {
        return if r.lines == 0 { Verdict::NoOutput } else { Verdict::Failed("ended without a result".into()) };
    }
    if r.is_error {
        let head: String = r.error_text.chars().take(400).collect();
        if LIMIT.is_match(&head) || r.api_status == Some(429) {
            let until = r
                .meters
                .as_ref()
                .and_then(|m| m.values().filter(|w| w.utilization >= 1.0).filter_map(|w| w.resets_at).max())
                .or_else(|| parse_reset(&head, now_s));
            return Verdict::Limited { until };
        }
        if AUTH.is_match(&head) || r.api_status == Some(401) {
            return Verdict::SignedOut;
        }
        return Verdict::Failed(format!("error: {}", head.lines().next().unwrap_or("")));
    }
    if !end.exit_ok {
        return Verdict::Failed("the agent exited with an error after its result".into());
    }
    Verdict::FollowUp(format!(
        "Your last message has no {final_marker} section, so the job is not finished. Ending your turn ends \
         the job: nothing waits for you. Finish any remaining work now, then end with the {final_marker} section."
    ))
}

static RESET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)resets\s+(?:(?P<mon>[A-Z][a-z]{2})\s+(?P<day>\d{1,2}),?\s+)?(?:at\s+)?(?P<h>\d{1,2})(?::(?P<m>\d{2}))?\s*(?P<ap>am|pm)\s*(?:\((?P<tz>[^)]+)\))?").expect("regex")
});
static RESETS_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"resets in\s+((?:\d+[dhms])+)").expect("regex"));

/// The next moment after `now_s` matching "resets [Mon D,] H[:MM]am|pm [(Zone)]"
/// or "resets in 6d23h".
pub fn parse_reset(text: &str, now_s: i64) -> Option<i64> {
    if let Some(c) = RESETS_IN.captures(text) {
        return Some(now_s + duration_seconds(&c[1]));
    }
    let c = RESET.captures(text)?;
    let tz = c.name("tz").map_or(Some(TimeZone::UTC), |z| match z.as_str() {
        "UTC" | "GMT" => Some(TimeZone::UTC),
        name => TimeZone::get(name).ok(),
    })?;
    let mut hour: i8 = c["h"].parse().ok()?;
    let minute: i8 = c.name("m").map_or(Some(0), |m| m.as_str().parse().ok())?;
    if hour == 12 {
        hour = 0;
    }
    if c["ap"].eq_ignore_ascii_case("pm") {
        hour += 12;
    }
    let time = Time::new(hour, minute, 0, 0).ok()?;
    let now = Timestamp::from_second(now_s).ok()?.to_zoned(tz.clone());
    let at = |date: Date| date.to_datetime(time).to_zoned(tz.clone()).ok();
    let after = |z: &Zoned| z.timestamp().as_second() > now_s;
    let found = if let (Some(mon), Some(day)) = (c.name("mon"), c.name("day")) {
        let month = month_number(mon.as_str())?;
        let day: i8 = day.as_str().parse().ok()?;
        [now.year(), now.year() + 1].into_iter().filter_map(|y| Date::new(y, month, day).ok().and_then(at)).find(after)
    } else {
        [now.date(), now.date().tomorrow().ok()?].into_iter().filter_map(at).find(after)
    };
    found.map(|z| z.timestamp().as_second())
}

/// "6d23h", "4h30m": seconds.
pub fn duration_seconds(text: &str) -> i64 {
    static PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)([dhms])").expect("regex"));
    PART.captures_iter(text)
        .map(|c| {
            let n: i64 = c[1].parse().unwrap_or(0);
            n * match &c[2] {
                "d" => 86_400,
                "h" => 3_600,
                "m" => 60,
                _ => 1,
            }
        })
        .sum()
}

fn month_number(m: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let m = m.to_ascii_lowercase();
    MONTHS.iter().position(|x| *x == m).and_then(|i| i8::try_from(i + 1).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-02T15:00:00Z
    const NOW: i64 = 1_793_631_600;
    const OK: Ending = Ending { exit_ok: true, timed_out: false, silent: false, over_budget: false };

    fn done(answer: &str) -> Report {
        Report { lines: 1, ended: true, answer: answer.into(), ..Default::default() }
    }

    fn error(text: &str) -> Report {
        Report { lines: 1, ended: true, is_error: true, error_text: text.into(), ..Default::default() }
    }

    #[test]
    fn done_needs_the_marker_once() {
        assert_eq!(verdict(&done("x\nFINAL ok"), OK, "FINAL", false, NOW), Verdict::Done);
        assert!(matches!(verdict(&done("I'll continue"), OK, "FINAL", false, NOW), Verdict::FollowUp(_)));
        assert_eq!(verdict(&done("anything"), OK, "", false, NOW), Verdict::Done);
    }

    #[test]
    fn readonly_answer_needs_no_marker() {
        assert_eq!(verdict(&done("the review, no marker"), OK, "FINAL", true, NOW), Verdict::Done);
        assert!(matches!(verdict(&done("  \n"), OK, "FINAL", true, NOW), Verdict::FollowUp(_)));
    }

    #[test]
    fn a_finished_report_survives_how_the_process_ended() {
        let silent = Ending { silent: true, exit_ok: false, ..OK };
        assert_eq!(verdict(&done("FINAL ok"), silent, "FINAL", false, NOW), Verdict::Done);
        let late = Ending { timed_out: true, ..OK };
        assert_eq!(verdict(&done("the review"), late, "FINAL", true, NOW), Verdict::Done);
        assert!(matches!(verdict(&done("I'll continue"), silent, "FINAL", false, NOW), Verdict::Failed(_)));
    }

    #[test]
    fn errors_are_classified_from_their_message() {
        assert!(matches!(
            verdict(&error("You've hit your weekly limit · resets Nov 6, 5am (UTC)"), OK, "FINAL", false, NOW),
            Verdict::Limited { until: Some(_) }
        ));
        // lanes' regression case: the underscore spelling, with no status code.
        assert_eq!(
            verdict(&error("API Error: rate_limit_error"), OK, "FINAL", false, NOW),
            Verdict::Limited { until: None }
        );
        let r = Report { api_status: Some(429), ..error("") };
        assert_eq!(verdict(&r, OK, "FINAL", false, NOW), Verdict::Limited { until: None });
        assert_eq!(
            verdict(&error("Failed to authenticate: OAuth session expired"), OK, "FINAL", false, NOW),
            Verdict::SignedOut
        );
        assert!(matches!(verdict(&error("tool crashed"), OK, "FINAL", false, NOW), Verdict::Failed(_)));
        assert_eq!(verdict(&Report::default(), OK, "FINAL", false, NOW), Verdict::NoOutput);
        let over = Ending { over_budget: true, ..OK };
        assert_eq!(verdict(&done("FINAL"), over, "FINAL", false, NOW), Verdict::Budget);
    }

    #[test]
    fn resets() {
        assert_eq!(parse_reset("resets 4pm (UTC)", NOW), Some(NOW + 3600));
        assert_eq!(parse_reset("resets 2pm (UTC)", NOW), Some(NOW + 23 * 3600), "a past time today means tomorrow");
        assert_eq!(parse_reset("resets 12am (UTC)", NOW), Some(NOW + 9 * 3600));
        assert_eq!(parse_reset("resets 3:30pm (UTC)", NOW), Some(NOW + 1800));
        assert_eq!(parse_reset("resets Jan 2, 3pm (UTC)", NOW), Some(1_798_902_000), "rolls into next year");
        assert_eq!(parse_reset("21.0% used · resets in 5d20h", NOW), Some(NOW + 5 * 86_400 + 20 * 3600));
        assert_eq!(parse_reset("no reset here", NOW), None);
    }
}
