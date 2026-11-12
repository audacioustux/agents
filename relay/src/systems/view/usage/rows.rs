//! `view usage` as a table: one row per account and meter, each with its gauge, a sparkline of
//! where it has been, its pace over the last day and what that pace comes to by the reset.
//! Folded from the same [`Point`]s the chart draws.

use std::fmt::Write as _;

use ratatui::style::Color;
use serde_json::json;

use super::{History, Point};
use crate::config::Limits;
use crate::harness::{Meter, Window};
use crate::systems::quota::gauge::{self, Gauge, Tone};

/// The sparkline's cells, when there is room; it shrinks to [`MIN_SPARK`].
pub const SPARK: usize = 14;
pub const MIN_SPARK: usize = 6;
/// The readings the pace is taken over: this long back from now, in bus microseconds.
pub const PACE_WINDOW: u64 = 24 * HOUR;
/// The least time the readings of a pace must span: two readings a minute apart say nothing.
pub const MIN_SPAN: u64 = HOUR;
const HOUR: u64 = 3600 * 1_000_000;
const DAY_S: f64 = 86_400.0;

/// A level, one cell tall, in eighths.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The meter's level in each of `cells` equal buckets from `since` to `now`: the last reading at or
/// before the bucket's end (so a bucket with none repeats the one before it), and `None` before the
/// first reading. A reset is the drop it is: the bucket that holds the reading after it is lower.
pub fn buckets(points: &[Point], since: u64, now: u64, cells: usize) -> Vec<Option<f64>> {
    let width = (now.saturating_sub(since) / cells.max(1) as u64).max(1);
    let mut at = 0;
    let mut level = None;
    (0..cells)
        .map(|i| {
            let end = if i + 1 == cells { u64::MAX } else { since + (i as u64 + 1) * width };
            while at < points.len() && points[at].t < end {
                level = Some(points[at].utilization);
                at += 1;
            }
            level
        })
        .collect()
}

/// `buckets` as block characters, 0% the lowest and 100% the full block, a blank before any reading.
pub fn spark(levels: &[Option<f64>]) -> String {
    levels
        .iter()
        .map(|l| {
            l.map_or(' ', |u| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 7")]
                let i = (u.clamp(0.0, 1.0) * 7.0).round() as usize;
                BLOCKS[i.min(7)]
            })
        })
        .collect()
}

/// How fast the meter fills, as a fraction of its window per day, from the readings in the last
/// [`PACE_WINDOW`] since its last reset ([`new_window`]). A level that only jitters down is idle, not
/// negative. `None` when fewer than two readings remain, or they span under [`MIN_SPAN`].
pub fn pace(points: &[Point], now: u64) -> Option<f64> {
    // The last reset is looked for in every reading, for the reading that holds the old reset time
    // may be older than the window; only the readings of the window after it make the pace.
    let all: Vec<&Point> = points.iter().collect();
    let cutoff = now.saturating_sub(PACE_WINDOW);
    let run: Vec<&Point> = all[new_window(&all)..].iter().copied().filter(|p| p.t >= cutoff).collect();
    let (first, last) = (run.first()?, run.last()?);
    let span = last.t.checked_sub(first.t).filter(|s| *s >= MIN_SPAN)?;
    #[allow(clippy::cast_precision_loss, reason = "microseconds of a window fit an f64 exactly enough")]
    let days = span as f64 / 1e6 / DAY_S;
    Some(((last.utilization - first.utilization) / days).max(0.0))
}

/// How far a meter's reset time must move on between two readings for a new window to have begun:
/// the time jitters by about a minute between readings of one window.
const RESET_MOVE_S: i64 = 3600;

/// Where the last window begins in `readings`: at the first reading to confirm that the meter's
/// reset time moved forward by more than [`RESET_MOVE_S`] from the one before it that had a reset
/// time. The provider says so; a level that fell a point does not (it jitters), and a reading without
/// a reset time says nothing, so it is skipped over, not taken for a break. 0 when there is none.
fn new_window(readings: &[&Point]) -> usize {
    let mut known: Option<i64> = None;
    let mut from = 0;
    for (i, p) in readings.iter().enumerate() {
        let Some(r) = p.resets_at else { continue };
        if known.is_some_and(|k| r - k > RESET_MOVE_S) {
            from = i;
        }
        known = Some(r);
    }
    from
}

