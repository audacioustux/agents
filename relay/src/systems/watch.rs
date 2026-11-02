//! view watch: the live board, as a terminal UI (ratatui). It reads what
//! `view status` reads and draws three panes that fit the window: the jobs,
//! the selected job, and the accounts. It emits nothing.
//!
//! Mouse: click a job to select it, scroll the jobs or the job pane, click a
//! key in the bottom line. Keys: ↑/↓ (k/j) select, PgUp/PgDn scroll the job
//! pane, `a` adds jobs that ended over a day ago, `q` or Esc quits. A
//! reinstalled relay takes over the window on its own, so nothing needs to
//! restart the board after an upgrade.

use std::io::stdout;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::bus::now_micros;
use crate::ctx::Ctx;
use crate::systems::view::{activity, job_cells, listed};
use crate::world::{Job, Status};

/// What one frame shows, taken from the bus in one read.
struct Board {
    project: String,
    jobs: Vec<Vec<String>>,
    statuses: Vec<Status>,
    /// The job pane for each job: its title and lines.
    details: Vec<(String, Vec<String>)>,
    accounts_head: String,
    accounts: Vec<Vec<String>>,
    ready: Vec<bool>,
}

impl Board {
    fn read(ctx: &Ctx, all: bool) -> Result<Self> {
        let world = ctx.world()?;
        let now = now_micros();
        let jobs = listed(&world, all);
        let accounts = crate::systems::quota::accounts_view(ctx, &world);
        Ok(Self {
            project: ctx.cfg.name.clone(),
            jobs: filled(jobs.iter().map(|j| job_cells(j, now)).collect()),
            statuses: jobs.iter().map(|j| j.status).collect(),
            details: jobs.iter().map(|j| (format!(" {} ", j.name), details(ctx, j))).collect(),
            accounts_head: format!(" {} ", accounts.head),
            accounts: filled(accounts.cells),
            ready: accounts.ready,
        })
    }

    fn running(&self) -> usize {
        self.statuses.iter().filter(|s| **s == Status::Running).count()
    }
}

/// What the job pane says: where the job stands, what its session is doing
/// now, and its answer.
fn details(ctx: &Ctx, j: &Job) -> Vec<String> {
    let place = j.account.as_deref().map_or_else(|| "waiting for an account".to_owned(), |a| format!("on {a}"));
    let mut head = vec![format!("{} {place}", j.status.as_str())];
    if j.attempts > 1 {
        head.push(format!("attempt {}", j.attempts));
    }
    if j.turns > 0 {
        head.push(format!("{} turns", j.turns));
    }
    if j.cost_usd > 0.0 {
        head.push(format!("${:.2}", j.cost_usd));
    }
    head.extend(j.branch.clone());
    let mut d = vec![head.join(" · ")];
    d.extend(j.reason.iter().map(|r| format!("why: {r}")));
    if j.status == Status::Running {
        d.push(activity(&ctx.job_file(&j.name, "log.ndjson")));
    }
    if let Ok(answer) = std::fs::read_to_string(ctx.job_file(&j.name, "result.md")) {
        d.push(String::new());
        d.extend(answer.lines().map(str::to_owned));
    }
    d
}

/// The rows without the columns that are empty in every row.
fn filled(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let keep: Vec<bool> = (0..n).map(|c| rows.iter().any(|r| r.get(c).is_some_and(|s| !s.is_empty()))).collect();
    rows.into_iter().map(|r| r.into_iter().zip(&keep).filter(|(_, k)| **k).map(|(s, _)| s).collect()).collect()
}

/// Column widths: each as wide as its widest cell, the last taking the rest.
fn widths(rows: &[Vec<String>]) -> Vec<Constraint> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..n)
        .map(|c| {
            let w = rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0);
            if c + 1 == n { Constraint::Fill(1) } else { Constraint::Length(u16::try_from(w).unwrap_or(u16::MAX)) }
        })
        .collect()
}

