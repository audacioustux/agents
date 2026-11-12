//! `relay view usage`: each account's quota over time, as a chart printed inline. A read-only fold
//! over the readings already on the bus (`QuotaEvent::Reading`); it writes nothing and never starts a
//! reading. Meters only: who an account is signed in as is never read here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::Result;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph, Widget};
use serde::Serialize;
use serde_json::json;

use crate::bus::{Event, QuotaEvent, Stamped, now_micros};
use crate::config::Limits;
use crate::systems::quota::gauge;

use super::machine::Scope;

/// Which readings a chart shows.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// One meter by name, as the reading names it (`five_hour`) or as a board shows it (`5 hours`);
    /// else each account's 7-day meters ([`gauge::weekly`]).
    pub meter: Option<String>,
    /// Only these accounts (`harness:label`); every one when empty.
    pub accounts: Vec<String>,
    /// Only these harnesses; every one when empty.
    pub harnesses: Vec<String>,
    /// Readings from this bus time on (microseconds).
    pub since: u64,
}

impl Filter {
    fn meter(&self, name: &str) -> bool {
        match &self.meter {
            Some(m) => name == m || gauge::heading(name) == *m,
            None => gauge::weekly(name),
        }
    }

    fn account(&self, key: &str) -> bool {
        let harness = key.split(':').next().unwrap_or(key);
        (self.accounts.is_empty() || self.accounts.iter().any(|a| a == key))
            && (self.harnesses.is_empty() || self.harnesses.iter().any(|h| h == harness))
    }
}

/// One meter as one reading saw it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Point {
    /// Bus time, microseconds.
    pub t: u64,
    /// A fraction, 0 to 1.
    pub utilization: f64,
    /// Unix seconds, as the meter says.
    pub resets_at: Option<i64>,
}

/// Every account's meters over time: account, then meter, then its readings oldest first.
#[derive(Debug, Default)]
pub struct History {
    pub series: BTreeMap<String, BTreeMap<String, Vec<Point>>>,
    /// Accounts named by a bare label on older buses (`adventor`): no rule says which harness they
    /// are on, so their readings are left out.
    pub bare: BTreeSet<String>,
}

impl History {
    /// The readings on `buses` that `f` lets through. The same account read by several projects is
    /// one series, merged by time; a reading on two buses alike is kept once.
    pub fn fold<'a>(buses: impl IntoIterator<Item = &'a [Stamped]>, f: &Filter) -> Self {
        let mut h = Self::default();
        for s in buses.into_iter().flatten().filter(|s| s.t >= f.since) {
            let Event::Quota(QuotaEvent::Reading { account, meters, .. }) = &s.event else { continue };
            if !account.contains(':') {
                h.bare.insert(account.clone());
                continue;
            }
            if !f.account(account) {
                continue;
            }
            for (name, m) in meters.iter().filter(|(name, _)| f.meter(name)) {
                let point = Point { t: s.t, utilization: m.utilization, resets_at: m.resets_at };
                h.series.entry(account.clone()).or_default().entry(name.clone()).or_default().push(point);
            }
        }
        for points in h.series.values_mut().flat_map(BTreeMap::values_mut) {
            points.sort_by(|a, b| {
                a.t.cmp(&b.t).then(a.utilization.total_cmp(&b.utilization)).then(a.resets_at.cmp(&b.resets_at))
            });
            points.dedup();
        }
        h
    }

    /// Each series, as (account, meter, readings).
    fn each(&self) -> impl Iterator<Item = (&str, &str, &[Point])> {
        self.series.iter().flat_map(|(a, ms)| ms.iter().map(move |(m, p)| (a.as_str(), m.as_str(), p.as_slice())))
    }

    /// The `--json` reply: per account and meter, the readings as `{t, utilization, resets_at}`.
    pub fn to_json(&self, f: &Filter, limits: &Limits) -> serde_json::Value {
        let mut v = json!({
            "since": f.since,
            "meter": f.meter,
            "floor": limits.floor,
            "accounts": self.series,
        });
        if let Some(n) = limits.floor_near_reset {
            v["floor_near_reset"] = json!({ "within_hours": n.within_hours, "floor": n.floor });
        }
        if !self.bare.is_empty() {
            v["left_out"] = json!(self.bare);
        }
        v
    }
}