/// What the pace comes to before the reset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outlook {
    /// No pace, or no reset to project to.
    Unknown,
    /// The meter reaches its floor first, in this many seconds (0: it is there).
    Full(i64),
    /// It resets below its floor, at this level (a fraction), the rest unused.
    Unused(f64),
}

impl Outlook {
    /// `used` was read at `read_s` (unix seconds) and is `pace` a day faster, for a meter that
    /// resets at `resets_at`: against the floor in force at each time ([`Limits::floor_for`]).
    pub fn project(used: f64, pace: Option<f64>, resets_at: Option<i64>, read_s: i64, now_s: i64, l: &Limits) -> Self {
        let (Some(pace), Some(reset)) = (pace, resets_at.filter(|r| *r > now_s)) else { return Self::Unknown };
        if used >= l.floor_for(Some(reset), now_s).0 {
            return Self::Full(0);
        }
        #[allow(clippy::cast_precision_loss, reason = "seconds")]
        let (read, now, reset_f, rate) = (read_s as f64, now_s as f64, reset as f64, pace / DAY_S);
        let level = |t: f64| used + rate * (t - read);
        // The zones of floor, as (from, until, floor): the plain one, then the one before the reset.
        let near = l.floor_near_reset.map(|n| (reset_f - n.within_hours * 3600.0, n.floor));
        let edge = near.map_or(reset_f, |(from, _)| from.clamp(now, reset_f));
        let mut zones = vec![(now, edge, l.floor)];
        if let Some((from, floor)) = near {
            zones.push((from.clamp(now, reset_f), reset_f, floor));
        }
        for (from, until, floor) in zones {
            if from >= until {
                continue;
            }
            let at = if rate > 0.0 { read + (floor - used) / rate } else { f64::INFINITY };
            let at = at.max(from);
            if at < until {
                #[allow(clippy::cast_possible_truncation, reason = "seconds")]
                return Self::Full((at - now).ceil() as i64);
            }
        }
        Self::Unused(level(reset_f).clamp(0.0, 1.0))
    }

    /// `full in 1h at this pace`, `~41% at reset: 59% unused`, or `—`.
    pub fn text(self) -> String {
        match self {
            Self::Unknown => "—".into(),
            Self::Full(s) if s <= 0 => "full now".into(),
            Self::Full(s) => format!("full in {} at this pace", crate::accounts::fmt_left(s)),
            Self::Unused(level) => {
                let at = percent(level);
                format!("~{at}% at reset: {}% unused", 100 - at)
            }
        }
    }
}

/// A fraction as whole percent, 0 to 100.
fn percent(f: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation, reason = "0 to 100")]
    let p = (f.clamp(0.0, 1.0) * 100.0).round() as i64;
    p
}

/// What names a row: its account, the provider and the model or group its meter counts alone, and
/// its window unless that is the week's (`omp:w · google-antigravity └ Gemini`, `claude:a 5 hours`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Label {
    pub account: String,
    pub provider: Option<String>,
    pub scope: Option<String>,
    /// The window's words, when it is not the week's.
    pub window: Option<String>,
    /// The meter's own name, when another of the account's would read the same without it.
    pub name: Option<String>,
}

impl Label {
    pub fn of(account: &str, m: &Meter) -> Self {
        Self {
            account: account.to_owned(),
            provider: m.provider.clone(),
            scope: m.scope.clone(),
            window: (m.window != Window::Week).then(|| m.window.label()),
            name: None,
        }
    }

    pub fn text(&self) -> String {
        self.with(&self.account, self.provider.as_deref(), self.scope.as_deref())
    }

    fn with(&self, account: &str, provider: Option<&str>, scope: Option<&str>) -> String {
        let mut t = account.to_owned();
        if let Some(p) = provider {
            let _ = write!(t, " · {p}");
        }
        if let Some(s) = scope {
            let _ = write!(t, " └ {s}");
        }
        for w in self.window.iter().chain(&self.name) {
            let _ = write!(t, " {w}");
        }
        t
    }

