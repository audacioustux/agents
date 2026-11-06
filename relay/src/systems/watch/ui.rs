//! What the board remembers between frames, and what a key or a click asks of it.

use std::path::PathBuf;
use std::time::SystemTime;

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Margin, Position, Rect};

use super::board::{Board, Entry, Key};
use super::pane::{Pane, Step, Tone, markdown};
use crate::world::Status;

/// The panes, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Jobs,
    Decisions,
    Job,
    Accounts,
}

impl Id {
    pub const ALL: [Self; 4] = [Self::Jobs, Self::Decisions, Self::Job, Self::Accounts];
}

/// What a key or a click asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    ToggleAll,
    ToggleEverywhere,
    Help,
    /// The selected job's pane in `$PAGER`.
    Open,
    /// Focus the next pane, or the previous when negative.
    Cycle(isize),
    Focus(Id),
    /// Move the focused pane, or the selection when that is the jobs pane.
    Step(Step),
    /// Scroll the pane under the pointer by rows.
    Wheel(Id, isize),
    Select(usize),
}

/// Which jobs the board lists.
#[derive(Debug, Clone, Copy)]
pub struct Shown {
    /// Jobs that ended over a day ago too.
    pub all: bool,
    /// Every project's, not only this one's.
    pub everywhere: bool,
}

/// What the board remembers between frames.
#[derive(Debug)]
pub struct Ui {
    pub shown: Shown,
    /// Whether `p` may change `everywhere`: outside a project there is only every project.
    pub can_toggle: bool,
    pub help: bool,
    pub focus: Id,
    /// The selected job, followed by its name as rows move.
    pub selected: Option<Key>,
    /// Its row in the board.
    pub index: usize,
    panes: [Pane; 4],
    /// Where the last frame put each key of the bottom line.
    pub keys: Vec<(Rect, Action)>,
    /// Why the last read failed: the board shown is the last good one.
    pub error: Option<String>,
    /// Counts the board's reads, so the job pane knows when to read the job's files again.
    generation: u64,
    detail: Option<Detail>,
}

impl Ui {
    pub fn new(all: bool, everywhere: bool, can_toggle: bool) -> Self {
        Self {
            shown: Shown { all, everywhere: everywhere || !can_toggle },
            can_toggle,
            help: false,
            focus: Id::Jobs,
            selected: None,
            index: 0,
            panes: [Pane::default(); 4],
            keys: Vec::new(),
            error: None,
            generation: 0,
            detail: None,
        }
    }

    pub fn pane(&self, id: Id) -> &Pane {
        &self.panes[id as usize]
    }

    pub fn pane_mut(&mut self, id: Id) -> &mut Pane {
        &mut self.panes[id as usize]
    }

    /// After a read: the selection follows its job to its new row; a job gone leaves the one now
    /// at its row selected, and with nothing selected the first job is.
    pub fn sync(&mut self, b: &Board) {
        self.generation += 1;
        let found = self.selected.as_ref().and_then(|k| b.find(k));
        self.select(b, found.unwrap_or_else(|| self.index.min(b.jobs.len().saturating_sub(1))));
        if self.focus == Id::Decisions && b.decisions.is_empty() {
            self.focus = Id::Jobs;
        }
    }

    fn select(&mut self, b: &Board, i: usize) {
        let Some(e) = b.jobs.get(i) else {
            (self.selected, self.index) = (None, 0);
            return;
        };
        if self.selected.as_ref() != Some(&e.key) {
            self.selected = Some(e.key.clone());
            self.pane_mut(Id::Job).offset = 0;
        }
        self.index = i;
        self.pane_mut(Id::Jobs).reveal(i);
    }

    /// The panes shown, in focus order.
    fn order(b: &Board) -> impl Iterator<Item = Id> {
        Id::ALL.into_iter().filter(|&id| id != Id::Decisions || !b.decisions.is_empty())
    }

    /// Carries out what is the board's own to do; quitting, toggles and the pager are the loop's.
    pub fn apply(&mut self, a: Action, b: &Board) {
        match a {
            Action::Step(s) if self.focus == Id::Jobs => {
                let i = self.pane(Id::Jobs).stepped(self.index, s, b.jobs.len());
                self.select(b, i);
            }
            Action::Step(s) => self.pane_mut(self.focus).scroll(s),
            Action::Wheel(id, n) => self.pane_mut(id).scroll(Step::Rows(n)),
            Action::Select(i) => {
                self.focus = Id::Jobs;
                if i < b.jobs.len() {
                    self.select(b, i);
                }
            }
            Action::Focus(id) => self.focus = id,
            Action::Cycle(d) => {
                let order: Vec<Id> = Self::order(b).collect();
                let at = order.iter().position(|&id| id == self.focus).unwrap_or(0);
                let n = isize::try_from(order.len()).unwrap_or(1);
                let next = (isize::try_from(at).unwrap_or(0) + d).rem_euclid(n);
                self.focus = order[usize::try_from(next).unwrap_or(0)];
            }
            Action::Help => self.help = !self.help,
            Action::Quit | Action::ToggleAll | Action::ToggleEverywhere | Action::Open => {}
        }
    }