/// The colours the series take in turn: twelve, so a machine's accounts rarely share one.
const PALETTE: [Color; 12] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::Blue,
    Color::LightRed,
    Color::LightCyan,
    Color::LightMagenta,
    Color::LightYellow,
    Color::LightGreen,
    Color::LightBlue,
    Color::Red,
];
/// The chart's rows, its axes and labels included.
const CHART_ROWS: u16 = 18;

/// One series as the chart draws it: its colour, its points (seconds, percent) and its legend.
#[derive(Debug)]
struct Drawn {
    color: Color,
    data: Vec<(f64, f64)>,
    label: String,
}

/// What one chart draws: `history` from `since` to `now` (bus time), its times in `tz`, the floors by `limits`.
#[derive(Debug)]
pub struct Usage<'a> {
    pub history: &'a History,
    pub limits: &'a Limits,
    pub filter: &'a Filter,
    pub now: u64,
    pub tz: &'a TimeZone,
}

impl Usage<'_> {
    /// The rows it takes: a heading, the chart, then one legend line per series and per floor.
    pub fn height(&self) -> u16 {
        let lines = self.history.each().count() + 1 + usize::from(self.limits.floor_near_reset.is_some());
        1 + CHART_ROWS + u16::try_from(lines).unwrap_or(u16::MAX - CHART_ROWS - 1)
    }

    fn time(&self, us: u64, fmt: &str) -> String {
        i64::try_from(us)
            .ok()
            .and_then(|us| jiff::Timestamp::from_microsecond(us).ok())
            .map_or_else(String::new, |t| t.to_zoned(self.tz.clone()).strftime(fmt).to_string())
    }

    fn heading(&self) -> String {
        let meter = self.filter.meter.as_deref().map_or_else(|| "7-day meters".to_owned(), |m| format!("meter {m}"));
        format!("quota since {}, {meter}", self.time(self.filter.since, "%b %-d %H:%M %Z"))
    }

    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        #[allow(clippy::cast_precision_loss, reason = "bus time in seconds fits an f64 exactly")]
        let x = |us: u64| (us / 1_000_000) as f64;
        let (x0, x1) = (x(self.filter.since), x(self.now).max(x(self.filter.since) + 1.0));
        let floor = |f: f64| vec![(x0, f * 100.0), (x1, f * 100.0)];
        let floors: Vec<(String, Vec<(f64, f64)>)> =
            std::iter::once((format!("floor {:.0}%", self.limits.floor * 100.0), floor(self.limits.floor)))
                .chain(self.limits.floor_near_reset.map(|n| {
                    (
                        format!("floor {:.0}% in the last {}h before a reset", n.floor * 100.0, n.within_hours),
                        floor(n.floor),
                    )
                }))
                .collect();
        let fmt = "%b %-d %H:%M";
        let series: Vec<Drawn> = self
            .history
            .each()
            .enumerate()
            .map(|(i, (account, meter, points))| {
                let last = points.last().map_or_else(String::new, |p| {
                    let reset = p.resets_at.map_or_else(String::new, |r| {
                        format!(", resets {}", self.time(u64::try_from(r).unwrap_or(0) * 1_000_000, fmt))
                    });
                    format!("  {:.0}% at the last reading{reset}, {} reading(s)", p.utilization * 100.0, points.len())
                });
                Drawn {
                    color: PALETTE[i % PALETTE.len()],
                    data: points.iter().map(|p| (x(p.t), (p.utilization * 100.0).clamp(0.0, 100.0))).collect(),
                    label: format!("{account} {}{last}", gauge::heading(meter)),
                }
            })
            .collect();

        let mut rows = area;
        Paragraph::new(self.heading()).render(Rect { height: 1, ..rows }, buf);
        rows.y += 1;
        rows.height = rows.height.saturating_sub(1);

        // The floors first, so an account's line is drawn over them.
        let dim = Style::new().fg(Color::DarkGray);
        let floor_marks = [Marker::Custom('┄'), Marker::Custom('╌')];
        let mut datasets: Vec<Dataset<'_>> = floors
            .iter()
            .zip(floor_marks)
            .map(|((_, d), mark)| Dataset::default().marker(mark).graph_type(GraphType::Line).style(dim).data(d))
            .collect();
        datasets.extend(series.iter().map(|d| {
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(d.color))
                .data(&d.data)
        }));
        let mut labels = vec![self.time(self.filter.since, fmt)];
        if area.width >= 60 {
            labels.push(self.time(u64::midpoint(self.filter.since, self.now), fmt));
        }
        labels.push(self.time(self.now, &format!("{fmt} %Z")));
        let chart = Chart::new(datasets)
            .legend_position(None)
            .x_axis(Axis::default().bounds([x0, x1]).labels(labels).style(dim))
            .y_axis(Axis::default().bounds([0.0, 100.0]).labels(["0%", "50%", "100%"]).style(dim));
        chart.render(Rect { height: CHART_ROWS.min(rows.height), ..rows }, buf);
        rows.y += CHART_ROWS;
        rows.height = rows.height.saturating_sub(CHART_ROWS);

        let mut legend: Vec<Line<'_>> = series
            .iter()
            .map(|d| Line::from(vec![Span::styled("━━ ", Style::new().fg(d.color)), Span::raw(d.label.as_str())]))
            .collect();
        legend.extend(floors.iter().zip(['┄', '╌']).map(|((name, _), mark)| {
            Line::from(vec![Span::styled(format!("{mark}{mark} "), dim), Span::raw(name.clone())])
        }));
        Paragraph::new(legend).render(rows, buf);
    }
}