    /// In `max` columns, where it can be: the provider is cut first, then the account, then the
    /// model or group, each at its end with `…` and no further than it must, so what tells two
    /// rows apart (the `└ Gemini` at the end) stays as long as it can.
    pub fn within(&self, max: usize) -> String {
        use unicode_width::UnicodeWidthStr;
        let mut parts = [self.provider.clone(), Some(self.account.clone()), self.scope.clone()];
        loop {
            let t = self.with(parts[1].as_deref().unwrap_or_default(), parts[0].as_deref(), parts[2].as_deref());
            if t.width() <= max {
                return t;
            }
            let Some(p) = parts.iter_mut().flatten().find(|p| p.trim_end_matches('…').chars().count() > 1) else {
                return t;
            };
            let mut kept: Vec<char> = p.trim_end_matches('…').chars().collect();
            kept.pop();
            *p = kept.into_iter().chain(['…']).collect();
        }
    }
}

/// One account's one meter.
#[derive(Debug, Clone)]
pub struct Row {
    pub account: String,
    pub meter: String,
    /// What it measures: its window, provider and scope.
    pub kind: Meter,
    pub name: Label,
    pub readings: usize,
    /// At the last reading.
    pub gauge: Gauge,
    /// Unix seconds.
    pub resets_at: Option<i64>,
    pub levels: Vec<Option<f64>>,
    /// A fraction of the window a day.
    pub pace: Option<f64>,
    pub outlook: Outlook,
}

impl Row {
    /// The sparkline in `cells` cells.
    pub fn spark(&self, cells: usize) -> String {
        spark(&resample(&self.levels, cells))
    }

    /// What names the row: the account, and what its meter counts as `quota show` says it
    /// ([`Label`]).
    pub fn label(&self) -> String {
        self.name.text()
    }

    /// `+14%/d`, or `—`.
    pub fn pace_text(&self) -> String {
        self.pace.map_or_else(|| "—".into(), |p| format!("{:+}%/d", percent_signed(p)))
    }

    /// `resets 19h`, `reset` once it has, or nothing when the meter says no time.
    pub fn resets_text(&self) -> String {
        match self.gauge.left {
            Some(s) if s > 0 => format!("resets {}", crate::accounts::fmt_left(s)),
            Some(_) => "reset".into(),
            None => String::new(),
        }
    }

    /// The gauge's colour: how close it is to its floor.
    pub fn tone(&self) -> Color {
        match self.gauge.tone() {
            Tone::Room => Color::Green,
            Tone::Near => Color::Yellow,
            Tone::Over => Color::Red,
        }
    }

    pub fn json(&self) -> serde_json::Value {
        let (full_in, at_reset) = match self.outlook {
            Outlook::Full(s) => (Some(s), None),
            Outlook::Unused(l) => (None, Some(l)),
            Outlook::Unknown => (None, None),
        };
        json!({
            "account": self.account,
            "meter": self.meter,
            "window": self.kind.window,
            "provider": self.kind.provider,
            "scope": self.kind.scope,
            "readings": self.readings,
            "utilization": self.gauge.used,
            "floor": self.gauge.floor,
            "resets_at": self.resets_at,
            "pace_per_day": self.pace,
            "projected_at_reset": at_reset,
            "full_in_seconds": full_in,
            "outlook": self.outlook.text(),
            "sparkline": spark(&self.levels),
        })
    }
}

/// A signed rate as whole percent.
fn percent_signed(f: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation, reason = "a rate of percent a day")]
    let p = (f * 100.0).round() as i64;
    p
}

/// `levels` in `cells` cells: each takes the level at the end of its share, so a narrower
/// sparkline is the same shape, not a different one.
fn resample(levels: &[Option<f64>], cells: usize) -> Vec<Option<f64>> {
    if cells >= levels.len() {
        return levels.to_vec();
    }
    (1..=cells).map(|i| levels[(i * levels.len()).div_ceil(cells) - 1]).collect()
}

