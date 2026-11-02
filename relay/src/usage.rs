//! Reading quota from `claude -p /usage`, which answers locally with no model
//! call, and the reset times Claude writes in messages
//! ("resets 4pm (UTC)", "resets Nov 6, 5am (UTC)").

use std::sync::LazyLock;

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use regex::Regex;

use crate::bus::{Window, Windows};

static LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Current (week \(all models\)|session):\s+(\d+)% used(?:\s+·\s+resets\s+(.+?))?\s*$").unwrap()
});
static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
static RESET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)resets\s+(?:(?P<mon>[A-Z][a-z]{2})\s+(?P<day>\d{1,2}),?\s+)?(?:at\s+)?(?P<h>\d{1,2})(?::(?P<m>\d{2}))?\s*(?P<ap>am|pm)\s*(?:\((?P<tz>[^)]+)\))?").unwrap()
});

/// Windows from `/usage` output, keyed like `rate_limit_event`'s (`seven_day`, `five_hour`).
pub fn parse_usage(text: &str, now_s: i64) -> Windows {
    let clean = ANSI.replace_all(text, "");
    let mut out = Windows::new();
    for line in clean.lines() {
        let Some(c) = LINE.captures(line.trim()) else { continue };
        let key = if c[1].starts_with("week") { "seven_day" } else { "five_hour" };
        let used: f64 = c[2].parse().unwrap_or(0.0);
        let resets_at = c.get(3).and_then(|r| parse_reset(&format!("resets {}", r.as_str()), now_s));
        out.insert(key.into(), Window { utilization: used / 100.0, resets_at });
    }
    out
}

/// The next moment matching "resets [Mon D,] H[:MM]am|pm [(Zone)]" after `now_s`.
pub fn parse_reset(text: &str, now_s: i64) -> Option<i64> {
    let c = RESET.captures(text)?;
    let tz = c.name("tz").map_or(Some(TimeZone::UTC), |z| match z.as_str() {
        "UTC" | "GMT" => Some(TimeZone::UTC),
        name => TimeZone::get(name).ok(),
    })?;
    let mut hour: i8 = c["h"].parse().ok()?;
    let minute: i8 = c.name("m").map_or(Some(0), |m| m.as_str().parse().ok())?;
    let pm = c["ap"].eq_ignore_ascii_case("pm");
    if hour == 12 {
        hour = 0;
    }
    if pm {
        hour += 12;
    }
    let time = Time::new(hour, minute, 0, 0).ok()?;
    let now = Timestamp::from_second(now_s).ok()?.to_zoned(tz.clone());
    let candidate = |date: Date| date.to_datetime(time).to_zoned(tz.clone()).ok();
    let after = |z: &Zoned| z.timestamp().as_second() > now_s;
    let found = if let (Some(mon), Some(day)) = (c.name("mon"), c.name("day")) {
        let month = month_number(mon.as_str())?;
        let day: i8 = day.as_str().parse().ok()?;
        [now.year(), now.year() + 1]
            .into_iter()
            .filter_map(|y| Date::new(y, month, day).ok().and_then(candidate))
            .find(after)
    } else {
        [now.date(), now.date().tomorrow().ok()?].into_iter().filter_map(candidate).find(after)
    };
    found.map(|z| z.timestamp().as_second())
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

    #[test]
    fn usage_output() {
        let text = "You are currently using your subscription\n\n\x1b[1mCurrent week (all models): 24% used · resets Nov 8, 3pm (UTC)\x1b[0m\nCurrent week (Fable): 0% used · resets Nov 8, 3pm (UTC)\nCurrent session: 61% used · resets 7pm (UTC)\n";
        let w = parse_usage(text, NOW);
        assert_eq!(w.len(), 2);
        assert!((w["seven_day"].utilization - 0.24).abs() < 1e-9);
        assert_eq!(w["seven_day"].resets_at, Some(NOW + 6 * 86_400));
        assert_eq!(w["five_hour"].resets_at, Some(NOW + 4 * 3600));
    }

    #[test]
    fn resets() {
        assert_eq!(parse_reset("resets 4pm (UTC)", NOW), Some(NOW + 3600));
        assert_eq!(parse_reset("resets 2pm (UTC)", NOW), Some(NOW + 23 * 3600), "a past time today means tomorrow");
        assert_eq!(parse_reset("resets 12am (UTC)", NOW), Some(NOW + 9 * 3600));
        assert_eq!(parse_reset("resets 3:30pm (UTC)", NOW), Some(NOW + 1800));
        assert_eq!(parse_reset("resets Jan 2, 3pm (UTC)", NOW), Some(1_798_902_000), "rolls into next year");
        assert_eq!(parse_reset("no reset here", NOW), None);
    }
}
