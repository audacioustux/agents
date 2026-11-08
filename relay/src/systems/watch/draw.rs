//! One frame. Every pane is drawn by one of two renderers, a table or text,
//! each with the same border (thick when focused), scrollbar and position.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState, Wrap,
};

use super::board::{ACCOUNTS, Board};
use super::pane::{Column, Pane, Step, Tone, cut, heights, width, widths, wrap};
use super::ui::{Action, Id, Ui};
use crate::world::Status;

const GAP: u16 = 2;
const MARK: &str = "› ";

pub fn draw(f: &mut Frame<'_>, b: &Board, ui: &mut Ui) {
    let area = f.area();
    // Borders and a heading row; an empty accounts pane says why in two rows.
    let want = |n: usize| u16::try_from(n + 3).unwrap_or(u16::MAX);
    let decisions_want = if b.decisions.is_empty() { 0 } else { want(b.decisions.len()) };
    let accounts_want = want(if b.accounts.is_empty() { 2 } else { b.accounts.len() });
    let [jobs_h, decisions_h, job_h, accounts_h] =
        heights(area.height, want(b.jobs.len()), decisions_want, accounts_want);
    let [jobs, decisions, job, accounts, keys] =
        Layout::vertical([jobs_h, decisions_h, job_h, accounts_h, 1].map(Constraint::Length)).areas(area);
    let focused = ui.focus;
    let frame = |id: Id, title: Line<'static>| framed(title, id == focused);

    let clock = crate::os::local(jiff::Timestamp::now()).strftime(" %H:%M:%S %Z ").to_string();
    let jobs_block = frame(Id::Jobs, jobs_title(b, ui)).title(Line::from(clock).right_aligned());
    let rows: Vec<&[String]> = b.jobs.iter().map(|e| e.cells.as_slice()).collect();
    let selected = ui.selected.is_some().then_some(ui.index);
    let style = |i: usize| status_style(b.jobs[i].status);
    let t = Tabled { cols: b.job_columns(), rows: &rows, style, selected, empty: "" };
    table_pane(f, ui.pane_mut(Id::Jobs), jobs, jobs_block, &t);

    if b.decisions.is_empty() {
        ui.pane_mut(Id::Decisions).place(Rect::default(), 0, 0);
    } else {
        let title = Line::from(" decisions waiting · relay decide answer ID --action ... ".bold());
        let yellow = |_| Style::new().fg(Color::Yellow);
        let t = Tabled { cols: b.decision_columns(), rows: &b.decisions, style: yellow, selected: None, empty: "" };
        table_pane(f, ui.pane_mut(Id::Decisions), decisions, frame(Id::Decisions, title), &t);
    }

    // The scrollbar takes the pane's last inner column.
    let cols = usize::from(job.width.saturating_sub(3));
    let detail = ui.detail(b);
    let title = Line::from(detail.title.clone());
    let rows: Vec<(String, Tone)> =
        detail.lines.iter().flat_map(|(l, t)| wrap(l, cols).into_iter().map(move |r| (r, *t))).collect();
    text_pane(f, ui.pane_mut(Id::Job), job, frame(Id::Job, title), &rows);

    let ready = |i: usize| if b.ready[i] { Style::new().fg(Color::Green) } else { Style::new() };
    let title = Line::from(b.accounts_head.clone());
    let t = Tabled { cols: &ACCOUNTS, rows: &b.accounts, style: ready, selected: None, empty: &b.no_accounts };
    table_pane(f, ui.pane_mut(Id::Accounts), accounts, frame(Id::Accounts, title), &t);

    ui.keys = key_line(f, keys, ui);
    if ui.help {
        help(f, area);
    }
}

fn framed(title: Line<'static>, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused { block.border_type(BorderType::Thick).border_style(Style::new().fg(Color::Cyan)) } else { block }
}

