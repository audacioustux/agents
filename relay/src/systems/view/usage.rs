//! `relay view usage`: each account's quota over time, as a chart printed inline. A read-only fold
//! over the readings already on the bus (`QuotaEvent::Reading`); it writes nothing and never starts a
//! reading. Meters only: who an account is signed in as is never read here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::Result;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Widget};
use serde::Serialize;
use serde_json::json;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
/// The chart's rows, its axes and labels included: the heading and legend are lines of their own.
const CHART_ROWS: u16 = 18;

/// What names each series on the chart and in its legend, in turn: one character, so it takes
/// one cell beside the series' last point, and needs no colour to be read. Past the last, `*`.
const IDS: &str = "123456789abcdefghijklmnopqrstuvwxyz";

/// One series as the chart draws it: its id and colour, its points (seconds, percent) and its legend.
#[derive(Debug)]
struct Drawn {
    id: char,
    color: Color,
    data: Vec<(f64, f64)>,
    label: String,
}

/// A floor drawn across the chart: its legend, its mark and its level (a fraction).
#[derive(Debug)]
struct Floor {
    name: String,
    mark: char,
    level: f64,
}

/// Bus time (microseconds) as the chart's x: seconds.
#[allow(clippy::cast_precision_loss, reason = "bus time in seconds fits an f64 exactly")]
fn x(us: u64) -> f64 {
    (us / 1_000_000) as f64
}