    pub fn action(&self, e: &Event) -> Option<Action> {
        match e {
            // While the help shows, any key or click closes it.
            Event::Key(k) if k.kind == KeyEventKind::Press && self.help => Some(Action::Help),
            Event::Mouse(m) if self.help && matches!(m.kind, MouseEventKind::Down(_)) => Some(Action::Help),
            Event::Key(k) if k.kind == KeyEventKind::Press => key(k),
            Event::Mouse(m) if !self.help => self.mouse(*m),
            _ => None,
        }
    }

    fn mouse(&self, m: MouseEvent) -> Option<Action> {
        let at = Position::new(m.column, m.row);
        let under = Id::ALL.into_iter().find(|&id| self.pane(id).area.contains(at));
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, a)) = self.keys.iter().find(|(r, _)| r.contains(at)) {
                    return Some(*a);
                }
                let jobs = self.pane(Id::Jobs);
                // Under the heading row, one row per job from the first in view.
                let inside = jobs.area.inner(Margin::new(1, 1));
                match under? {
                    Id::Jobs if inside.contains(at) && at.y > inside.y => {
                        Some(Action::Select(jobs.offset + usize::from(at.y - inside.y - 1)))
                    }
                    id => Some(Action::Focus(id)),
                }
            }
            MouseEventKind::ScrollDown => under.map(|id| Action::Wheel(id, 3)),
            MouseEventKind::ScrollUp => under.map(|id| Action::Wheel(id, -3)),
            _ => None,
        }
    }

    /// The selected job's pane, its files read again only after a new read of the board (the
    /// answer only when it changed), and only this job's.
    pub fn detail(&mut self, b: &Board) -> &Detail {
        let entry = b.jobs.get(self.index);
        let fresh = |d: &Detail| Some(&d.key) == entry.map(|e| &e.key) && d.generation == self.generation;
        if !self.detail.as_ref().is_some_and(fresh) {
            let answer = self.detail.take().map(|d| d.answer).unwrap_or_default();
            self.detail = Some(match entry {
                Some(e) => Detail::of(e, b, self.generation, answer),
                None => Detail::empty(b, self.generation),
            });
        }
        self.detail.as_ref().expect("built above")
    }
}

/// The job pane's text, for one job at one read of the board.
#[derive(Debug)]
pub struct Detail {
    key: Key,
    generation: u64,
    pub title: String,
    pub lines: Vec<(String, Tone)>,
    /// What Enter hands `$PAGER`: the head and the answer as written.
    pub page: Option<String>,
    answer: Answer,
}

impl Detail {
    fn of(e: &Entry, b: &Board, generation: u64, mut cache: Answer) -> Self {
        let answer = cache.read(e.file("result.md"));
        let activity = (e.status == Status::Running)
            .then(|| crate::systems::view::activity(&crate::ctx::log_in(&e.jobs_dir, &e.key.name, "log.ndjson")));
        let mut lines: Vec<(String, Tone)> = e.task.lines().map(|l| (l.to_owned(), Tone::Heading)).collect();
        lines.extend(e.head.iter().chain(&activity).map(|l| (l.clone(), Tone::Plain)));
        let head: Vec<String> = lines.iter().map(|(l, _)| l.clone()).collect();
        if let Some(a) = &answer {
            lines.push((String::new(), Tone::Plain));
            lines.extend(markdown(a));
        }
        let title =
            if b.everywhere { format!(" {}/{} ", e.key.project, e.key.name) } else { format!(" {} ", e.key.name) };
        let page = Some(format!("{}\n\n{}", head.join("\n"), answer.as_deref().unwrap_or("(no answer yet)")));
        Self { key: e.key.clone(), generation, title, lines, page, answer: cache }
    }

    fn empty(b: &Board, generation: u64) -> Self {
        let lines = vec![(b.no_jobs.clone(), Tone::Plain)];
        Self { key: Key::default(), generation, title: " job ".into(), lines, page: None, answer: Answer::default() }
    }
}

/// The selected job's answer, read again only when its file changes.
#[derive(Debug, Default)]
struct Answer {
    path: PathBuf,
    modified: Option<SystemTime>,
    text: Option<String>,
}

impl Answer {
    fn read(&mut self, path: PathBuf) -> Option<String> {
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if path != self.path || modified != self.modified {
            self.text = modified.and_then(|_| std::fs::read_to_string(&path).ok());
            (self.path, self.modified) = (path, modified);
        }
        self.text.clone()
    }
}

fn key(k: &KeyEvent) -> Option<Action> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('d') if ctrl => Action::Step(Step::Pages(1)),
        KeyCode::Char('u') if ctrl => Action::Step(Step::Pages(-1)),
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => Action::Cycle(1),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => Action::Cycle(-1),
        KeyCode::Down | KeyCode::Char('j') => Action::Step(Step::Rows(1)),
        KeyCode::Up | KeyCode::Char('k') => Action::Step(Step::Rows(-1)),
        KeyCode::PageDown | KeyCode::Char(' ') => Action::Step(Step::Pages(1)),
        KeyCode::PageUp => Action::Step(Step::Pages(-1)),
        KeyCode::Home | KeyCode::Char('g') => Action::Step(Step::Top),
        KeyCode::End | KeyCode::Char('G') => Action::Step(Step::Bottom),
        KeyCode::Enter => Action::Open,
        KeyCode::Char('a') => Action::ToggleAll,
        KeyCode::Char('p') => Action::ToggleEverywhere,
        KeyCode::Char('?') => Action::Help,
        _ => return None,
    })
}