fn table<'a>(rows: &'a [Vec<String>], style: impl Fn(usize) -> Style, block: Block<'a>) -> Table<'a> {
    let lines = rows.iter().enumerate().map(|(i, r)| Row::new(r.iter().map(String::as_str)).style(style(i)));
    Table::new(lines, widths(rows)).column_spacing(2).block(block)
}

fn status_style(s: Status) -> Style {
    match s {
        Status::Running => Style::new().fg(Color::Green),
        Status::Queued => Style::new().fg(Color::Yellow),
        Status::Failed => Style::new().fg(Color::Red),
        Status::Stopped => Style::new().add_modifier(Modifier::DIM),
        Status::Done => Style::new(),
    }
}

/// What a click or a key asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Quit,
    ToggleAll,
    Up,
    Down,
    Select(usize),
    ScrollUp,
    ScrollDown,
}

/// Where the last frame put what a click can hit.
#[derive(Default)]
struct Hits {
    jobs: Rect,
    detail: Rect,
    keys: Vec<(Rect, Action)>,
}

/// The bottom line: each key's hint, clickable.
fn keys(all: bool) -> [(&'static str, &'static str, Action); 4] {
    [
        ("q", "quit", Action::Quit),
        ("a", if all { "recent jobs only" } else { "older jobs too" }, Action::ToggleAll),
        ("↑↓", "select", Action::Down),
        ("PgUp/PgDn", "scroll", Action::ScrollDown),
    ]
}

fn draw(f: &mut Frame<'_>, b: &Board, ui: &mut Ui) {
    let rows = |n: usize| u16::try_from(n).unwrap_or(u16::MAX).saturating_add(2);
    // The lists as tall as their rows; the selected job's pane takes the rest.
    let [jobs, detail, accounts, help] = Layout::vertical([
        Constraint::Max(rows(b.jobs.len().max(1))),
        Constraint::Fill(1),
        Constraint::Max(rows(b.accounts.len())),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let clock = jiff::Timestamp::now().strftime(" %H:%M:%S ").to_string();
    let jobs_block = Block::bordered()
        .title(format!(" {} · {} running ", b.project, b.running()).bold())
        .title(Line::from(clock).right_aligned());
    let job_table = table(&b.jobs, |i| status_style(b.statuses[i]), jobs_block)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    f.render_stateful_widget(job_table, jobs, &mut ui.selected);

    let (title, lines) = match ui.selected.selected().and_then(|i| b.details.get(i)) {
        Some((t, d)) => (t.as_str(), d.iter().map(|l| Line::from(l.as_str())).collect()),
        None => (" job ", vec![Line::from("No jobs yet. Queue one: relay queue add NAME --text 'what to do'")]),
    };
    let pane = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((ui.scroll, 0));
    f.render_widget(pane.block(Block::bordered().title(title)), detail);

    let ready =
        |i: usize| if b.ready[i] { Style::new().fg(Color::Green) } else { Style::new().add_modifier(Modifier::DIM) };
    f.render_widget(table(&b.accounts, ready, Block::bordered().title(b.accounts_head.as_str())), accounts);

    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = help.x;
    for (key, label, action) in keys(ui.all) {
        let w = u16::try_from(key.chars().count() + label.chars().count() + 3).unwrap_or(u16::MAX);
        hits.push((Rect::new(x, help.y, w, 1), action));
        spans.push(Span::from(format!(" {key}")).bold());
        spans.push(Span::from(format!(" {label} ")).dim());
        x = x.saturating_add(w);
    }
    f.render_widget(Line::from(spans), help);
    ui.hits = Hits { jobs, detail, keys: hits };
}

/// What the board remembers between frames.
struct Ui {
    all: bool,
    selected: TableState,
    /// Lines scrolled off the top of the job pane.
    scroll: u16,
    hits: Hits,
}

impl Ui {
    fn action(&self, e: &Event) -> Option<Action> {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => key(k),
            Event::Mouse(m) => self.mouse(*m),
            _ => None,
        }
    }

    fn mouse(&self, m: MouseEvent) -> Option<Action> {
        let at = Position::new(m.column, m.row);
        let over_detail = self.hits.detail.contains(at);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, a)) = self.hits.keys.iter().find(|(r, _)| r.contains(at)) {
                    return Some(*a);
                }
                // One line per job inside the border, from the table's first visible row.
                let inside = self.hits.jobs.inner(Margin::new(1, 1));
                inside.contains(at).then(|| Action::Select(self.selected.offset() + usize::from(at.y - inside.y)))
            }
            MouseEventKind::ScrollDown => Some(if over_detail { Action::ScrollDown } else { Action::Down }),
            MouseEventKind::ScrollUp => Some(if over_detail { Action::ScrollUp } else { Action::Up }),
            _ => None,
        }
    }

    fn apply(&mut self, a: Action, b: &Board) {
        let before = self.selected.selected();
        let lines = before.and_then(|i| b.details.get(i)).map_or(0, |(_, d)| d.len());
        match a {
            Action::Up => self.selected.select_previous(),
            Action::Down => self.selected.select_next(),
            Action::Select(i) if i < b.jobs.len() => self.selected.select(Some(i)),
            Action::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            Action::ScrollDown => {
                let last = u16::try_from(lines.saturating_sub(1)).unwrap_or(u16::MAX);
                self.scroll = self.scroll.saturating_add(3).min(last);
            }
            Action::Quit | Action::ToggleAll | Action::Select(_) => {}
        }
        if self.selected.selected() != before {
            self.scroll = 0;
        }
    }
}