/// The time format of the x labels and the legend.
const FMT: &str = "%b %-d %H:%M";
const DIM: Style = Style::new().fg(Color::DarkGray);

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

    fn floors(&self) -> Vec<Floor> {
        let l = self.limits;
        let floor = Floor { name: format!("floor {:.0}%", l.floor * 100.0), mark: '┄', level: l.floor };
        let near = l.floor_near_reset.map(|n| Floor {
            name: format!("floor {:.0}% in the last {}h before a reset", n.floor * 100.0, n.within_hours),
            mark: '╌',
            level: n.floor,
        });
        std::iter::once(floor).chain(near).collect()
    }

    fn series(&self) -> Vec<Drawn> {
        self.history
            .each()
            .enumerate()
            .map(|(i, (account, meter, points))| {
                let last = points.last().map_or_else(String::new, |p| {
                    let reset = p.resets_at.map_or_else(String::new, |r| {
                        format!(", resets {}", self.time(u64::try_from(r).unwrap_or(0) * 1_000_000, FMT))
                    });
                    format!("  {:.0}% at the last reading{reset}, {} reading(s)", p.utilization * 100.0, points.len())
                });
                Drawn {
                    id: IDS.chars().nth(i).unwrap_or('*'),
                    color: PALETTE[i % PALETTE.len()],
                    data: points.iter().map(|p| (x(p.t), (p.utilization * 100.0).clamp(0.0, 100.0))).collect(),
                    label: format!("{account} {}{last}", gauge::heading(meter)),
                }
            })
            .collect()
    }

    /// The axes' labels in `width` columns: the time at the start, middle and end of the window,
    /// fewer and shorter as the chart narrows, and none at all when it is very narrow, so the
    /// chart keeps room to draw in.
    fn labels(&self, width: u16) -> (Vec<String>, Vec<&'static str>) {
        let (since, now) = (self.filter.since, self.now);
        let x = match width {
            60.. => {
                vec![self.time(since, FMT), self.time(since.midpoint(now), FMT), self.time(now, &format!("{FMT} %Z"))]
            }
            40..60 => vec![self.time(since, FMT), self.time(now, &format!("{FMT} %Z"))],
            24..40 => vec![self.time(since, "%b %-d"), self.time(now, "%b %-d")],
            _ => Vec::new(),
        };
        let y = if width >= 16 { vec!["0%", "50%", "100%"] } else { Vec::new() };
        (x, y)
    }

    /// The chart alone (its axes, floors and series), filling `area`.
    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        let (x0, x1) = (x(self.filter.since), x(self.now).max(x(self.filter.since) + 1.0));
        let floors: Vec<(Marker, Vec<(f64, f64)>)> = self
            .floors()
            .iter()
            .map(|f| (Marker::Custom(f.mark), vec![(x0, f.level * 100.0), (x1, f.level * 100.0)]))
            .collect();
        let series = self.series();
        // The floors first, so an account's line is drawn over them.
        let mut datasets: Vec<Dataset<'_>> = floors
            .iter()
            .map(|(mark, d)| Dataset::default().marker(*mark).graph_type(GraphType::Line).style(DIM).data(d))
            .collect();
        datasets.extend(series.iter().map(|d| {
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(d.color))
                .data(&d.data)
        }));
        let (x_labels, y_labels) = self.labels(area.width);
        let graph = graph_area(area, &x_labels, &y_labels);
        Chart::new(datasets)
            .legend_position(None)
            .x_axis(Axis::default().bounds([x0, x1]).labels(x_labels).style(DIM))
            .y_axis(Axis::default().bounds([0.0, 100.0]).labels(y_labels).style(DIM))
            .render(area, buf);
        if graph.is_empty() {
            return;
        }
        // Each series' id beside its last point: just right of it where there is room, and where
        // another id took that cell, the nearest free one above or below it, then to its left.
        let mut taken = BTreeSet::new();
        for d in &series {
            let Some(&(px, py)) = d.data.last() else { continue };
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a share of the graph's cells, clamped to them"
            )]
            let cell = |share: f64, cells: u16| (share.clamp(0.0, 1.0) * f64::from(cells - 1)).round() as u16;
            let col = graph.x + cell((px - x0) / (x1 - x0), graph.width);
            let row = graph.y + cell(1.0 - py / 100.0, graph.height);
            let col = if col + 1 < graph.right() { col + 1 } else { col };
            let rows = (0..graph.height).flat_map(|k| [i32::from(row) - i32::from(k), i32::from(row) + i32::from(k)]);
            let rows: Vec<u16> =
                rows.filter_map(|r| u16::try_from(r).ok()).filter(|r| graph.y <= *r && *r < graph.bottom()).collect();
            let free =
                (graph.x..=col).rev().flat_map(|c| rows.iter().map(move |&r| (c, r))).find(|p| !taken.contains(p));
            if let Some((c, r)) = free {
                taken.insert((c, r));
                buf[(c, r)].set_char(d.id).set_style(Style::new().fg(d.color).add_modifier(Modifier::BOLD));
            }
        }
    }

    /// The legend: a line per series (its id as on the chart and a sample of its colour, then its
    /// account, meter, last reading and reset) and per floor, each as (sample, its colour, words).
    fn legend(&self) -> Vec<(String, Color, String)> {
        let series = self.series().into_iter().map(|d| (format!("{} ━━ ", d.id), d.color, d.label));
        let floors = self.floors().into_iter().map(|f| (format!("  {0}{0} ", f.mark), Color::DarkGray, f.name));
        series.chain(floors).collect()
    }

    /// The whole view as text: the heading, the chart `width` columns wide (100 where no width is
    /// known), then the legend. Heading and legend are lines of their own, wrapped to `width` when
    /// it is known, never cut; with `colour`, each sample in its series' colour.
    pub fn print(&self, width: Option<usize>, colour: bool) -> String {
        let cols = width.unwrap_or(100);
        let area = Rect::new(0, 0, u16::try_from(cols).unwrap_or(u16::MAX), CHART_ROWS);
        let mut buf = Buffer::empty(area);
        self.draw(area, &mut buf);
        let mut lines = wrap("", &self.heading(), width);
        lines.push(text(&buf, colour));
        for (sample, color, words) in self.legend() {
            let shown = if colour { format!("\x1b[{}m{sample}\x1b[39m", sgr(color)) } else { sample.clone() };
            let mut wrapped = wrap(&" ".repeat(sample.width()), &words, width.map(|w| w.max(sample.width() + 1)));
            if let Some(first) = wrapped.first_mut() {
                first.replace_range(..sample.width(), &shown);
            }
            lines.extend(wrapped);
        }
        lines.join("\n")
    }
}

/// Where ratatui's `Chart` plots in `area` with these axis labels (its own layout, which it does
/// not expose): right of the y labels and axis, above the x axis and labels.
fn graph_area(area: Rect, x_labels: &[String], y_labels: &[&str]) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }
    let (has_x, has_y) = (!x_labels.is_empty(), !y_labels.is_empty());
    let mut bottom = area.bottom() - 1;
    for _ in 0..2 {
        // The x labels' row, then the x axis's.
        if has_x && bottom > area.top() {
            bottom -= 1;
        }
    }
    let first_x = x_labels.first().map_or(0, |l| l.width().saturating_sub(usize::from(has_y)));
    let left_of_axis = y_labels.iter().map(|l| l.width()).max().unwrap_or(0).max(first_x);
    let mut left = area.left() + u16::try_from(left_of_axis).unwrap_or(u16::MAX).min(area.width / 3);
    if has_y && left + 1 < area.right() {
        left += 1;
    }
    Rect::new(left, area.top(), area.right().saturating_sub(left), bottom - area.top() + 1)
}

