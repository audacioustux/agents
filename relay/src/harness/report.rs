//! What a finished run means, the same way for every harness. Each
//! harness's [`crate::harness::Reader`] fills a [`Report`]; [`verdict`] judges it.

use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use regex::Regex;

use crate::harness::{Meters, Window};

/// A run's output, in harness-neutral terms.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// The id the harness gave the session, once it said.
    pub session_id: Option<String>,
    /// Lines that parsed as events.
    pub lines: usize,
    /// A final result arrived.
    pub ended: bool,
    /// The final answer: the chosen result's text ([`Report::choose`]), or the main agent's last
    /// words when that result is empty.
    pub answer: String,
    /// Every result of the run, in order, each with its own status: `answer` is chosen from them
    /// ([`Report::choose`]), as a report can be written in a wake-up. Empty for a harness that
    /// gives one result.
    pub results: Vec<Said>,
    /// The zone the run's process kept its clock in (its environment's `TZ`): a reset time in an
    /// error that names no zone is in it. None: this machine's zone.
    pub zone: Option<TimeZone>,
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

/// One result of a run: its text, and how that result itself ended.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Said {
    pub text: String,
    /// It came in a wake-up turn (a background task finishing after the report).
    pub wakeup: bool,
    pub is_error: bool,
    pub error_text: String,
    pub api_status: Option<u64>,
}

impl Report {
    /// Keeps the report, with its own status: the latest result carrying `marker`, else the latest
    /// one that is not a wake-up (a wake-up's "ok" must not replace it), else the latest. With no
    /// results, the report stays as its reader filled it.
    pub fn choose(&mut self, marker: &str) {
        let marked = self.results.iter().rev().find(|s| !marker.is_empty() && s.text.contains(marker));
        // With no finished report, an error that ended the run (a limit, a lapsed sign-in) is what
        // it is judged by, never an earlier "waiting" that it cut short.
        let failed_last = || self.results.last().filter(|s| !marker.is_empty() && s.is_error);
        let ordinary = || self.results.iter().rev().find(|s| !s.wakeup);
        let Some(s) = marked.or_else(failed_last).or_else(ordinary).or(self.results.last()).cloned() else { return };
        (self.answer, self.is_error, self.error_text, self.api_status) =
            (s.text, s.is_error, s.error_text, s.api_status);
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
    /// Everything it started was seen to end and was reaped ([`crate::supervised::Supervised::tree_ended`]).
    pub tree_ended: bool,
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
                .or_else(|| parse_reset_in(&head, now_s, r.zone.as_ref().unwrap_or(&TimeZone::system()), None));
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

/// The zone a process with `TZ` set to `tz` reads its clock in: a zone name (`Asia/Dhaka`,
/// `:Etc/GMT-6`), else a POSIX rule (`UTC-6`); an empty one is UTC. With no `TZ`, or one that
/// cannot be read, this machine's zone.
pub fn zone_of(tz: Option<&str>) -> TimeZone {
    let Some(tz) = tz else { return TimeZone::system() };
    let name = tz.strip_prefix(':').unwrap_or(tz);
    if name.is_empty() {
        return TimeZone::UTC;
    }
    TimeZone::get(name).or_else(|_| TimeZone::posix(name)).unwrap_or_else(|_| TimeZone::system())
}

static RESET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)resets\s+(?:(?P<mon>[A-Z][a-z]{2})\s+(?P<day>\d{1,2}),?\s+)?(?:at\s+)?(?P<h>\d{1,2})(?::(?P<m>\d{2}))?\s*(?P<ap>am|pm)\s*(?:\((?P<tz>[^)]+)\))?").expect("regex")
});
static RESETS_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"resets in\s+((?:\d+[dhms])+)").expect("regex"));

/// How far behind `now` a meter's reset time may be and still be read as this meter's own: the
/// CLI keeps printing the reset that just passed for a while. The one place the grace is declared.
pub const RESET_GRACE_S: i64 = 3600;