/// The project (or every project's counts), a badge for the decisions waiting, and why the last
/// read failed or which projects could not be read.
/// The diagnostics and the badge come before the counts, which can be long, so a narrow window
/// cuts the counts first.
fn jobs_title(b: &Board, ui: &Ui) -> Line<'static> {
    let mut spans = vec![Span::from(format!(" {} ", b.title)).bold()];
    if let Some(e) = &ui.error {
        spans.push(Span::from(format!("· {e} ")).fg(Color::Red).bold());
    }
    for u in &b.unreadable {
        spans.push(Span::from(format!("· unreadable: {u} ")).fg(Color::Red));
    }
    let n = b.decisions.len();
    if n > 0 {
        spans.push(
            Span::from(format!("· {n} decision{} waiting ", if n == 1 { "" } else { "s" })).fg(Color::Yellow).bold(),
        );
    }
    if !b.counts.is_empty() {
        spans.push(Span::from(format!("· {} ", b.counts)).bold());
    }
    Line::from(spans)
}

fn status_style(s: Status) -> Style {
    match s {
        Status::Running => Style::new().fg(Color::Green),
        Status::Queued => Style::new().fg(Color::Yellow),
        Status::Checking => Style::new().fg(Color::Cyan),
        Status::Failed => Style::new().fg(Color::Red),
        Status::Done | Status::Stopped => Style::new(),
    }
}

/// What a table pane holds: its rows under a heading per column, each row's style, the selected
/// row, and what it says when it has none.
struct Tabled<'a, R, S> {
    cols: &'a [Column],
    rows: &'a [R],
    style: S,
    selected: Option<usize>,
    empty: &'a str,
}

/// A pane of rows under a heading row: columns fitted to the width (each cut with `…`), the rows in
/// view, the selected one reversed.
fn table_pane<R: AsRef<[String]>, S: Fn(usize) -> Style>(
    f: &mut Frame<'_>,
    pane: &mut Pane,
    area: Rect,
    block: Block<'_>,
    t: &Tabled<'_, R, S>,
) {
    let Tabled { cols, rows, style, selected, empty } = t;
    let (selected, empty) = (*selected, *empty);
    let inner = block.inner(area);
    let view = usize::from(inner.height.saturating_sub(1));
    pane.place_keeping(area, view, rows.len(), selected);
    // A scrollbar takes the last inner column, when there is one; a table with a selection marks
    // it with `› ` (reverse video alone is gone under NO_COLOR) in a column of its own.
    let bar = usize::from(pane.rows > pane.view);
    let mark = if selected.is_some() { MARK.chars().count() } else { 0 };
    let w = widths(cols, rows, usize::from(inner.width).saturating_sub(bar + mark), GAP.into());
    let heading = Row::new(cols.iter().zip(&w).map(|(c, &w)| cut(c.head, w))).style(Style::new().bold());
    let shown = rows.iter().enumerate().skip(pane.offset).take(view).map(|(i, r)| {
        let st = if Some(i) == selected { style(i).add_modifier(Modifier::REVERSED) } else { style(i) };
        Row::new(r.as_ref().iter().zip(&w).map(|(s, &w)| cut(s, w))).style(st)
    });
    let position = match selected {
        Some(i) if !rows.is_empty() => Some(format!("{} of {}", i + 1, rows.len())),
        _ => pane.position(),
    };
    let block = position.map_or(block.clone(), |p| block.title_bottom(Line::from(format!(" {p} ")).right_aligned()));
    let widths = w.iter().map(|&w| Constraint::Length(u16::try_from(w).unwrap_or(u16::MAX)));
    let table = Table::new(shown, widths).header(heading).column_spacing(GAP).block(block);
    match selected {
        Some(i) => {
            // The rows drawn start at the pane's offset; the selection, when in view, is among them.
            let mut state = TableState::default().with_selected(i.checked_sub(pane.offset).filter(|&r| r < view));
            let table = table.highlight_symbol(MARK).highlight_spacing(HighlightSpacing::Always);
            f.render_stateful_widget(table, area, &mut state);
        }
        None => f.render_widget(table, area),
    }
    if rows.is_empty() && !empty.is_empty() && inner.height > 1 {
        let below = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
        f.render_widget(Paragraph::new(empty.to_owned()).wrap(Wrap { trim: true }), below);
    }
    scrollbar(f, pane);
}