fn key(k: &KeyEvent) -> Option<Action> {
    Some(match k.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::PageDown | KeyCode::Char(' ') => Action::ScrollDown,
        KeyCode::PageUp => Action::ScrollUp,
        KeyCode::Char('a') => Action::ToggleAll,
        _ => return None,
    })
}

/// Why the board closed.
enum Close {
    Quit,
    /// The binary was replaced: the new one should take over.
    Upgraded,
}

/// Mouse reporting for as long as it lives; a panic or an early return still gives it back.
struct Mouse;

impl Mouse {
    fn capture() -> std::io::Result<Self> {
        execute!(stdout(), EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for Mouse {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
}

/// Runs the board until a quit key. Redraws on every project's events (the
/// accounts are shared), on any key, click or resize, and every few seconds for the clocks.
pub fn run(ctx: &Ctx, all: bool) -> Result<()> {
    let bell = ctx.doorbell()?;
    let (tx, rang) = mpsc::channel();
    std::thread::spawn(move || while tx.send(bell.wait(Duration::from_secs(60))).is_ok() {});
    let close = ratatui::run(|terminal| {
        let _mouse = Mouse::capture()?;
        ui(terminal, ctx, all, &rang)
    })?;
    match close {
        Close::Quit => Ok(()),
        // The terminal is restored by now; the new binary takes it over with the same arguments.
        Close::Upgraded => {
            use std::os::unix::process::CommandExt;
            Err(std::process::Command::new(crate::os::own_binary()?).args(std::env::args_os().skip(1)).exec().into())
        }
    }
}

fn ui(terminal: &mut DefaultTerminal, ctx: &Ctx, all: bool, rang: &mpsc::Receiver<bool>) -> Result<Close> {
    let version = crate::os::binary_version();
    let mut ui = Ui { all, selected: TableState::default().with_selected(Some(0)), scroll: 0, hits: Hits::default() };
    let mut board = Board::read(ctx, ui.all)?;
    let mut read_at = Instant::now();
    loop {
        terminal.draw(|f| draw(f, &board, &mut ui))?;
        let mut stale = read_at.elapsed() >= Duration::from_secs(3) || rang.try_iter().any(|r| r);
        if event::poll(Duration::from_millis(250))? {
            match ui.action(&event::read()?) {
                Some(Action::Quit) => return Ok(Close::Quit),
                Some(Action::ToggleAll) => (ui.all, stale) = (!ui.all, true),
                Some(a) => ui.apply(a, &board),
                // A resize is redrawn by the next draw; nothing to read again.
                None => {}
            }
        }
        if stale {
            if crate::os::binary_version() != version {
                return Ok(Close::Upgraded);
            }
            board = Board::read(ctx, ui.all)?;
            read_at = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_columns_are_left_out() {
        let rows = vec![vec!["a".to_owned(), String::new(), "x".to_owned()], vec!["b".to_owned(), String::new()]];
        assert_eq!(filled(rows), [vec!["a".to_owned(), "x".to_owned()], vec!["b".to_owned()]]);
    }

    fn board() -> Board {
        let cells = |r: &[&str]| r.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        Board {
            project: "demo".into(),
            jobs: vec![
                cells(&["build", "running", "claude:a", "3m", "$0.41", "Build the thing"]),
                cells(&["look", "done", "claude:b", "2h", "$0.10", "Look at it"]),
            ],
            statuses: vec![Status::Running, Status::Done],
            details: vec![
                (" build ".into(), (0..40).map(|i| format!("line {i}")).collect()),
                (" look ".into(), vec!["done on claude:b".into()]),
            ],
            accounts_head: " accounts ".into(),
            accounts: vec![cells(&["claude:a", "ready", "1 running", "29% seven_day, resets Nov 8 14:59Z"])],
            ready: vec![true],
        }
    }

    fn drawn(ui: &mut Ui) -> String {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(72, 16)).unwrap();
        t.draw(|f| draw(f, &board(), ui)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fresh() -> Ui {
        Ui { all: false, selected: TableState::default().with_selected(Some(0)), scroll: 0, hits: Hits::default() }
    }

    fn click(ui: &Ui, kind: MouseEventKind, column: u16, row: u16) -> Option<Action> {
        ui.mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    }

    #[test]
    fn a_click_selects_the_job_under_it_and_a_hint_does_what_it_says() {
        let mut ui = fresh();
        let frame = drawn(&mut ui);
        let line = |needle: &str| u16::try_from(frame.lines().position(|l| l.contains(needle)).unwrap()).unwrap();
        let left = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(click(&ui, left, 10, line("Look at it")), Some(Action::Select(1)));
        assert_eq!(click(&ui, left, 1, line("quit")), Some(Action::Quit));
        let older = u16::try_from(frame.lines().last().unwrap().find("older").unwrap()).unwrap();
        assert_eq!(click(&ui, left, older, line("quit")), Some(Action::ToggleAll));
        assert_eq!(click(&ui, left, 10, line("line 1")), None, "a click on the job pane does nothing");
    }

    #[test]
    fn the_wheel_scrolls_the_pane_it_is_over_and_a_new_selection_starts_at_the_top() {
        let mut ui = fresh();
        let frame = drawn(&mut ui);
        let pane = u16::try_from(frame.lines().position(|l| l.contains("line 2")).unwrap()).unwrap();
        assert_eq!(click(&ui, MouseEventKind::ScrollDown, 10, pane), Some(Action::ScrollDown));
        assert_eq!(click(&ui, MouseEventKind::ScrollDown, 10, 1), Some(Action::Down));
        let b = board();
        ui.apply(Action::ScrollDown, &b);
        assert_eq!(ui.scroll, 3);
        let first = drawn(&mut ui).lines().find(|l| l.contains("line ")).map(str::to_owned);
        assert!(first.is_some_and(|l| l.contains("line 3")), "the pane starts three lines down");
        ui.apply(Action::Select(1), &b);
        assert_eq!((ui.selected.selected(), ui.scroll), (Some(1), 0));
    }

    #[test]
    fn columns_fit_their_content_and_the_last_takes_the_rest() {
        let rows = [vec!["ab".to_owned(), "xyz".to_owned(), "t".to_owned()], vec!["abcd".to_owned(), String::new()]];
        assert_eq!(widths(&rows), [Constraint::Length(4), Constraint::Length(3), Constraint::Fill(1)]);
    }
}