/// `buf` as lines of text, trailing blanks dropped; with `colour`, each change of colour as an SGR escape.
pub fn text(buf: &Buffer, colour: bool) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut fg = Color::Reset;
        for x in area.left()..area.right() {
            let cell = &buf[(x, y)];
            if colour && cell.fg != fg {
                fg = cell.fg;
                let _ = write!(line, "\x1b[{}m", sgr(fg));
            }
            line.push_str(cell.symbol());
        }
        if colour && fg != Color::Reset {
            line.push_str("\x1b[39m");
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let trimmed = out.trim_end_matches('\n').len();
    out.truncate(trimmed);
    out
}

/// The SGR foreground code of one of the colours a chart uses.
fn sgr(c: Color) -> u8 {
    match c {
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
        _ => 39,
    }
}

/// The buses a chart reads: the project's, in one; every project's on this machine outside one,
/// with each that cannot be read named.
fn buses(scope: Scope<'_>) -> Result<(Vec<Vec<Stamped>>, Vec<String>)> {
    if let Some(ctx) = scope.home {
        return Ok((vec![ctx.events()?], Vec::new()));
    }
    let (mut read, mut unreadable) = (Vec::new(), Vec::new());
    let mut dirs = crate::ctx::try_project_dirs(scope.root)?;
    dirs.sort();
    for d in dirs {
        match crate::bus::read_all(&d.join("bus")) {
            Ok(events) => read.push(events),
            Err(e) => {
                let name = d.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                unreadable.push(format!("{name}: {e:#}"));
            }
        }
    }
    Ok((read, unreadable))
}

