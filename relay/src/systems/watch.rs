//! view watch: the live board, as a terminal UI (ratatui). It reads what
//! `view status` reads, for this project or every project on the machine, and
//! draws its panes to fit the window: the jobs, any decisions waiting, the
//! selected job, and the accounts. Each pane scrolls on its own; `?` lists the
//! keys. It writes no state. A reinstalled relay takes over the window on its
//! own, so nothing needs to restart the board after an upgrade.

mod board;
mod draw;
mod pane;
mod ui;

use std::io::{Write as _, stdout};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::crossterm::{cursor, execute};

use crate::bus::now_micros;
use crate::systems::view::machine::Scope;
use board::Board;
use ui::{Action, Ui};

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

/// Runs the board until a quit key, or SIGTERM, SIGHUP or SIGINT, each of which gives the terminal
/// back as `q` does (and exits with 128 plus its number). Redraws on every project's events (the accounts are shared), on any key, click
/// or resize, and each second for the clock.
pub fn run(scope: Scope<'_>, all: bool, everywhere: bool) -> Result<()> {
    let bell = crate::os::Doorbell::watch(&crate::ctx::buses_in(scope.root))?;
    let (tx, rang) = mpsc::channel();
    std::thread::spawn(move || while tx.send(bell.wait(Duration::from_secs(60))).is_ok() {});
    restore_on_signals()?;
    let close = ratatui::run(|terminal| {
        let _mouse = Mouse::capture()?;
        let mut ui = Ui::new(all, everywhere, scope.home.is_some());
        board_loop(terminal, scope, &mut ui, &rang)
    })?;
    match close {
        Close::Quit => Ok(()),
        // The terminal is restored by now; the new binary takes it over with the same arguments.
        Close::Upgraded => {
            use std::os::unix::process::CommandExt;
            Err(Command::new(crate::os::own_binary()?).args(std::env::args_os().skip(1)).exec().into())
        }
    }
}

/// SIGTERM, SIGHUP and SIGINT end the board as `q` does, from a thread of their own: the main
/// thread may be inside crossterm's read of a terminal that has hung up, which never returns.
fn restore_on_signals() -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT]).context("handling signals")?;
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            // A terminal that has gone away takes none of this; the exit is what matters then.
            let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
            let _ = disable_raw_mode();
            std::process::exit(128 + signal);
        }
    });
    Ok(())
}

fn read(scope: Scope<'_>, ui: &Ui) -> Result<Board> {
    let m = scope.read()?;
    Ok(Board::new(scope.cfg, &m, m.accounts(scope.cfg), ui.shown.all, ui.shown.everywhere, now_micros()))
}

fn board_loop(
    terminal: &mut DefaultTerminal,
    scope: Scope<'_>,
    ui: &mut Ui,
    rang: &mpsc::Receiver<bool>,
) -> Result<Close> {
    let version = crate::os::binary_version();
    // A failed read keeps the last good board (at first, an empty one) and says why; the next read retries.
    let mut board = Board::default();
    let mut good_at = "never".to_owned();
    let mut stale = true;
    let mut read_at = Instant::now();
    let mut drawn: Option<i64> = None;
    let mut changed = true;
    loop {
        if stale {
            if crate::os::binary_version() != version {
                return Ok(Close::Upgraded);
            }
            match read(scope, ui) {
                Ok(b) => {
                    (board, ui.error) = (b, None);
                    good_at = jiff::Timestamp::now().strftime("%H:%M:%SZ").to_string();
                }
                // The cause first: the path after it is the part a narrow title cuts.
                Err(e) => ui.error = Some(format!("read failed (board as of {good_at}): {}: {e}", e.root_cause())),
            }
            ui.sync(&board);
            (read_at, changed) = (Instant::now(), true);
        }
        // Drawn on a change, else once a second for the clock: an idle board stays quiet.
        let second = jiff::Timestamp::now().as_second();
        if changed || drawn != Some(second) {
            terminal.draw(|f| draw::draw(f, &board, ui))?;
            (drawn, changed) = (Some(second), false);
        }
        stale = read_at.elapsed() >= Duration::from_secs(3) || rang.try_iter().any(|r| r);
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let e = event::read()?;
        changed = matches!(e, Event::Resize(..));
        let Some(action) = ui.action(&e) else { continue };
        changed = true;
        match action {
            Action::Quit => return Ok(Close::Quit),
            Action::ToggleAll => (ui.shown.all, stale) = (!ui.shown.all, true),
            Action::ToggleEverywhere if ui.can_toggle => (ui.shown.everywhere, stale) = (!ui.shown.everywhere, true),
            Action::Open => {
                if let Some(text) = ui.detail(&board).page.clone()
                    && let Err(e) = page(terminal, &text)
                {
                    ui.error = Some(format!("{}: {e}", e.root_cause()));
                }
            }
            a => ui.apply(a, &board),
        }
    }
}

/// Hands `text` to `$PAGER` (else `less`) with the terminal given back for as long as it runs,
/// then takes the terminal again, whatever the pager did.
fn page(terminal: &mut DefaultTerminal, text: &str) -> Result<()> {
    let pager = std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "less".into());
    execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    let ran = (|| -> Result<()> {
        let mut child = Command::new("sh")
            .args(["-c", &pager])
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting the pager {pager}"))?;
        // A pager that quits before reading it all closes the pipe; that is no failure.
        let _ = child.stdin.take().context("the pager's stdin")?.write_all(text.as_bytes());
        child.wait().with_context(|| format!("waiting for {pager}"))?;
        Ok(())
    })();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;
    ran
}

#[cfg(test)]
mod tests;
