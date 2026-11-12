//! The quota as a view of the watch: `view usage`'s own rows and chart ([`Usage`]), refolded from the
//! buses as readings land. It reads what `view usage` reads and writes nothing.

use anyhow::Result;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::draw::key_line;
use super::pane::wrap;
use super::ui::Ui;
use crate::config::Limits;
use crate::systems::view::machine::Scope;
use crate::systems::view::usage::{self, Filter, History, Usage, Within};

/// The rows the chart keeps before the legend takes the rest.
const MIN_CHART: u16 = 8;
/// `--since`'s default, in bus microseconds.
const WEEK: u64 = 7 * 24 * 3600 * 1_000_000;

/// Which readings the chart shows: `view usage`'s filters, and how far back.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Its meter, accounts and harnesses; `since` and `within` are set at each read.
    pub filter: Filter,
    /// How far back from now, in bus microseconds.
    pub window: u64,
    /// The line chart, not the rows (`c` switches).
    pub chart: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self { filter: Filter::default(), window: WEEK, chart: false }
    }
}

/// The readings as of one fold.
#[derive(Debug)]
pub struct Chart {
    history: History,
    filter: Filter,
    limits: Limits,
    unreadable: Vec<String>,
    now: u64,
    tz: TimeZone,
}

impl Chart {
    /// Folds every project's bus as `view usage` does at `now`: inside a project, only its accounts
    /// ([`Within`], found without recording anything). Starts no reading.
    pub fn read(scope: Scope<'_>, spec: &Spec, now: u64) -> Result<Self> {
        let mut filter = spec.filter.clone();
        filter.since = now.saturating_sub(spec.window);
        filter.within = scope.home.map(|ctx| Within::of(&ctx.cfg));
        let (read, unreadable) = usage::buses(scope)?;
        let history = History::fold(read.iter().map(Vec::as_slice), &filter);
        Ok(Self { history, filter, limits: scope.cfg.limits.clone(), unreadable, now, tz: TimeZone::system() })
    }

    fn usage(&self) -> Usage<'_> {
        Usage { history: &self.history, limits: &self.limits, filter: &self.filter, now: self.now, tz: &self.tz }
    }
}

/// The quota view: the rows, or with `c` the chart.
pub fn draw(f: &mut Frame<'_>, ui: &mut Ui) {
    if ui.usage.chart { chart(f, ui) } else { rows(f, ui) }
}

/// The rows: the heading, a line per account and meter (a window too short for them says how many
/// it leaves out), what the marks mean and what is left out.
fn rows(f: &mut Frame<'_>, ui: &mut Ui) {
    let [body, keys] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    let cols = usize::from(body.width);
    let mut head: Vec<Line<'static>> = Vec::new();
    let mut table: Vec<Line<'static>> = Vec::new();
    let mut foot: Vec<Line<'static>> = Vec::new();
    if let Some(e) = &ui.error {
        head.extend(wrap(e, cols).into_iter().map(|l| Line::from(l.fg(Color::Red).bold())));
    }
    match ui.chart.as_ref() {
        None => head.push(Line::from("reading the quota history…")),
        Some(c) => {
            let usage = c.usage();
            head.extend(usage.heading_lines(Some(cols)).into_iter().map(|l| Line::from(l.bold())));
            if c.history.series.is_empty() {
                head.push(Line::from("no readings".dim()));
            } else {
                table = usage
                    .table_lines(Some(cols))
                    .into_iter()
                    .map(|l| {
                        Line::from(
                            l.into_iter()
                                .map(|p| match p.color {
                                    Some(c) => Span::from(p.text).fg(c),
                                    None => Span::from(p.text),
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                foot.extend(usage.key_lines(Some(cols)).into_iter().map(|l| Line::from(l.dim())));
            }
            let notes = c.history.left_out().into_iter().chain(c.unreadable.iter().map(|u| format!("unreadable: {u}")));
            for n in notes {
                foot.extend(wrap(&n, cols).into_iter().map(|l| Line::from(l.dim())));
            }
        }
    }
    let total = usize::from(body.height);
    // The footer gives way first: one line stays for a row, or for the notice that rows are left out.
    let reserve = usize::from(!table.is_empty());
    foot.truncate(total.saturating_sub(head.len() + reserve));
    let room = total.saturating_sub(head.len() + foot.len());
    if table.len() > room {
        let shown = room.saturating_sub(1);
        let more = table.len() - shown;
        table.truncate(shown);
        if room > 0 {
            table.push(Line::from(format!("… {more} more lines than the window shows").dim()));
        }
    }
    let mut lines = head;
    lines.extend(table);
    lines.extend(foot);
    f.render_widget(Paragraph::new(lines), body);
    ui.keys = key_line(f, keys, ui);
    if ui.help {
        super::draw::help(f, f.area());
    }
}

/// The chart view: the heading, the chart filling the window, the legend and what is left out.
fn chart(f: &mut Frame<'_>, ui: &mut Ui) {
    let [body, keys] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    let cols = usize::from(body.width);
    let mut head: Vec<Line<'static>> = Vec::new();
    let mut foot: Vec<Line<'static>> = Vec::new();
    if let Some(e) = &ui.error {
        head.extend(wrap(e, cols).into_iter().map(|l| Line::from(l.fg(Color::Red).bold())));
    }
    let chart = ui.chart.as_ref();
    let mut empty = false;
    match chart {
        None => head.push(Line::from("reading the quota history…")),
        Some(c) => {
            let usage = c.usage();
            head.extend(usage.heading_lines(Some(cols)).into_iter().map(|l| Line::from(l.bold())));
            empty = c.history.series.is_empty();
            if empty {
                head.push(Line::from("no readings".dim()));
            } else {
                for l in usage.legend_lines(Some(cols)) {
                    let mut lines = l.lines.into_iter();
                    if let Some(first) = lines.next() {
                        let rest = first.get(l.sample_width..).unwrap_or_default().to_owned();
                        foot.push(Line::from(vec![Span::from(l.sample).fg(l.color), Span::from(rest)]));
                    }
                    foot.extend(lines.map(Line::from));
                }
            }
            let notes = c.history.left_out().into_iter().chain(c.unreadable.iter().map(|u| format!("unreadable: {u}")));
            for n in notes {
                foot.extend(wrap(&n, cols).into_iter().map(|l| Line::from(l.dim())));
            }
        }
    }
    let total = body.height;
    let head_h = u16::try_from(head.len()).unwrap_or(u16::MAX).min(total);
    let avail = total - head_h;
    let want = u16::try_from(foot.len()).unwrap_or(u16::MAX);
    // With no chart to draw there is nothing to keep room for: what was left out says why.
    let foot_h = if empty { want.min(avail) } else { want.min(avail.saturating_sub(MIN_CHART)) };
    if foot_h < want && foot_h > 0 {
        foot.truncate(usize::from(foot_h) - 1);
        foot.push(Line::from("… more than the window shows".dim()));
    }
    let [head_area, chart_area, foot_area] =
        Layout::vertical([head_h, avail - foot_h, foot_h].map(Constraint::Length)).areas(body);
    f.render_widget(Paragraph::new(head), head_area);
    if let (Some(c), false) = (chart, empty) {
        c.usage().draw(Rect { ..chart_area }, f.buffer_mut());
    }
    f.render_widget(Paragraph::new(foot), foot_area);
    ui.keys = key_line(f, keys, ui);
    if ui.help {
        super::draw::help(f, f.area());
    }
}
