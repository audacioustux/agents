//! `relay view usage`: each account's quota, a row per account and meter (or with `--chart`, over time as a chart), printed inline. A read-only fold
//! over the readings already on the bus (a quota command's `QuotaEvent::Reading` and a live
//! session's `SessionEvent::Rate`); it writes nothing and never starts a reading. Meters only: who
//! an account is signed in as is never read here.

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

use crate::bus::{Stamped, now_micros};
use crate::config::Limits;
use crate::systems::quota::gauge;
use crate::systems::watch::ChartSpec;
use crate::world::World;

use super::machine::Scope;

pub mod rows;

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
    /// Inside a project, the accounts it can dispatch to; every account when outside one.
    pub within: Option<Within>,
}

/// The accounts one project can dispatch to: what a chart inside it is limited to.
#[derive(Debug, Clone, Default)]
pub struct Within {
    /// The project's name, for the heading.
    pub project: String,
    /// The accounts (`harness:label`) its dispatcher considers ([`crate::accounts::discover`]).
    pub accounts: std::collections::BTreeSet<String>,
}

impl Within {
    /// The accounts `cfg`'s project can dispatch to: its enabled harnesses' accounts, minus
    /// `accounts.exclude`, found by the code its dispatcher runs, but writing nothing.
    pub fn of(cfg: &crate::config::Config) -> Self {
        Self {
            project: cfg.name.clone(),
            accounts: crate::accounts::discover_in(crate::host::Mode::ReadOnly, cfg)
                .iter()
                .map(crate::harness::Home::key)
                .collect(),
        }
    }
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
        self.within.as_ref().is_none_or(|w| w.accounts.contains(key))
            && (self.accounts.is_empty() || self.accounts.iter().any(|a| a == key))
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
    /// The readings on `buses` that `f` lets through: a quota command's, and a live session's
    /// rate, each as the project's own fold takes it in ([`World::meters_read`]). The same account
    /// read by several projects is one series, merged by time; a reading on two buses alike is kept once.
    pub fn fold<'a>(buses: impl IntoIterator<Item = &'a [Stamped]>, f: &Filter) -> Self {
        let mut h = Self::default();
        for bus in buses {
            let mut world = World::default();
            for s in bus {
                world.apply(s);
                let Some((account, meters)) = world.meters_read(s).filter(|_| s.t >= f.since) else { continue };
                if !account.contains(':') {
                    h.bare.insert(account.to_owned());
                    continue;
                }
                if !f.account(account) {
                    continue;
                }
                for (name, m) in meters.iter().filter(|(name, _)| f.meter(name)) {
                    let point = Point { t: s.t, utilization: m.utilization, resets_at: m.resets_at };
                    h.series.entry(account.to_owned()).or_default().entry(name.clone()).or_default().push(point);
                }
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

    /// What the chart leaves out, if anything: the bare labels.
    pub fn left_out(&self) -> Option<String> {
        let names = self.bare.iter().cloned().collect::<Vec<_>>().join(", ");
        (!self.bare.is_empty())
            .then(|| format!("left out: readings under a bare label, which names no harness ({names})"))
    }

    /// Each series, as (account, meter, readings).
    fn each(&self) -> impl Iterator<Item = (&str, &str, &[Point])> {
        self.series.iter().flat_map(|(a, ms)| ms.iter().map(move |(m, p)| (a.as_str(), m.as_str(), p.as_slice())))
    }

    /// The `--json` reply: per account and meter, the readings as `{t, utilization, resets_at}`, and
    /// `rows`, one per account and meter by reset time, with its pace and what it comes to by then.
    pub fn to_json(&self, f: &Filter, limits: &Limits, now: u64) -> serde_json::Value {
        let rows: Vec<_> = rows::rows(self, limits, f.since, now).iter().map(rows::Row::json).collect();
        let mut v = json!({
            "since": f.since,
            "meter": f.meter,
            "floor": limits.floor,
            "pace_window_hours": rows::PACE_WINDOW / 3_600_000_000,
            "accounts": self.series,
            "rows": rows,
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
pub const CHART_ROWS: u16 = 18;

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
        let since = self.time(self.filter.since, "%b %-d %H:%M %Z");
        match &self.filter.within {
            Some(w) => format!("{}'s accounts, quota since {since}, {meter}", w.project),
            None => format!("quota since {since}, {meter}"),
        }
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

    /// What `view usage` prints: the rows, or with `chart` the line chart.
    pub fn render(&self, chart: bool, width: Option<usize>, colour: bool) -> String {
        if chart { self.print(width, colour) } else { self.print_rows(width, colour) }
    }

    /// The rows of the table, soonest reset first.
    pub fn rows(&self) -> Vec<rows::Row> {
        rows::rows(self.history, self.limits, self.filter.since, self.now)
    }

    /// The table's lines in `width` columns ([`rows::table`]).
    pub fn table_lines(&self, width: Option<usize>) -> Vec<rows::Line> {
        rows::table(&self.rows(), width)
    }

    /// What the table's marks mean, wrapped to `width` when it is known.
    pub fn key_lines(&self, width: Option<usize>) -> Vec<String> {
        let floors = self.floors().into_iter().map(|f| f.name).collect::<Vec<_>>().join("; ");
        let hours = rows::PACE_WINDOW / 3_600_000_000;
        let words = format!(
            "│ marks the {floors}. Sparkline: the window, oldest first, 0% to 100%. Pace: the last {hours}h of readings."
        );
        wrap("", &words, width)
    }

    /// The view as text: the heading, the table in `width` columns (the widest where none is known),
    /// then what its marks mean. With `colour`, a gauge in its tone and a full outlook in red.
    pub fn print_rows(&self, width: Option<usize>, colour: bool) -> String {
        let mut lines = self.heading_lines(width);
        for line in self.table_lines(width) {
            let text: String = line
                .into_iter()
                .map(|p| match p.color.filter(|_| colour) {
                    Some(c) => format!("\x1b[{}m{}\x1b[39m", sgr(c), p.text),
                    None => p.text,
                })
                .collect();
            lines.push(text);
        }
        lines.extend(self.key_lines(width));
        lines.join("\n")
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

    /// The heading, wrapped to `width` when it is known.
    pub fn heading_lines(&self, width: Option<usize>) -> Vec<String> {
        wrap("", &self.heading(), width)
    }

    /// The legend wrapped to `width` when it is known, never cut: a block per series and floor,
    /// each starting with spaces where its sample goes.
    pub fn legend_lines(&self, width: Option<usize>) -> Vec<Legend> {
        self.legend()
            .into_iter()
            .map(|(sample, color, words)| {
                let sample_width = sample.width();
                let lines = wrap(&" ".repeat(sample_width), &words, width.map(|w| w.max(sample_width + 1)));
                Legend { sample, color, sample_width, lines }
            })
            .collect()
    }

    /// The whole view as text: the heading, the chart `width` columns wide (100 where no width is
    /// known), then the legend. Heading and legend are lines of their own, wrapped to `width` when
    /// it is known, never cut; with `colour`, each sample in its series' colour.
    pub fn print(&self, width: Option<usize>, colour: bool) -> String {
        let cols = width.unwrap_or(100);
        let area = Rect::new(0, 0, u16::try_from(cols).unwrap_or(u16::MAX), CHART_ROWS);
        let mut buf = Buffer::empty(area);
        self.draw(area, &mut buf);
        let mut lines = self.heading_lines(width);
        lines.push(text(&buf, colour));
        for mut l in self.legend_lines(width) {
            let shown = if colour { format!("\x1b[{}m{}\x1b[39m", sgr(l.color), l.sample) } else { l.sample.clone() };
            if let Some(first) = l.lines.first_mut() {
                first.replace_range(..l.sample_width, &shown);
            }
            lines.extend(l.lines);
        }
        lines.join("\n")
    }
}

/// One legend entry, wrapped.
#[derive(Debug)]
pub struct Legend {
    /// Its id and a sample of its colour.
    pub sample: String,
    pub color: Color,
    pub sample_width: usize,
    /// Its words in lines, the first beginning with `sample_width` spaces where the sample goes.
    pub lines: Vec<String>,
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

/// The buses a chart reads: every project's on this machine, in a project or outside one (accounts
/// are the machine's, so a reading is wherever it happened to be taken), with each that cannot be
/// read named. Inside a project the chart is then limited to its accounts ([`Within`]).
pub fn buses(scope: Scope<'_>) -> Result<(Vec<Vec<Stamped>>, Vec<String>)> {
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

/// Tabulates the readings once, or with `chart` charts them; with `watch`, opens the watch on the same instead.
pub fn run(scope: Scope<'_>, json: bool, mut filter: Filter, since: &str, watch: bool, chart: bool) -> Result<()> {
    if let Some(a) = filter.accounts.iter().find(|a| !a.contains(':')) {
        fail!(Usage, "--account {a}: an account is harness:label");
    }
    let Some(window) = super::stats::parse_duration(since) else {
        fail!(Usage, "--since {since}: a duration such as 30m, 12h, 7d or 2w");
    };
    if watch {
        if json {
            fail!(Usage, "--watch draws a terminal UI; --json prints the readings once: use one");
        }
        return crate::systems::watch::run(
            scope,
            false,
            scope.home.is_none(),
            Some(ChartSpec { filter, window, chart }),
        );
    }
    let now = now_micros();
    filter.since = now.saturating_sub(window);
    filter.within = scope.home.map(|ctx| Within::of(&ctx.cfg));
    let (read, unreadable) = buses(scope)?;
    let history = History::fold(read.iter().map(Vec::as_slice), &filter);
    let limits = &scope.cfg.limits;
    if json {
        let mut v = history.to_json(&filter, limits, now);
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
        let (cols, colour) = (size.map(|(cols, _)| cols), size.is_some());
        out!("{}", usage.render(chart, cols, colour));
    }
    if let Some(left_out) = history.left_out() {
        out!("{left_out}");
    }
    for u in unreadable {
        out!("unreadable: {u}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Event, QuotaEvent, SessionEvent};
    use crate::harness::{Meter, Meters};

    const H: u64 = 3600 * 1_000_000;
    const NOW: u64 = 1_800_000_000 * 1_000_000;

    fn meters(meters: &[(&str, f64, i64)]) -> Meters {
        meters.iter().map(|&(n, u, r)| (n.to_owned(), Meter { utilization: u, resets_at: Some(r) })).collect()
    }

    fn reading(t: u64, account: &str, m: &[(&str, f64, i64)]) -> Stamped {
        let meters = meters(m);
        let event = Event::Quota(QuotaEvent::Reading { account: account.into(), meters, sign_in: Some("s".into()) });
        Stamped { t, event, from: None }
    }

    /// A live session's rate, as a runner writes it.
    fn rate(t: u64, account: &str, m: &[(&str, f64, i64)]) -> Stamped {
        let event = Event::Session(SessionEvent::Rate { account: account.into(), meters: meters(m) });
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

    /// A project named `name` under `root`'s state, its bus holding `readings` (account, 7-day utilization).
    fn project(root: &std::path::Path, name: &str, readings: &[(&str, f64)]) -> crate::ctx::Ctx {
        let repo = root.join("repos").join(name);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = crate::config::Config::parse(&format!("name = \"{name}\"\n"), &repo).unwrap();
        let ctx = crate::ctx::Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        std::fs::create_dir_all(&ctx.bus).unwrap();
        let mut lines = String::new();
        for &(account, u) in readings {
            let (meters, sign_in) = (meters(&[("seven_day", u, 1_800_500_000)]), Some("s".into()));
            let mut v = serde_json::to_value(QuotaEvent::Reading { account: account.into(), meters, sign_in }).unwrap();
            v["t"] = now_micros().into();
            {
                use std::fmt::Write as _;
                writeln!(lines, "{v}").unwrap();
            }
        }
        std::fs::write(ctx.bus.join(crate::bus::system_file("quota")), lines).unwrap();
        ctx
    }

    /// The accounts a chart inside `ctx` would show, the project allowed `usable` of them.
    fn charted(ctx: &crate::ctx::Ctx, usable: &[&str]) -> Vec<String> {
        let scope = Scope { cfg: &ctx.cfg, home: Some(ctx), root: &ctx.state_root };
        let (read, unreadable) = super::buses(scope).unwrap();
        assert!(unreadable.is_empty(), "{unreadable:?}");
        let within =
            Within { project: ctx.cfg.name.clone(), accounts: usable.iter().map(|a| (*a).to_owned()).collect() };
        let filter = Filter { within: Some(within), ..Filter::default() };
        History::fold(read.iter().map(Vec::as_slice), &filter).series.into_keys().collect()
    }

    #[test]
    fn a_project_with_no_readings_of_its_own_charts_its_account_from_another_projects_bus() {
        let d = crate::config::tempdir::Dir::new();
        let _grid = project(d.path(), "grid", &[("claude:tj.pro", 0.4), ("omp:w", 0.2), ("claude:other", 0.9)]);
        let quiet = project(d.path(), "quiet", &[]);
        assert_eq!(quiet.events().unwrap().len(), 0);
        assert_eq!(charted(&quiet, &["claude:tj.pro", "omp:w"]), ["claude:tj.pro", "omp:w"]);
    }

    #[test]
    fn an_account_the_project_cannot_use_is_not_charted_wherever_it_was_read() {
        let d = crate::config::tempdir::Dir::new();
        let _grid = project(d.path(), "grid", &[("claude:tj.pro", 0.4), ("claude:other", 0.9)]);
        let own = project(d.path(), "own", &[("claude:other", 0.5)]);
        assert_eq!(charted(&own, &["claude:tj.pro"]), ["claude:tj.pro"]);
        assert_eq!(charted(&own, &[]), Vec::<String>::new());
    }

    #[test]
    fn outside_a_project_every_account_is_charted_and_the_heading_says_no_project() {
        let d = crate::config::tempdir::Dir::new();
        let grid = project(d.path(), "grid", &[("claude:tj.pro", 0.4), ("claude:other", 0.9)]);
        let cfg = grid.cfg.clone();
        let scope = Scope { cfg: &cfg, home: None, root: &grid.state_root };
        let (read, _) = super::buses(scope).unwrap();
        let h = History::fold(read.iter().map(Vec::as_slice), &Filter::default());
        assert_eq!(h.series.keys().collect::<Vec<_>>(), ["claude:other", "claude:tj.pro"]);
        let (limits, tz) = (limits(), TimeZone::UTC);
        let usage = |filter: &Filter| Usage { history: &h, limits: &limits, filter, now: NOW, tz: &tz }.heading();
        assert_eq!(usage(&week()), "quota since Jan 8 08:00 UTC, 7-day meters");
        let inside = Filter { within: Some(Within { project: "tickify".into(), ..Within::default() }), ..week() };
        assert_eq!(usage(&inside), "tickify's accounts, quota since Jan 8 08:00 UTC, 7-day meters");
    }

    /// `view usage` promises to write nothing: a program found on `PATH` but not in the record
    /// (`sh` stands for the claude harness here) is used for the account set, and not recorded.
    #[test]
    fn the_accounts_of_a_project_are_found_without_recording_any_program() {
        let d = crate::config::tempdir::Dir::new();
        let state = d.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        let mut cfg =
            crate::config::Config::parse("name = \"t\"\n[harness.claude]\nprogram = \"sh\"\n", d.path()).unwrap();
        cfg.programs = Some(crate::host::record_in(&state));
        let record = crate::host::record_in(&state);
        Within::of(&cfg);
        assert!(!record.exists(), "the record was written");
        assert!(!record.with_extension("lock").exists(), "the lock was taken");
        // The control: the dispatcher's own path records what it finds, so the setup is a real case.
        crate::accounts::discover(&cfg);
        assert!(record.exists(), "the dispatcher records what PATH finds");
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
    fn a_live_sessions_rates_are_readings_too() {
        let resets = 1_800_500_000;
        let only = [vec![
            rate(NOW - 5 * H, "claude:a", &[("seven_day", 0.30, resets)]),
            rate(NOW - 4 * H, "claude:a", &[("five_hour", 0.1, 1), ("seven_day", 0.35, resets)]),
        ]];
        let h = History::fold(only.iter().map(Vec::as_slice), &week());
        let used: Vec<f64> = h.series["claude:a"]["seven_day"].iter().map(|p| p.utilization).collect();
        assert_eq!(used, [0.30, 0.35], "a window with only live updates is charted");
        let text = printed(&h, Some(100));
        assert!(text.contains("1 ━━ claude:a 7 days  35% at the last reading"), "{text}");
        // Mixed: the last value is the latest of either kind.
        let mixed = [vec![
            reading(NOW - 6 * H, "claude:a", &[("seven_day", 0.20, resets)]),
            rate(NOW - 2 * H, "claude:a", &[("seven_day", 0.40, resets)]),
        ]];
        let h = History::fold(mixed.iter().map(Vec::as_slice), &week());
        assert_eq!(h.series["claude:a"]["seven_day"].last().map(|p| p.utilization), Some(0.40));
    }

    /// A rate from an attempt that is not its job's current one: the world does not take it in, nor does the chart.
    #[test]
    fn a_rate_the_world_drops_is_not_charted() {
        let stale = crate::bus::Origin { job: "gone".into(), attempt: 1, created: None };
        let mut s = rate(NOW - H, "claude:a", &[("seven_day", 0.9, 1_800_500_000)]);
        s.from = Some(stale);
        let h = History::fold([std::slice::from_ref(&s)], &week());
        assert!(h.series.is_empty(), "{:?}", h.series);
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
        let v = fold(&f).to_json(&f, &limits, NOW);
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

    /// `view usage` prints the rows; `--chart` the chart, with the legend; plain text has no escapes.
    #[test]
    fn view_usage_prints_the_rows_and_chart_prints_the_chart() {
        let (f, limits, tz) = (week(), limits(), TimeZone::UTC);
        let h = fold(&f);
        let usage = Usage { history: &h, limits: &limits, filter: &f, now: NOW, tz: &tz };
        let rows = usage.render(false, Some(120), false);
        let lines: Vec<&str> = rows.lines().collect();
        assert_eq!(lines[0], "quota since Jan 8 08:00 UTC, 7-day meters", "{rows}");
        assert!(lines[1].starts_with("omp:w openai-codex/7 days") || lines[1].starts_with("claude:a"), "{rows}");
        assert!(lines.iter().any(|l| l.starts_with("claude:a") && l.contains("5%") && l.contains("resets")), "{rows}");
        assert!(!rows.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "no chart braille in the rows:\n{rows}");
        assert!(!rows.contains('\x1b') && !rows.contains("at the last reading"), "{rows}");
        let chart = usage.render(true, Some(100), false);
        assert_eq!(chart, usage.print(Some(100), false));
        assert!(chart.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "the chart in braille:\n{chart}");
        assert!(chart.contains("1 ━━ claude:a 7 days"), "and its legend:\n{chart}");
        let coloured = usage.render(false, Some(120), true);
        assert!(coloured.contains("\x1b[32m") || coloured.contains("\x1b[33m"), "a gauge in its tone: {coloured}");
    }

    #[test]
    fn json_has_each_rows_pace_and_projection() {
        let f = week();
        let v = fold(&f).to_json(&f, &limits(), NOW);
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let a = rows.iter().find(|r| r["account"] == "claude:a").unwrap();
        assert_eq!(
            (a["meter"].clone(), a["utilization"].clone(), a["readings"].clone()),
            (json!("seven_day"), json!(0.05), json!(3))
        );
        assert_eq!(a["pace_per_day"], json!(null), "the readings since the reset are one");
        assert_eq!(a["outlook"], json!("—"));
        assert_eq!(v["pace_window_hours"], json!(24));
        let w = rows.iter().find(|r| r["account"] == "omp:w").unwrap();
        assert!(w["sparkline"].as_str().unwrap().chars().count() == 14, "{w}");
        assert!(v["accounts"]["claude:a"]["seven_day"].is_array(), "the readings stay where they were");
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