/// A pane of text already wrapped to its width, scrolled.
fn text_pane(f: &mut Frame<'_>, pane: &mut Pane, area: Rect, block: Block<'_>, rows: &[(String, Tone)]) {
    let inner = block.inner(area);
    pane.place(area, usize::from(inner.height), rows.len());
    let shown: Vec<Line<'_>> = rows.iter().skip(pane.offset).take(pane.view).map(|(r, t)| line(r, *t)).collect();
    let block = match pane.position() {
        Some(p) => block.title_bottom(Line::from(format!(" {p} ")).right_aligned()),
        None => block,
    };
    f.render_widget(Paragraph::new(shown).block(block), area);
    scrollbar(f, pane);
}

fn line(row: &str, tone: Tone) -> Line<'_> {
    match tone {
        Tone::Plain => Line::from(row),
        Tone::Heading => Line::from(row.bold()),
        Tone::Code => Line::from(row.fg(Color::Cyan)),
    }
}

/// A scrollbar on the right border of a pane whose rows do not all fit.
fn scrollbar(f: &mut Frame<'_>, pane: &Pane) {
    if pane.rows <= pane.view || pane.area.height < 3 {
        return;
    }
    let mut state =
        ScrollbarState::new(pane.last_offset() + 1).position(pane.offset).viewport_content_length(pane.view);
    let bar =
        Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None).track_symbol(None);
    f.render_stateful_widget(bar, pane.area.inner(Margin::new(0, 1)), &mut state);
}

/// The bottom line: each key's hint, clickable where it is drawn, then a note on selecting text.
fn key_line(f: &mut Frame<'_>, area: Rect, ui: &Ui) -> Vec<(Rect, Action)> {
    let mut keys = vec![
        ("q", "quit", Action::Quit),
        ("?", "help", Action::Help),
        ("↑", "", Action::Step(Step::Rows(-1))),
        ("↓", "", Action::Step(Step::Rows(1))),
        ("PgUp", "", Action::Step(Step::Pages(-1))),
        ("PgDn", "", Action::Step(Step::Pages(1))),
        ("Tab", "pane", Action::Cycle(1)),
        ("⏎", "pager", Action::Open),
        ("a", if ui.shown.all { "recent" } else { "older" }, Action::ToggleAll),
    ];
    if ui.can_toggle {
        keys.push(("p", if ui.shown.everywhere { "this project" } else { "all projects" }, Action::ToggleEverywhere));
    }
    let (mut spans, mut hits, mut x) = (Vec::new(), Vec::new(), 0u16);
    for (key, label, action) in keys {
        let label = if label.is_empty() { String::new() } else { format!(" {label}") };
        let w = u16::try_from(width(key) + width(&label) + 2).unwrap_or(u16::MAX);
        if x + w > area.width {
            break;
        }
        hits.push((Rect::new(area.x + x, area.y, w, 1), action));
        spans.push(Span::from(format!(" {key}")).bold());
        spans.push(Span::from(format!("{label} ")).dim());
        x += w;
    }
    let note = "  shift-drag selects text";
    if usize::from(area.width.saturating_sub(x)) >= width(note) {
        spans.push(Span::from(note).dim());
    }
    f.render_widget(Line::from(spans), area);
    hits
}

const HELP: &[&str] = &[
    "Tab, Shift-Tab, → ←, l h   the next or previous pane (its border is thick)",
    "↑ ↓, k j                   a row; in the jobs pane, the selection",
    "PgUp PgDn, Space, ^U ^D    a page",
    "Home End, g G              the top, the end",
    "Enter                      the selected job in $PAGER",
    "a                          older jobs too, or recent ones only",
    "p                          every project, or this one",
    "?                          this help         q, Esc, ^C   quit",
    "",
    "The wheel scrolls the pane under it; a click selects a job or focuses a pane.",
    "The board takes the mouse: hold Shift to select text.",
    "Jobs: green running, yellow queued, cyan checking, red failed.",
    "Accounts: green ready.",
    "since: how long a job has run, been queued, or been over.",
];

fn help(f: &mut Frame<'_>, area: Rect) {
    let w = u16::try_from(HELP.iter().map(|l| width(l)).max().unwrap_or(0) + 4).unwrap_or(u16::MAX).min(area.width);
    let h = u16::try_from(HELP.len() + 2).unwrap_or(u16::MAX).min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let lines: Vec<Line<'_>> = HELP.iter().map(|l| Line::from(format!(" {l}"))).collect();
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" keys · any key closes this ".bold())), r);
}