/// The rows of `history`, the meters that reset soonest first (those that say no time last).
pub fn rows(history: &History, limits: &Limits, since: u64, now: u64) -> Vec<Row> {
    let now_s = i64::try_from(now / 1_000_000).unwrap_or(i64::MAX);
    let mut rows: Vec<Row> = history
        .each()
        .filter_map(|(account, meter, points)| {
            let last = points.last()?;
            let read_s = i64::try_from(last.t / 1_000_000).unwrap_or(i64::MAX);
            let pace = pace(points, now);
            let m = Meter { utilization: last.utilization, resets_at: last.resets_at, ..Meter::default() };
            let kind = history.kind(account, meter);
            Some(Row {
                name: Label::of(account, &kind),
                kind,
                account: account.to_owned(),
                meter: meter.to_owned(),
                readings: points.len(),
                gauge: Gauge::new(&m, Some(limits), false, now_s),
                resets_at: last.resets_at,
                levels: buckets(points, since, now, SPARK),
                pace,
                outlook: Outlook::project(last.utilization, pace, last.resets_at, read_s, now_s, limits),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let key = |r: &Row| (r.resets_at.is_none(), r.resets_at);
        key(a).cmp(&key(b)).then_with(|| (&a.account, &a.meter).cmp(&(&b.account, &b.meter)))
    });
    // Two meters that would read alike are told apart by their names.
    let texts: Vec<String> = rows.iter().map(Row::label).collect();
    for (i, r) in rows.iter_mut().enumerate() {
        if texts.iter().filter(|t| **t == texts[i]).count() > 1 {
            r.name.name = Some(format!("({})", r.meter));
        }
    }
    rows
}

/// A piece of a line and the colour it takes, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Styled {
    pub text: String,
    pub color: Option<Color>,
}

pub type Line = Vec<Styled>;

fn s(text: impl Into<String>, color: Option<Color>) -> Styled {
    Styled { text: text.into(), color }
}

fn width(line: &Line) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.iter().map(|p| p.text.width()).sum()
}

/// `text` padded (right-aligned with `right`) to `cols` columns.
fn pad(text: &str, cols: usize, right: bool) -> String {
    use unicode_width::UnicodeWidthStr;
    let fill = " ".repeat(cols.saturating_sub(text.width()));
    if right { format!("{fill}{text}") } else { format!("{text}{fill}") }
}

/// `line` cut to `cols` columns.
fn cut(line: Line, cols: usize) -> Line {
    use unicode_width::UnicodeWidthChar;
    let mut left = cols;
    let mut out = Line::new();
    for p in line {
        let mut text = String::new();
        for c in p.text.chars() {
            let w = c.width().unwrap_or(0);
            if w > left {
                left = 0;
                break;
            }
            left -= w;
            text.push(c);
        }
        out.push(Styled { text, color: p.color });
        if left == 0 {
            break;
        }
    }
    out
}

/// The most of a label the table keeps; the rest is cut with `…`.
const LABEL: usize = 30;

/// The rows as lines in `width` columns (the widest layout when none is known): the gauge's bar
/// shrinks first (to [`gauge::MIN_BAR`]), then the sparkline (to [`MIN_SPARK`]); when even that is
/// too wide, each outlook goes on a line of its own below its row, and a line is cut only past that.
pub fn table(rows: &[Row], width_of: Option<usize>) -> Vec<Line> {
    use unicode_width::UnicodeWidthStr;
    // Each cut to fit, unless that makes two read alike: those stay whole.
    let short: Vec<String> = rows.iter().map(|r| r.name.within(LABEL)).collect();
    let labels: Vec<String> = rows
        .iter()
        .zip(&short)
        .map(|(r, l)| if short.iter().filter(|c| *c == l).count() > 1 { r.label() } else { l.clone() })
        .collect();
    let col = |f: &dyn Fn(&Row) -> String| rows.iter().map(|r| f(r).width()).max().unwrap_or(0);
    let (label_w, pace_w, resets_w) =
        (labels.iter().map(|l| l.width()).max().unwrap_or(0), col(&Row::pace_text), col(&Row::resets_text));
    let out_w = rows.iter().map(|r| r.outlook.text().width()).max().unwrap_or(0);
    // Each layout as (bar, spark), the widest first.
    let layouts = (gauge::MIN_BAR..=gauge::BAR)
        .rev()
        .map(|b| (b, SPARK))
        .chain((MIN_SPARK..SPARK).rev().map(|k| (gauge::MIN_BAR, k)))
        .collect::<Vec<_>>();
    let wide = |(bar, spark): (usize, usize), outlook: bool| {
        label_w + 2 + bar + 5 + 2 + spark + 2 + pace_w + 2 + resets_w + if outlook { 2 + out_w } else { 0 }
    };
    let fits = |outlook: bool| width_of.map_or(layouts.first(), |w| layouts.iter().find(|l| wide(**l, outlook) <= w));
    let (layout, split) = match fits(true) {
        Some(l) => (*l, false),
        None => (*fits(false).or(layouts.last()).expect("layouts"), true),
    };
    let (bar, cells) = layout;
    let mut lines = Vec::new();
    for (r, label) in rows.iter().zip(&labels) {
        let tone = Some(r.tone());
        let mut line = vec![
            s(pad(label, label_w, false), None),
            s("  ", None),
            s(format!("{} {:>3}%", r.gauge.bar(bar), percent(r.gauge.used)), tone),
            s("  ", None),
            s(pad(&r.spark(cells), cells, false), None),
            s("  ", None),
            s(pad(&r.pace_text(), pace_w, true), None),
            s("  ", None),
            s(pad(&r.resets_text(), resets_w, false), None),
        ];
        let colour = matches!(r.outlook, Outlook::Full(_)).then_some(Color::Red);
        let outlook = s(r.outlook.text(), colour);
        if split {
            lines.push(line);
            line = vec![s(" ".repeat(label_w + 2), None), outlook];
        } else {
            line.extend([s("  ", None), outlook]);
        }
        lines.push(line);
    }
    let lines = lines.into_iter().map(trim);
    match width_of {
        Some(w) => lines.map(|l| if width(&l) > w { cut(l, w) } else { l }).collect(),
        None => lines.collect(),
    }
}

