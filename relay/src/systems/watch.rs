//! view watch: the live board, as a terminal UI (ratatui). It reads what
//! `view status` reads and draws it in tables that fit the window, with the
//! selected job's details below. It emits nothing.
//!
//! Keys: ↑/↓ (or k/j) select a job, `a` toggles jobs that ended over a day
//! ago, `q`, Esc or Ctrl-C quits.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::bus::now_micros;
use crate::ctx::Ctx;
use crate::systems::view::{activity, job_cells, listed};
use crate::world::Status;

/// What one frame shows, taken from the bus in one read.
struct Board {
    title: String,
    jobs: Vec<Vec<String>>,
    /// The selected job's details, one entry per job.
    details: Vec<Vec<String>>,
    accounts_head: String,
    accounts: Vec<Vec<String>>,
}

impl Board {
    fn read(ctx: &Ctx, all: bool) -> Result<Self> {
        let world = ctx.world()?;
        let now = now_micros();
        let jobs = listed(&world, all);
        let accounts = crate::systems::quota::accounts_view(ctx, &world);
        Ok(Self {
            title: format!(" {} · {} ", ctx.cfg.name, jiff::Timestamp::now().strftime("%H:%M:%S")),
            jobs: filled(jobs.iter().map(|j| job_cells(j, now)).collect()),
            details: jobs.iter().map(|j| details(ctx, j)).collect(),
            accounts_head: format!(" {} ", accounts.head),
            accounts: filled(accounts.cells),
        })
    }
}

/// What the detail pane says about a job: where it stands, what its session
/// is doing now, and the start of its answer.
fn details(ctx: &Ctx, j: &crate::world::Job) -> Vec<String> {
    let mut d = vec![format!(
        "{} · {} · attempt {} · {} turns · ${:.2}{}",
        j.status.as_str(),
        j.account.as_deref().unwrap_or("-"),
        j.attempts,
        j.turns,
        j.cost_usd,
        j.branch.as_deref().map_or(String::new(), |b| format!(" · branch {b}")),
    )];
    d.extend(j.reason.iter().map(|r| format!("reason: {r}")));
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

fn table<'a>(rows: &'a [Vec<String>], title: &'a str) -> Table<'a> {
    Table::new(rows.iter().map(|r| Row::new(r.iter().map(String::as_str))), widths(rows))
        .column_spacing(2)
        .block(Block::bordered().title(title))
}

fn draw(f: &mut Frame<'_>, b: &Board, selected: &mut TableState) {
    let rows = |n: usize| u16::try_from(n).unwrap_or(u16::MAX).saturating_add(2);
    // The lists as tall as their rows; the selected job's details take the rest.
    let [jobs, detail, accounts, help] = Layout::vertical([
        Constraint::Max(rows(b.jobs.len().max(1))),
        Constraint::Fill(1),
        Constraint::Max(rows(b.accounts.len())),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let job_table = table(&b.jobs, &b.title)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    f.render_stateful_widget(job_table, jobs, selected);
    let lines: Vec<Line<'_>> = selected
        .selected()
        .and_then(|i| b.details.get(i))
        .map_or_else(|| vec![Line::from("no job selected")], |d| d.iter().map(|l| Line::from(l.as_str())).collect());
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(" job ")), detail);
    f.render_widget(table(&b.accounts, &b.accounts_head), accounts);
    f.render_widget(
        Line::from(" ↑↓ select · a all jobs · q quit").style(Style::new().add_modifier(Modifier::DIM)),
        help,
    );
}

/// Runs the board until a quit key. Redraws on every project's events (the
/// accounts are shared), on any key or resize, and every few seconds for the clocks.
pub fn run(ctx: &Ctx, all: bool) -> Result<()> {
    let bell = ctx.doorbell()?;
    let (tx, rang) = mpsc::channel();
    std::thread::spawn(move || while tx.send(bell.wait(Duration::from_secs(60))).is_ok() {});
    ratatui::run(|terminal| ui(terminal, ctx, all, &rang))
}

fn ui(terminal: &mut DefaultTerminal, ctx: &Ctx, mut all: bool, rang: &mpsc::Receiver<bool>) -> Result<()> {
    let mut selected = TableState::default().with_selected(Some(0));
    let mut board = Board::read(ctx, all)?;
    let mut read_at = Instant::now();
    loop {
        terminal.draw(|f| draw(f, &board, &mut selected))?;
        let mut stale = read_at.elapsed() >= Duration::from_secs(3) || rang.try_iter().any(|r| r);
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => selected.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => selected.select_previous(),
                    KeyCode::Char('a') => (all, stale) = (!all, true),
                    _ => {}
                },
                // A resize is redrawn by the next draw; nothing to read again.
                _ => {}
            }
        }
        if stale {
            board = Board::read(ctx, all)?;
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

    #[test]
    fn columns_fit_their_content_and_the_last_takes_the_rest() {
        let rows = [vec!["ab".to_owned(), "xyz".to_owned(), "t".to_owned()], vec!["abcd".to_owned(), String::new()]];
        assert_eq!(widths(&rows), [Constraint::Length(4), Constraint::Length(3), Constraint::Fill(1)]);
    }
}