/// The furthest a reset of a meter of this `window` can lie ahead of `now_s` (a month as its
/// longest, 31 days); none for a window of a length not said.
pub fn reset_ahead_limit(now_s: i64, window: Window) -> Option<i64> {
    let hours = if window == Window::Month { 744 } else { window.hours()? };
    Some(now_s + i64::from(hours) * 3600)
}

/// `reset` when it is a reset time of a meter of this `window` read at `now_s`: in
/// (`now_s` - [`RESET_GRACE_S`], `now_s` + the window]. A reset in the past within the grace is the
/// meter resetting now, and stays as it is. Any other is no reset time. The one place the range
/// is judged, for every harness.
pub fn reset_in_range(reset: i64, now_s: i64, window: Window) -> Option<i64> {
    let ahead = reset_ahead_limit(now_s, window).is_none_or(|limit| reset <= limit);
    (reset > now_s - RESET_GRACE_S && ahead).then_some(reset)
}

/// The moment matching "resets [Mon D,] H[:MM]am|pm [(Zone)]" or "resets in 6d23h" that is a reset
/// of a meter of `window` ([`reset_in_range`]): the earliest candidate in range, so a reset that
/// passed a minute ago stays that moment, not next year or tomorrow. None when no candidate is in
/// range. A time with no zone is in this machine's, as the CLI printed it.
#[cfg(test)]
pub fn parse_reset(text: &str, now_s: i64) -> Option<i64> {
    parse_reset_in(text, now_s, &TimeZone::system(), None)
}