/// `words` after `indent` on every line, wrapped at spaces to `width` display columns (a word wider
/// than a line is broken); one line when no width is given.
fn wrap(indent: &str, words: &str, width: Option<usize>) -> Vec<String> {
    let Some(width) = width else { return vec![format!("{indent}{words}")] };
    let room = width.saturating_sub(indent.width()).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in words.split(' ') {
        let fits = line.is_empty() || line.width() + 1 + word.width() <= room;
        if !fits {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        for c in word.chars() {
            if line.width() + c.width().unwrap_or(0) > room {
                lines.push(std::mem::take(&mut line));
            }
            line.push(c);
        }
    }
    lines.push(line);
    lines.into_iter().map(|l| format!("{indent}{}", l.trim_end())).collect()
}

/// `buf` as lines of text, trailing blanks dropped; with `colour`, each change of colour as an SGR escape.
/// A wide symbol's cells are printed once: the cells it covers after its first are skipped.
pub fn text(buf: &Buffer, colour: bool) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut fg = Color::Reset;
        let mut covered = 0;
        for x in area.left()..area.right() {
            if covered > 0 {
                covered -= 1;
                continue;
            }
            let cell = &buf[(x, y)];
            covered = cell.symbol().width().saturating_sub(1);
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
    let tz = TimeZone::system();
    let usage = Usage { history: &history, limits, filter: &filter, now, tz: &tz };
    if history.series.is_empty() {
        out!("{}: no readings", usage.heading());
    } else {
        out!("{}", usage.print(size.map(|(cols, _)| cols), size.is_some()));
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
    fn every_weekly_window_a_harness_reports_shows_by_default() {
        let b = [vec![reading(
            NOW - H,
            "omp:w",
            &[
                ("google-antigravity/Gemini (Weekly)", 0.1, 1),
                ("minimax-code/General 7 Day", 0.04, 1),
                ("minimax-code/General 5 Hour", 0.05, 1),
                ("openai-codex/7 days", 0.19, 1),
            ],
        )]];
        let h = History::fold(b.iter().map(Vec::as_slice), &week());
        assert_eq!(
            h.series["omp:w"].keys().collect::<Vec<_>>(),
            ["google-antigravity/Gemini (Weekly)", "minimax-code/General 7 Day", "openai-codex/7 days"]
        );
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

    fn limits() -> Limits {
        Limits {
            floor: 0.5,
            floor_near_reset: Some(crate::config::NearReset { within_hours: 24.0, floor: 0.98 }),
            ..Limits::default()
        }
    }

    /// The text `history` prints as in `width` columns, without colour.
    fn printed(history: &History, width: Option<usize>) -> String {
        let (f, limits, tz) = (week(), limits(), TimeZone::UTC);
        Usage { history, limits: &limits, filter: &f, now: NOW, tz: &tz }.print(width, false)
    }

    #[test]
    fn the_chart_has_its_axes_and_floors() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (f, limits, tz) = (week(), limits(), TimeZone::UTC);
        let h = fold(&f);
        let usage = Usage { history: &h, limits: &limits, filter: &f, now: NOW, tz: &tz };
        let mut t = Terminal::new(TestBackend::new(100, CHART_ROWS)).unwrap();
        t.draw(|frame| usage.draw(frame.area(), frame.buffer_mut())).unwrap();
        let text = text(t.backend().buffer(), false);
        let lines: Vec<&str> = text.lines().collect();
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
        assert!(!text.contains('\x1b'), "plain text has no escapes");
    }

    #[test]
    fn the_heading_and_legend_are_lines_of_their_own() {
        let text = printed(&fold(&week()), Some(100));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "quota since Jan 8 08:00 UTC, 7-day meters", "{text}");
        assert_eq!(lines.len(), 1 + usize::from(CHART_ROWS) + 4, "{text}");
        let legend = &lines[lines.len() - 4..];
        assert!(legend[0].starts_with("1 ━━ claude:a 7 days  5% at the last reading, resets "), "{legend:?}");
        assert!(legend[1].starts_with("2 ━━ omp:w openai-codex/7 days  50% at the last reading"), "{legend:?}");
        assert_eq!(legend[2..], ["  ┄┄ floor 50%", "  ╌╌ floor 98% in the last 24h before a reset"]);
        let piped = printed(&fold(&week()), None);
        assert!(piped.lines().any(|l| l.starts_with("1 ━━ claude:a 7 days") && l.ends_with("3 reading(s)")), "{piped}");
    }

    #[test]
    fn at_60_columns_every_legend_keeps_its_reset_time() {
        let week = 1_800_500_000;
        let b = [vec![
            reading(NOW - 2 * H, "claude:a-rather-long-account-label", &[("seven_day", 0.4, week)]),
            reading(NOW - H, "omp:another-long-label", &[("openai-codex/7 days", 0.6, week)]),
        ]];
        let text = printed(&History::fold(b.iter().map(Vec::as_slice), &super::tests::week()), Some(60));
        assert!(text.lines().all(|l| l.width() <= 60), "{text}");
        // A legend wrapped onto more lines is still one: its words in order, its reset time whole.
        let legend = text.lines().skip(1 + usize::from(CHART_ROWS)).collect::<Vec<_>>().join(" ");
        let squeezed = legend.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(squeezed.matches("resets Jan 21 02:53, 1 reading(s)").count(), 2, "{text}");
    }

    #[test]
    fn a_narrow_terminal_gets_no_line_wider_than_itself() {
        for width in [30, 12] {
            let text = printed(&fold(&week()), Some(width));
            assert!(text.lines().all(|l| l.width() <= width), "{width} columns:\n{text}");
            assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "still a chart:\n{text}");
            let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("resets Jan 21"), "{text}");
        }
        let text = printed(&fold(&week()), Some(30));
        assert!(text.lines().any(|l| l.starts_with("100%")), "the y axis kept at 30 columns:\n{text}");
        assert!(text.lines().any(|l| l.starts_with("Jan 8") && l.ends_with("Jan 15")), "short dates:\n{text}");
    }

    /// Where each of `ids` is drawn in the plot of `text` (right of the y axis): (row, column) each.
    fn marked(text: &str, ids: &[char]) -> Vec<Vec<(usize, usize)>> {
        let plot = |l: &str| l.split_once('│').map(|(_, plot)| plot.chars().collect::<Vec<_>>());
        let rows: Vec<Vec<char>> = text.lines().filter_map(plot).collect();
        ids.iter()
            .map(|id| {
                let at =
                    rows.iter().enumerate().flat_map(|(r, row)| row.iter().enumerate().map(move |(c, ch)| (r, c, ch)));
                at.filter(|(_, _, ch)| *ch == id).map(|(r, c, _)| (r, c)).collect()
            })
            .collect()
    }

    #[test]
    fn plain_output_tells_two_accounts_at_the_same_value_apart() {
        let week = 1_800_500_000;
        let b = [vec![
            reading(NOW - 30 * H, "claude:a", &[("seven_day", 0.2, week)]),
            reading(NOW - 40 * H, "claude:b", &[("seven_day", 0.7, week)]),
            reading(NOW - 3 * H, "claude:a", &[("seven_day", 0.6, week)]),
            reading(NOW - 3 * H, "claude:b", &[("seven_day", 0.6, week)]),
        ]];
        let text = printed(&History::fold(b.iter().map(Vec::as_slice), &super::tests::week()), Some(100));
        assert!(!text.contains('\x1b'), "{text}");
        let at = marked(&text, &['1', '2']);
        assert!(at.iter().all(|a| a.len() == 1), "each id once on the chart: {at:?}\n{text}");
        let ((r1, c1), (r2, c2)) = (at[0][0], at[1][0]);
        assert_ne!((r1, c1), (r2, c2), "{text}");
        assert!(r1.abs_diff(r2) <= 1 && c1.abs_diff(c2) <= 1, "both beside the shared last point:\n{text}");
        assert!(text.lines().any(|l| l.starts_with("1 ━━ claude:a 7 days  60%")), "{text}");
        assert!(text.lines().any(|l| l.starts_with("2 ━━ claude:b 7 days  60%")), "{text}");
    }

    #[test]
    fn an_id_is_drawn_beside_its_series_last_point() {
        let text = printed(&fold(&week()), Some(100));
        let at = marked(&text, &['1', '2']);
        let rows = text.lines().filter(|l| l.contains('│')).count();
        // claude:a ends at 5%, near the bottom; omp:w at 50%, half way up.
        assert!(at[0].len() == 1 && at[0][0].0 + 2 >= rows, "{at:?}\n{text}");
        assert!(at[1].len() == 1 && at[1][0].0.abs_diff(rows / 2) <= 1, "{at:?}\n{text}");
    }

    #[test]
    fn on_a_terminal_the_colours_are_escapes() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf.set_string(0, 0, "ab", Style::new().fg(Color::Cyan));
        assert_eq!(text(&buf, true), "\x1b[36mab\x1b[39m");
        assert_eq!(text(&buf, false), "ab");
    }

    #[test]
    fn a_wide_character_prints_once_with_no_padding() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 1));
        buf.set_string(0, 0, "claude:日本 x", Style::new());
        assert_eq!(text(&buf, false), "claude:日本 x");
    }
}