/// `line` without trailing blanks.
fn trim(mut line: Line) -> Line {
    while let Some(last) = line.last_mut() {
        last.text.truncate(last.text.trim_end().len());
        if !last.text.is_empty() {
            break;
        }
        line.pop();
    }
    line
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::config::NearReset;

    const H: u64 = HOUR;
    const NOW: u64 = 1_800_000_000 * 1_000_000;
    const NOW_S: i64 = 1_800_000_000;

    fn p(hours_ago: u64, utilization: f64) -> Point {
        Point { t: NOW - hours_ago * H, utilization, resets_at: Some(NOW_S + 86_400) }
    }

    fn limits(floor: f64) -> Limits {
        Limits { floor, ..Limits::default() }
    }

    #[test]
    fn a_sparkline_is_the_level_in_each_bucket_and_a_reset_is_its_drop() {
        // Eight 3-hour buckets over a day; the readings climb, then the meter resets at hour 15.
        let points = [p(23, 0.0), p(17, 0.5), p(14, 0.95), p(5, 0.1), p(1, 0.25)];
        let levels = buckets(&points, NOW - 24 * H, NOW, 8);
        let pct: Vec<Option<i64>> = levels.iter().map(|l| l.map(percent)).collect();
        assert_eq!(
            pct,
            [Some(0), Some(0), Some(50), Some(95), Some(95), Some(95), Some(10), Some(25)],
            "each bucket takes its last reading, a gap repeats the one before, the last bucket holds `now`"
        );
        assert_eq!(spark(&levels), "▁▁▅███▂▃", "0% the lowest block, a reset the drop it is");
    }

    #[test]
    fn a_bucket_before_the_first_reading_is_blank() {
        let levels = buckets(&[p(2, 0.5)], NOW - 24 * H, NOW, 4);
        assert_eq!(spark(&levels), "   ▅");
        assert_eq!(spark(&buckets(&[], NOW - 24 * H, NOW, 3)), "   ");
    }

    #[test]
    fn a_narrower_sparkline_keeps_the_shape() {
        let levels: Vec<Option<f64>> = (0..14).map(|i| Some(f64::from(i) / 13.0)).collect();
        assert_eq!(spark(&resample(&levels, 7)), "▂▃▄▅▆▇█");
        assert_eq!(resample(&levels, 14), levels);
    }

    #[test]
    fn the_pace_is_the_rise_over_the_last_day_in_a_day() {
        // 20% to 44% over twelve hours: 48% a day.
        let at = pace(&[p(30, 0.0), p(12, 0.20), p(6, 0.32), p(0, 0.44)], NOW).unwrap();
        assert!((at - 0.48).abs() < 1e-9, "{at}");
        // Only the readings in the last 24h count: the one at 30h is not one of them.
        let at = pace(&[p(30, 0.9), p(12, 0.20), p(0, 0.44)], NOW).unwrap();
        assert!((at - 0.48).abs() < 1e-9, "{at}");
    }

    #[test]
    fn a_reset_is_the_meters_reset_time_moving_on_not_the_level_falling() {
        let week = NOW_S + 86_400;
        let next = week + 7 * 86_400;
        // 80% to 100%, the meter's window moves a week on, then 0% to 6% in six hours:
        // the pace is the 6, not the fall.
        let reading = |h, u, r| reset(r, p(h, u));
        let at = pace(
            &[reading(20, 0.80, week), reading(14, 1.0, week), reading(6, 0.0, next), reading(0, 0.06, next)],
            NOW,
        )
        .unwrap();
        assert!((at - 0.24).abs() < 1e-9, "{at}");
        assert_eq!(
            pace(&[reading(10, 0.9, week), reading(0, 0.01, next)], NOW),
            None,
            "one reading since it says nothing"
        );
    }

    /// A reading may leave the reset time out: the rollover is then between the readings that have
    /// one, and the new window begins at the first of them to confirm it.
    #[test]
    fn a_reading_without_a_reset_time_does_not_hide_the_rollover() {
        let (week, next) = (NOW_S + 3600, NOW_S + 3600 + 7 * 86_400);
        let at = |h, u, r: Option<i64>| Point { resets_at: r, ..p(h, u) };
        let readings = [
            at(4, 0.80, Some(week)),
            at(3, 0.90, Some(week)),
            at(2, 0.10, None),
            at(1, 0.20, Some(next)),
            at(0, 0.30, Some(next)),
        ];
        let got = pace(&readings, NOW).unwrap();
        assert!((got - 2.4).abs() < 1e-9, "10 points in an hour is 240% a day, not +0%/d: {got}");
        // Several unknowns in a row, still between the two known times.
        let more = [
            at(5, 0.7, Some(week)),
            at(4, 0.9, None),
            at(3, 0.1, None),
            at(2, 0.2, Some(next)),
            at(0, 0.3, Some(next)),
        ];
        assert!((pace(&more, NOW).unwrap() - 1.2).abs() < 1e-9);
        // Unknown throughout: nothing says it rolled over, and the fall is not a negative pace.
        let blind = [at(3, 0.9, None), at(2, 0.1, None), at(0, 0.3, None)];
        assert!(pace(&blind, NOW).unwrap() >= 0.0);
    }

    /// The rollover may lie before the pace window: the reading that holds the old reset time can be
    /// older than a day, and the window must still see it.
    #[test]
    fn a_rollover_whose_old_reset_time_is_older_than_the_window_is_still_seen() {
        let (week, next) = (NOW_S + 3600, NOW_S + 3600 + 7 * 86_400);
        let at = |h, u, r: Option<i64>| Point { resets_at: r, ..p(h, u) };
        let readings =
            [at(25, 0.80, Some(week)), at(23, 0.90, None), at(20, 0.10, Some(next)), at(0, 0.30, Some(next))];
        let got = pace(&readings, NOW).unwrap();
        assert!((got - 0.24).abs() < 1e-9, "20 points in 20h is 24% a day, not +0%/d: {got}");
        // Only the readings in the last day count towards it, as before.
        let early =
            [at(30, 0.1, Some(next)), at(25, 0.8, Some(next)), at(20, 0.82, Some(next)), at(0, 0.94, Some(next))];
        assert!((pace(&early, NOW).unwrap() - 0.144).abs() < 1e-9);
    }

    /// Real readings jitter: the level by a point (0.69, 0.68, 0.69) and the reset time by a minute
    /// (1794754740, 1794754800). Neither is a reset, so the window keeps its first reading.
    #[test]
    fn a_point_of_jitter_in_the_level_or_a_minute_in_the_reset_time_is_not_a_reset() {
        let r = 1_794_754_740;
        let reading = |h, u, r| reset(r, p(h, u));
        let jittery = [
            reading(23, 0.60, r),
            reading(20, 0.69, r + 60),
            reading(16, 0.68, r),
            reading(12, 0.69, r + 60),
            reading(8, 0.68, r),
            reading(4, 0.69, r + 60),
            reading(0, 0.72, r),
        ];
        let at = pace(&jittery, NOW).expect("a pace, not a restart at every fall");
        let want = 0.12 / (23.0 / 24.0);
        assert!((at - want).abs() < 1e-9, "{at} for {want}");
        // A level that only ever jitters is idle, not falling.
        let flat = [reading(10, 0.69, r), reading(5, 0.68, r + 60), reading(0, 0.68, r)];
        assert_eq!(pace(&flat, NOW), Some(0.0));
    }

    #[test]
    fn too_few_readings_or_too_short_a_span_have_no_pace() {
        assert_eq!(pace(&[], NOW), None);
        assert_eq!(pace(&[p(0, 0.5)], NOW), None);
        assert_eq!(pace(&[p(30, 0.1), p(25, 0.5)], NOW), None, "none in the window");
        let close = [Point { t: NOW - 600_000_000, ..p(0, 0.5) }, p(0, 0.51)];
        assert_eq!(pace(&close, NOW), None, "ten minutes apart is noise");
    }

    #[test]
    fn at_this_pace_a_meter_that_reaches_its_floor_before_the_reset_is_full() {
        // 50% now, 10% an hour (240% a day), floor 90%, the reset in 3 days: full in 4 hours.
        let o = Outlook::project(0.5, Some(2.4), Some(NOW_S + 3 * 86_400), NOW_S, NOW_S, &limits(0.9));
        assert_eq!(o, Outlook::Full(4 * 3600));
        assert_eq!(o.text(), "full in 4h at this pace");
        // The reading is older than now: the time already gone counts.
        let o = Outlook::project(0.5, Some(2.4), Some(NOW_S + 3 * 86_400), NOW_S - 3600, NOW_S, &limits(0.9));
        assert_eq!(o.text(), "full in 3h at this pace");
        let at = Outlook::project(0.95, Some(0.1), Some(NOW_S + 86_400), NOW_S, NOW_S, &limits(0.9));
        assert_eq!(at.text(), "full now", "already past the floor");
    }

    #[test]
    fn at_this_pace_a_meter_that_resets_below_its_floor_leaves_the_rest_unused() {
        // 23% now, 3% a day, 4 days to the reset: 35% at the reset, 65% unused.
        let o = Outlook::project(0.23, Some(0.03), Some(NOW_S + 4 * 86_400), NOW_S, NOW_S, &limits(0.9));
        assert_eq!(o.text(), "~35% at reset: 65% unused");
        let idle = Outlook::project(0.23, Some(0.0), Some(NOW_S + 4 * 86_400), NOW_S, NOW_S, &limits(0.9));
        assert_eq!(idle.text(), "~23% at reset: 77% unused");
        // The floor near the reset is the higher one: 92% a day before it is not full yet.
        let near = Limits { floor_near_reset: Some(NearReset { within_hours: 24.0, floor: 0.98 }), ..limits(0.9) };
        let o = Outlook::project(0.80, Some(0.12), Some(NOW_S + 86_400), NOW_S, NOW_S, &near);
        assert_eq!(o.text(), "~92% at reset: 8% unused", "{o:?}");
        let o = Outlook::project(0.80, Some(0.12), Some(NOW_S + 86_400), NOW_S, NOW_S, &limits(0.9));
        assert_eq!(o.text(), "full in 20h at this pace", "without it, 90% is reached 20 hours on");
    }

    #[test]
    fn without_a_pace_or_a_reset_to_come_there_is_no_outlook() {
        let l = limits(0.9);
        assert_eq!(Outlook::project(0.5, None, Some(NOW_S + 3600), NOW_S, NOW_S, &l).text(), "—");
        assert_eq!(Outlook::project(0.5, Some(0.1), None, NOW_S, NOW_S, &l).text(), "—");
        assert_eq!(Outlook::project(0.5, Some(0.1), Some(NOW_S - 5), NOW_S, NOW_S, &l).text(), "—", "already past");
    }

    fn history(series: &[(&str, &str, Vec<Point>)]) -> History {
        let mut h = History::default();
        for (a, m, points) in series {
            h.series.entry((*a).to_owned()).or_default().insert((*m).to_owned(), points.clone());
            h.kinds.entry((*a).to_owned()).or_default().insert((*m).to_owned(), Meter::named(m, 0.0, None));
        }
        h
    }

    fn reset(r: i64, mut p: Point) -> Point {
        p.resets_at = Some(r);
        p
    }

    fn two() -> History {
        history(&[
            (
                "claude:late",
                "seven_day",
                vec![reset(NOW_S + 6 * 86_400, p(10, 0.2)), reset(NOW_S + 6 * 86_400, p(0, 0.23))],
            ),
            ("claude:soon", "seven_day", vec![reset(NOW_S + 3600, p(10, 0.8)), reset(NOW_S + 3600, p(0, 0.91))]),
            ("claude:never", "seven_day", vec![Point { resets_at: None, ..p(0, 0.3) }]),
            ("omp:w", "openai-codex/7 days", vec![reset(NOW_S + 100, p(0, 0.1))]),
        ])
    }

    #[test]
    fn rows_come_by_reset_time_the_soonest_first_and_those_with_none_last() {
        let rows = rows(&two(), &limits(0.9), NOW - 7 * 24 * H, NOW);
        let order: Vec<&str> = rows.iter().map(|r| r.account.as_str()).collect();
        assert_eq!(order, ["omp:w", "claude:soon", "claude:late", "claude:never"]);
        assert_eq!(rows[0].label(), "omp:w · openai-codex", "a provider's week is named by its provider");
        assert_eq!(rows[1].label(), "claude:soon");
    }

    /// The owner's ask of 2026-11-12: a row says its meter as `quota show` does, by provider,
    /// model or group and window, so omp's read `omp:w · google-antigravity └ Gemini`, and two
    /// meters never read alike, however narrow the table.
    #[test]
    fn a_row_names_its_meter_by_provider_scope_and_window() {
        let at = |r| vec![reset(NOW_S + r, p(0, 0.1))];
        let h = history(&[
            ("omp:default", "google-antigravity/Claude & GPT (shared) (Weekly)", at(100)),
            ("omp:default", "google-antigravity/Gemini (Weekly)", at(200)),
            ("omp:default", "openai-codex/5 hours", at(300)),
            ("claude:studyac17", "seven_day_fable", at(400)),
            ("claude:studyac17", "seven_day", at(500)),
        ]);
        let rows = rows(&h, &limits(0.9), NOW - 7 * 24 * H, NOW);
        let labels: Vec<String> = rows.iter().map(Row::label).collect();
        assert_eq!(
            labels,
            [
                "omp:default · google-antigravity └ Claude & GPT (shared)",
                "omp:default · google-antigravity └ Gemini",
                "omp:default · openai-codex 5 hours",
                "claude:studyac17 └ Fable",
                "claude:studyac17",
            ]
        );
        let text = |l: &Line| l.iter().map(|s| s.text.as_str()).collect::<String>();
        for width in [None, Some(100), Some(60)] {
            let shown: Vec<String> = table(&rows, width).iter().map(text).collect();
            let rows = shown.iter().filter(|l| !l.starts_with(' '));
            let names: Vec<&str> = rows.map(|l| l.split("  ").next().unwrap()).collect();
            let mut distinct = names.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(distinct.len(), names.len(), "{width:?}: {shown:#?}");
            assert!(names[1].ends_with("└ Gemini"), "the end that tells them apart stays: {names:?}");
        }
    }

    #[test]
    fn a_row_has_its_gauge_pace_and_outlook() {
        let rows = rows(&two(), &limits(0.9), NOW - 7 * 24 * H, NOW);
        let soon = &rows[1];
        assert_eq!(soon.pace_text(), "+26%/d");
        assert_eq!(soon.resets_text(), "resets 1h");
        assert_eq!(soon.outlook, Outlook::Full(0), "91% is past a floor of 90%");
        assert_eq!(rows[2].outlook.text(), "~66% at reset: 34% unused");
        assert_eq!(rows[3].outlook.text(), "—");
        assert_eq!(rows[3].pace_text(), "—");
        assert_eq!(rows[3].resets_text(), "");
    }

    fn plain(lines: Vec<Line>) -> Vec<String> {
        lines.into_iter().map(|l| l.into_iter().map(|p| p.text).collect()).collect()
    }

    #[test]
    fn the_table_aligns_its_columns_and_fits_its_width() {
        let rows = rows(&two(), &limits(0.9), NOW - 7 * 24 * H, NOW);
        let wide = plain(table(&rows, None));
        assert_eq!(wide.len(), 4, "{wide:#?}");
        assert!(wide[1].starts_with("claude:soon "), "{wide:#?}");
        assert!(wide[1].contains("│") && wide[1].contains('%'), "the gauge's floor mark: {wide:#?}");
        for w in [100, 70, 40, 24] {
            let lines = plain(table(&rows, Some(w)));
            assert!(lines.iter().all(|l| l.width() <= w), "{w} columns: {lines:#?}");
        }
        assert_eq!(table(&rows, Some(100)).len(), 4, "the bar gave way first, the outlook stays in its row");
        let narrow = plain(table(&rows, Some(70)));
        assert_eq!(narrow.len(), 8, "{narrow:#?}");
        assert!(narrow.iter().any(|l| l.trim() == "full now"), "the outlook on its own line: {narrow:#?}");
    }
}