/// [`parse_reset`], with `local` as the zone of a time that names none: the zone of the process
/// that printed it ([`zone_of`]). `window: None` is for a limit message, which names a reset
/// still to come, of no meter: the next moment after `now_s`, however far.
pub fn parse_reset_in(text: &str, now_s: i64, local: &TimeZone, window: Option<Window>) -> Option<i64> {
    // The window of a limit message is unknown and its reset is ahead: no grace, no upper bound.
    let keep = |r: i64| match window {
        Some(w) => reset_in_range(r, now_s, w),
        None => (r > now_s).then_some(r),
    };
    if let Some(c) = RESETS_IN.captures(text) {
        return keep(now_s + duration_seconds(&c[1]));
    }
    let c = RESET.captures(text)?;
    let tz = c.name("tz").map_or(Some(local.clone()), |z| match z.as_str() {
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
    let at = |date: Date| date.to_datetime(time).to_zoned(tz.clone()).ok().map(|z| z.timestamp().as_second());
    let mut candidates: Vec<i64> = if let (Some(mon), Some(day)) = (c.name("mon"), c.name("day")) {
        let month = month_number(mon.as_str())?;
        let day: i8 = day.as_str().parse().ok()?;
        let year = now.year();
        [year - 1, year, year + 1].into_iter().filter_map(|y| Date::new(y, month, day).ok().and_then(at)).collect()
    } else {
        let today = now.date();
        [today.yesterday().ok()?, today, today.tomorrow().ok()?].into_iter().filter_map(at).collect()
    };
    candidates.sort_unstable();
    candidates.into_iter().find_map(keep)
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
    const OK: Ending = Ending { exit_ok: true, timed_out: false, silent: false, over_budget: false, tree_ended: true };

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

    #[test]
    fn a_reset_with_no_zone_is_in_the_local_zone() {
        let dhaka = TimeZone::get("Asia/Dhaka").unwrap();
        // 21:00 in Dhaka (UTC+6): 3pm there is 09:00 UTC tomorrow.
        assert_eq!(parse_reset_in("resets 3pm", NOW, &dhaka, None), Some(NOW + 18 * 3600));
        assert_eq!(parse_reset_in("resets 4pm (UTC)", NOW, &dhaka, None), Some(NOW + 3600), "a stated zone wins");
        for tz in ["Etc/GMT-6", ":Etc/GMT-6", "UTC-6"] {
            assert_eq!(parse_reset_in("resets 3pm", NOW, &zone_of(Some(tz)), None), Some(NOW + 18 * 3600), "{tz}");
        }
        assert_eq!(zone_of(Some("")), TimeZone::UTC);
    }

    /// Review of #51: a run's own limit message with a bare reset time is read in the zone the run
    /// kept its clock in, not in relay's.
    #[test]
    fn a_runs_bare_reset_time_is_in_the_runs_zone() {
        let dhaka = Report { zone: Some(zone_of(Some("Asia/Dhaka"))), ..error("You've hit your limit · resets 3pm") };
        // 21:00 in Dhaka (UTC+6): 3pm there is 09:00 UTC tomorrow.
        assert_eq!(verdict(&dhaka, OK, "FINAL", false, NOW), Verdict::Limited { until: Some(NOW + 18 * 3600) });
        let utc = Report { zone: Some(TimeZone::UTC), ..error("You've hit your limit · resets 4pm") };
        assert_eq!(verdict(&utc, OK, "FINAL", false, NOW), Verdict::Limited { until: Some(NOW + 3600) });
    }

    const UTC: &TimeZone = &TimeZone::UTC;

    /// 2026-11-13T01:00 +06, on the real bus: the reset that had just passed read a year out.
    #[test]
    fn a_reset_that_just_passed_stays_in_range_of_its_window() {
        // Undated, a 5-hour meter, read at 7:01pm: 7pm is a minute ago, not tomorrow.
        let seven_oh_one = NOW + 4 * 3600 + 60;
        assert_eq!(
            parse_reset_in("resets 7pm (UTC)", seven_oh_one, UTC, Some(Window::FiveHours)),
            Some(NOW + 4 * 3600)
        );
        // The same time read an hour and a minute later is no reset of that meter at all: tomorrow is
        // further than five hours.
        let later = NOW + 5 * 3600 + 60;
        assert_eq!(parse_reset_in("resets 7pm (UTC)", later, UTC, Some(Window::FiveHours)), None);
        // A dated week reset one minute past (Nov 2, 3pm; read at 15:01).
        assert_eq!(
            parse_reset_in("resets Nov 2, 3pm (UTC)", NOW + 60, UTC, Some(Window::Week)),
            Some(NOW),
            "not Nov 2 of next year"
        );
        // Across a year's end: read at 00:01 on Jan 1, a reset of Dec 31 11:30pm is a minute-old reset.
        let jan1 = 1_798_761_660; // 2027-01-01T00:01:00Z
        assert_eq!(parse_reset_in("resets Dec 31, 11:30pm (UTC)", jan1, UTC, Some(Window::Week)), Some(jan1 - 1860));
        // Past the grace it is no reset time, and a candidate beyond the window is none either.
        assert_eq!(parse_reset_in("resets Nov 2, 3pm (UTC)", NOW + RESET_GRACE_S + 1, UTC, Some(Window::Week)), None);
        assert_eq!(parse_reset_in("resets Nov 20, 3pm (UTC)", NOW, UTC, Some(Window::Week)), None);
        assert_eq!(parse_reset_in("resets in 6d23h", NOW, UTC, Some(Window::FiveHours)), None);
        assert_eq!(parse_reset_in("resets in 4h59m", NOW, UTC, Some(Window::FiveHours)), Some(NOW + 17_940));
        // A month is up to 31 days.
        assert_eq!(parse_reset_in("resets in 31d", NOW, UTC, Some(Window::Month)), Some(NOW + 31 * 86_400));
        // The range itself, for readings that state a time: the edges.
        assert_eq!(reset_in_range(NOW - RESET_GRACE_S, NOW, Window::Week), None);
        assert_eq!(reset_in_range(NOW - RESET_GRACE_S + 1, NOW, Window::Week), Some(NOW - RESET_GRACE_S + 1));
        assert_eq!(reset_in_range(NOW + 168 * 3600, NOW, Window::Week), Some(NOW + 168 * 3600));
        assert_eq!(reset_in_range(NOW + 168 * 3600 + 1, NOW, Window::Week), None);
        assert_eq!(reset_in_range(NOW + 10_000 * 3600, NOW, Window::Other(None)), Some(NOW + 10_000 * 3600));
    }
}