pub fn run(scope: Scope<'_>, json: bool, mut filter: Filter, since: &str) -> Result<()> {
    if let Some(a) = filter.accounts.iter().find(|a| !a.contains(':')) {
        fail!(Usage, "--account {a}: an account is harness:label");
    }
    let Some(window) = super::stats::parse_duration(since) else {
        fail!(Usage, "--since {since}: a duration such as 30m, 12h, 7d or 2w");
    };
    let now = now_micros();
    filter.since = now.saturating_sub(window);
    let (read, unreadable) = buses(scope)?;
    let history = History::fold(read.iter().map(Vec::as_slice), &filter);
    let limits = &scope.cfg.limits;
    if json {
        let mut v = history.to_json(&filter, limits);
        if !unreadable.is_empty() {
            v["unreadable"] = unreadable.into();
        }
        out!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let size = crate::os::terminal_size();
    let width = size.map_or(100, |(cols, _)| cols);
    let tz = TimeZone::system();
    let usage = Usage { history: &history, limits, filter: &filter, now, tz: &tz };
    if history.series.is_empty() {
        out!("{}: no readings", usage.heading());
    } else {
        let area = Rect::new(0, 0, u16::try_from(width).unwrap_or(u16::MAX).max(40), usage.height());
        let mut buf = Buffer::empty(area);
        usage.draw(area, &mut buf);
        out!("{}", text(&buf, size.is_some()));
    }
    if !history.bare.is_empty() {
        let names = history.bare.iter().cloned().collect::<Vec<_>>().join(", ");
        out!("left out: readings under a bare label, which names no harness ({names})");
    }
    for u in unreadable {
        out!("unreadable: {u}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Meter, Meters};

    const H: u64 = 3600 * 1_000_000;
    const NOW: u64 = 1_800_000_000 * 1_000_000;

    fn reading(t: u64, account: &str, meters: &[(&str, f64, i64)]) -> Stamped {
        let meters: Meters =
            meters.iter().map(|&(n, u, r)| (n.to_owned(), Meter { utilization: u, resets_at: Some(r) })).collect();
        let event = Event::Quota(QuotaEvent::Reading { account: account.into(), meters, sign_in: Some("s".into()) });
        Stamped { t, event, from: None }
    }

    /// Two projects' buses: `claude:a` read by both (one reading on each alike), a reset on it, `omp:w`
    /// with its 7-day meter missing from one reading, and an old bare label.
    fn buses() -> [Vec<Stamped>; 2] {
        let week = 1_800_500_000;
        let one = vec![
            reading(NOW - 30 * H, "claude:a", &[("five_hour", 0.2, 1), ("seven_day", 0.80, week - 604_800)]),
            reading(NOW - 20 * H, "claude:a", &[("seven_day", 0.95, week - 604_800)]),
            reading(NOW - 20 * H, "adventor", &[("seven_day", 0.5, week)]),
            reading(NOW - 9 * 24 * H, "claude:a", &[("seven_day", 0.10, week - 2 * 604_800)]),
        ];
        let two = vec![
            reading(NOW - 20 * H, "claude:a", &[("seven_day", 0.95, week - 604_800)]),
            reading(NOW - 10 * H, "claude:a", &[("seven_day", 0.05, week)]),
            reading(NOW - 12 * H, "omp:w", &[("openai-codex/7 days", 0.4, week), ("openai-codex/5 hours", 0.1, 1)]),
            reading(NOW - 6 * H, "omp:w", &[("openai-codex/5 hours", 0.3, 1)]),
            reading(NOW - 2 * H, "omp:w", &[("openai-codex/7 days", 0.5, week)]),
        ];
        [one, two]
    }

    fn week() -> Filter {
        Filter { since: NOW - 7 * 24 * H, ..Filter::default() }
    }

    fn fold(f: &Filter) -> History {
        let b = buses();
        History::fold(b.iter().map(Vec::as_slice), f)
    }

    #[test]
    fn readings_fold_into_one_series_per_account_and_meter_across_projects() {
        let h = fold(&week());
        let a = &h.series["claude:a"]["seven_day"];
        let used: Vec<(u64, f64)> = a.iter().map(|p| ((NOW - p.t) / H, p.utilization)).collect();
        assert_eq!(used, [(30, 0.80), (20, 0.95), (10, 0.05)], "merged by time, the duplicate once, the reset a drop");
        assert_eq!(h.series["claude:a"].len(), 1, "the 7-day meter alone by default: {:?}", h.series["claude:a"]);
        let w = &h.series["omp:w"]["openai-codex/7 days"];
        assert_eq!(w.len(), 2, "the reading without that meter adds no point");
        assert_eq!(h.bare.iter().collect::<Vec<_>>(), ["adventor"], "a bare label is left out, and named");
        assert!(!h.series.contains_key("adventor"));
    }

    #[test]
    fn a_meter_account_and_harness_can_be_picked() {
        let h = fold(&Filter { meter: Some("5 hours".into()), ..week() });
        assert_eq!(h.series.keys().collect::<Vec<_>>(), ["claude:a"], "a meter by its heading");
        let h = fold(&Filter { meter: Some("openai-codex/5 hours".into()), ..week() });
        assert_eq!(h.series["omp:w"]["openai-codex/5 hours"].len(), 2, "or by its name");
        let h = fold(&Filter { harnesses: vec!["omp".into()], ..week() });
        assert_eq!(h.series.keys().collect::<Vec<_>>(), ["omp:w"]);
        let h = fold(&Filter { accounts: vec!["claude:a".into()], ..week() });
        assert_eq!(h.series.keys().collect::<Vec<_>>(), ["claude:a"]);
        let h = fold(&Filter { since: NOW - 15 * H, ..week() });
        assert_eq!(h.series["claude:a"]["seven_day"].len(), 1, "--since bounds it");
    }

    #[test]
    fn json_lists_each_accounts_meters_readings() {
        let limits = Limits {
            floor_near_reset: Some(crate::config::NearReset { within_hours: 24.0, floor: 0.98 }),
            ..Limits::default()
        };
        let f = week();
        let v = fold(&f).to_json(&f, &limits);
        let first = &v["accounts"]["claude:a"]["seven_day"][0];
        assert_eq!(first, &json!({ "t": NOW - 30 * H, "utilization": 0.80, "resets_at": 1_800_500_000 - 604_800 }));
        assert_eq!(v["accounts"]["omp:w"]["openai-codex/7 days"].as_array().unwrap().len(), 2);
        assert_eq!((v["floor"].clone(), v["floor_near_reset"]["floor"].clone()), (json!(limits.floor), json!(0.98)));
        assert_eq!(v["left_out"], json!(["adventor"]));
        assert_eq!(v["since"], json!(f.since));
        let text = v.to_string();
        assert!(!text.contains("sign_in") && !text.contains('@'), "meters only: {text}");
    }

    #[test]
    fn the_chart_has_its_axes_legend_and_floors() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let limits = Limits {
            floor: 0.5,
            floor_near_reset: Some(crate::config::NearReset { within_hours: 24.0, floor: 0.98 }),
            ..Limits::default()
        };
        let f = week();
        let h = fold(&f);
        let tz = TimeZone::UTC;
        let usage = Usage { history: &h, limits: &limits, filter: &f, now: NOW, tz: &tz };
        let mut t = Terminal::new(TestBackend::new(100, usage.height())).unwrap();
        t.draw(|frame| usage.draw(frame.area(), frame.buffer_mut())).unwrap();
        let text = text(t.backend().buffer(), false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "quota since Jan 8 08:00 UTC, 7-day meters", "{text}");
        assert!(lines.iter().any(|l| l.starts_with("100%")) && lines.iter().any(|l| l.starts_with("0%")), "{text}");
        let x = lines.iter().find(|l| l.contains("Jan 8 08:00") && l.contains("Jan 15 08:00 UTC")).expect(&text);
        assert!(x.contains("Jan 11 20:00"), "the middle of the window labelled: {x}");
        // The floor at 50% is a dotted row across the chart, half way up it.
        let floor_row = lines.iter().position(|l| l.matches('┄').count() > 40).expect(&text);
        let (top, bottom) = (
            lines.iter().position(|l| l.starts_with("100%")).unwrap(),
            lines.iter().position(|l| l.starts_with("0%")).unwrap(),
        );
        assert!(
            floor_row.abs_diff(usize::midpoint(top, bottom)) <= 1,
            "{floor_row} between {top} and {bottom}:\n{text}"
        );
        assert!(lines.iter().any(|l| l.matches('╌').count() > 40), "the near-reset floor too:\n{text}");
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "the series in braille:\n{text}");
        let legend = &lines[lines.len() - 4..];
        assert!(legend[0].starts_with("━━ claude:a 7 days  5% at the last reading, resets "), "{legend:?}");
        assert!(legend[1].starts_with("━━ omp:w openai-codex/7 days  50% at the last reading"), "{legend:?}");
        assert_eq!(legend[2..], ["┄┄ floor 50%", "╌╌ floor 98% in the last 24h before a reset"]);
        assert!(!text.contains('\x1b'), "plain text has no escapes");
    }

    #[test]
    fn on_a_terminal_the_colours_are_escapes() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf.set_string(0, 0, "ab", Style::new().fg(Color::Cyan));
        assert_eq!(text(&buf, true), "\x1b[36mab\x1b[39m");
        assert_eq!(text(&buf, false), "ab");
    }
}
