//! The board without a terminal: built from folds, drawn on ratatui's `TestBackend`, driven by
//! the events a terminal would send.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::board::Board;
use super::draw::draw;
use super::pane::Step;
use super::ui::{Action, Id, Ui};
use crate::config::{Config, tempdir};
use crate::systems::quota::AccountsView;
use crate::systems::view::machine::{Machine, Project};
use crate::world::{Decision, Job, Status, World};

const NOW: u64 = 1_800_000_000 * 1_000_000;

fn job(name: &str, status: Status, ago_s: u64, task: &str) -> Job {
    let t = NOW - ago_s * 1_000_000;
    let ended = !status.is_live();
    Job {
        name: name.into(),
        status,
        task: task.into(),
        created_at: t,
        started_at: (status == Status::Running).then_some(t),
        ended_at: ended.then_some(t),
        account: (status == Status::Running).then(|| "claude:a".into()),
        ..Job::default()
    }
}

/// `j` as the profile `build-critical` at effort `high`, on claude's opus and omp's luna.
fn modelled(mut j: Job) -> Job {
    j.profile = Some("build-critical".into());
    j.effort = Some("high".into());
    j.models.insert("claude".into(), "claude-opus-5-5".into());
    j.models.insert("omp".into(), "openai-codex/gpt-6-luna".into());
    j
}

fn world(jobs: &[Job]) -> World {
    let mut w = World::default();
    for j in jobs {
        w.jobs.insert(j.name.clone(), j.clone());
    }
    w
}

/// A project directory with a config, and the folds of `demo` (ours) and `other`.
struct Fixture {
    dir: tempdir::Dir,
    cfg: Config,
    machine: Machine,
}

fn fixture(ours: &[Job], theirs: &[Job]) -> Fixture {
    let dir = tempdir::Dir::new();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    let cfg = Config::parse("name = \"demo\"\n[harness.omp]\nenabled = false\n", dir.path()).unwrap();
    let project = |name: &str, jobs: &[Job], own: bool| {
        let d = dir.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        Project { name: name.into(), jobs: d, world: world(jobs), own }
    };
    let machine = Machine {
        projects: vec![project("demo", ours, true), project("other", theirs, false)],
        unreadable: Vec::new(),
    };
    Fixture { dir, cfg, machine }
}

fn accounts(n: usize) -> AccountsView {
    let row = |i: usize| {
        vec![format!("claude:a{i}"), "Ultra #f4bd75".into(), "ready".into(), String::new(), "29% seven_day".into()]
    };
    AccountsView {
        json: serde_json::Value::Null,
        head: "accounts on claude (floor 90%, 1 session(s) each)".into(),
        cells: (0..n).map(row).collect(),
        ready: vec![true; n],
    }
}

impl Fixture {
    fn board(&self, ui: &Ui, accounts_n: usize) -> Board {
        Board::new(&self.cfg, &self.machine, accounts(accounts_n), ui.shown.all, ui.shown.everywhere, NOW)
    }

    fn answer(&self, project: &str, job: &str, text: &str) {
        std::fs::write(self.dir.path().join(project).join(format!("{job}.result.md")), text).unwrap();
    }
}

fn frame(b: &Board, ui: &mut Ui, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, b, ui)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
}

fn row_of(frame: &[String], needle: &str) -> u16 {
    u16::try_from(frame.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle}: {frame:#?}")))
        .unwrap()
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn two_jobs() -> Fixture {
    fixture(
        &[job("build", Status::Running, 180, "Build the thing"), job("look", Status::Done, 7200, "Look at it")],
        &[job("theirs", Status::Queued, 60, "Their task")],
    )
}

#[test]
fn the_board_fits_80x24_and_120x40_with_headings_keys_and_a_zoned_clock() {
    let f = fixture(
        &[
            modelled(job("build", Status::Running, 180, "Build the thing")),
            modelled(Job { account: None, ..job("look", Status::Done, 7200, "Look at it") }),
        ],
        &[job("theirs", Status::Queued, 60, "Their task")],
    );
    for (w, h) in [(80, 24), (120, 40)] {
        let mut ui = Ui::new(false, false, true);
        let b = f.board(&ui, 2);
        ui.sync(&b);
        let fr = frame(&b, &mut ui, w, h);
        assert!(fr[0].contains("demo · 1 running"), "{w}x{h}: {}", fr[0]);
        assert!(fr[0].trim_end_matches(['┓', '┐']).trim_end().ends_with('Z'), "the clock says its zone: {}", fr[0]);
        assert!(fr[1].contains("job") && fr[1].contains("state") && fr[1].contains("task"), "headings: {}", fr[1]);
        assert!(!fr.iter().any(|l| l.contains("Their task")), "one project by default");
        let keys = &fr[usize::from(h) - 1];
        for hint in ["q quit", "? help", "↑", "↓", "PgUp", "PgDn", "Tab pane", "a older", "p all projects"] {
            assert!(keys.contains(hint), "{w}x{h}: {hint} missing from {keys}");
        }
        assert!(fr.iter().any(|l| l.contains("claude:a1")), "the accounts");
        assert_eq!(b.accounts[0][3], "-", "an empty cell says so");
        // The model column follows `as`; the short form shows in the row, and the harness that ran
        // (claude) narrows a job with an account, while one without shows both.
        let heading = fr[1].find("as ").expect("as");
        assert!(fr[1][heading..].contains("model"), "{w}x{h}: {}", fr[1]);
        let run = &fr[row_of(&fr, "Build the") as usize];
        assert!(run.contains("opus-5.5") && !run.contains("luna"), "{w}x{h}: {run}");
        let ended = &fr[row_of(&fr, "Look at") as usize];
        // Where space is tight the cell is cut with `…`, as every column is; the cell itself is whole.
        assert!(ended.contains(if w == 80 { "opus-5.5,l…" } else { "opus-5.5,luna" }), "{w}x{h}: {ended}");
        assert!(b.jobs.iter().any(|e| e.cells[4] == "opus-5.5,luna"), "{:?}", b.jobs);
        assert!(run.find("build-crit").unwrap() < run.find("opus-5.5").unwrap());
        let task = row_of(&fr, "┌ build ");
        let pane = fr[usize::from(task)..].join("\n");
        assert!(pane.contains("as build-critical · model claude-opus-5-5 · effort high"), "{w}x{h}: {pane}");
        assert!(fr[usize::from(task) + 1].contains("Build the thing"), "the job pane starts with its task");
    }
}

/// The heading row is bold and not underlined (an underline breaks at the column gaps), and the rows
/// are neither, so without colour bold alone tells them apart.
#[test]
fn the_heading_is_bold_without_an_underline_and_the_rows_are_plain() {
    use ratatui::style::Modifier;
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|fr| draw(fr, &b, &mut ui)).unwrap();
    let buf = t.backend().buffer().clone();
    let at = |y: u16, x: u16| buf[(x, y)].modifier;
    let fr = frame(&b, &mut ui, 120, 40);
    let head = row_of(&fr, "state");
    let x = u16::try_from(fr[usize::from(head)].find("state").unwrap()).unwrap();
    assert!(at(head, x).contains(Modifier::BOLD), "the heading is bold");
    for y in [head, row_of(&fr, "Build the thing")] {
        for x in 0..120 {
            assert!(!at(y, x).contains(Modifier::UNDERLINED), "no underline at {x},{y}");
        }
    }
    let row = row_of(&fr, "Look at it");
    let x = u16::try_from(fr[usize::from(row)].find("done").unwrap()).unwrap();
    assert!(!at(row, x).contains(Modifier::BOLD), "a row is not bold");
}

#[test]
fn every_project_adds_a_column_and_each_projects_counts() {
    let f = two_jobs();
    let mut ui = Ui::new(false, true, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr[0].contains("every project · demo 1 running · other 0 running, 1 queued"), "{}", fr[0]);
    assert!(fr[1].contains("project"), "{}", fr[1]);
    assert!(fr[row_of(&fr, "Their task") as usize].contains("other"));
    assert!(fr.iter().any(|l| l.contains("demo/build")), "the job pane names the project");
}

#[test]
fn the_end_of_a_long_answer_is_reachable() {
    let f = two_jobs();
    let long: Vec<String> = (0..112).map(|i| format!("line {i} {}", "with words that wrap ".repeat(i % 7))).collect();
    f.answer("demo", "build", &format!("## Findings\n{}\nnotes: THE LAST LINE\n", long.join("\n")));
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 80, 24);
    assert!(fr.iter().any(|l| l.contains("│Findings")), "a heading without its #s");
    ui.apply(Action::Cycle(1), &b);
    assert_eq!(ui.focus, Id::Job);
    ui.apply(Action::Step(Step::Bottom), &b);
    let fr = frame(&b, &mut ui, 80, 24);
    let job = ui.pane(Id::Job);
    assert!(job.rows > 112, "rows are counted wrapped: {}", job.rows);
    assert_eq!(job.offset, job.rows - job.view);
    assert!(fr.iter().any(|l| l.contains("THE LAST LINE")), "{fr:#?}");
    assert!(fr.iter().any(|l| l.contains(&format!("of {} ┛", job.rows))), "x of y on the focused pane");
    assert!(fr[0].starts_with('┌') && fr.iter().any(|l| l.starts_with("┏ build ")), "the focused border");
}

#[test]
fn the_selection_follows_its_job_by_name() {
    let mut f = fixture(&[job("a", Status::Queued, 30, "A"), job("b", Status::Queued, 20, "B")], &[]);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected.as_ref().map(|k| k.name.as_str()), Some("b"), "the first job is selected");
    ui.apply(Action::Step(Step::Rows(1)), &b);
    ui.apply(Action::Wheel(Id::Job, 3), &b);
    assert_eq!(ui.selected.as_ref().map(|k| k.name.as_str()), Some("a"));
    let newer = job("c", Status::Queued, 1, "C");
    f.machine.projects[0].world.jobs.insert("c".into(), newer);
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!((ui.selected.as_ref().map(|k| k.name.as_str()), ui.index), (Some("a"), 2), "a new row moves it");
    f.machine.projects[0].world.jobs.remove("a");
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected.as_ref().map(|k| k.name.as_str()), Some("b"), "its row's job, once it is gone");
    f.machine.projects[0].world.jobs.clear();
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected, None);
    f.machine.projects[0].world.jobs.insert("d".into(), job("d", Status::Queued, 1, "D"));
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected.as_ref().map(|k| k.name.as_str()), Some("d"), "after an empty list, the first again");
}

#[test]
fn a_click_selects_the_job_under_it_after_the_list_has_scrolled() {
    let jobs: Vec<Job> = (0..60).map(|i| job(&format!("job-{i:02}"), Status::Queued, 100 - i, "work")).collect();
    let f = fixture(&jobs, &[]);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    frame(&b, &mut ui, 80, 24);
    ui.apply(Action::Step(Step::Rows(30)), &b);
    let fr = frame(&b, &mut ui, 80, 24);
    assert!(ui.pane(Id::Jobs).offset > 0, "the list scrolled with the selection");
    assert!(fr.iter().any(|l| l.contains("31 of 60")), "the position in the list");
    let y = row_of(&fr, "job-32");
    let picked = ui.action(&mouse(MouseEventKind::Down(MouseButton::Left), 5, y));
    let Some(Action::Select(i)) = picked else { panic!("{picked:?}") };
    assert_eq!(b.jobs[i].key.name, "job-32");
}

#[test]
fn the_wheel_scrolls_the_pane_under_it_and_keys_act_on_the_focused_one() {
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 30);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 80, 24);
    let accounts = ui.pane(Id::Accounts);
    assert!(accounts.area.height <= 6, "the accounts take at most a quarter: {}", accounts.area.height);
    assert!(fr.iter().any(|l| l.contains("of 30")), "the accounts say where they are");
    let over = mouse(MouseEventKind::ScrollDown, 5, accounts.area.y + 2);
    assert_eq!(ui.action(&over), Some(Action::Wheel(Id::Accounts, 3)));
    ui.apply(Action::Wheel(Id::Accounts, 3), &b);
    assert_eq!((ui.pane(Id::Accounts).offset, ui.index), (3, 0), "the selection stays");
    ui.apply(Action::Focus(Id::Accounts), &b);
    ui.apply(Action::Step(Step::Bottom), &b);
    assert_eq!(ui.pane(Id::Accounts).offset, 30 - ui.pane(Id::Accounts).view);
    assert_eq!(ui.action(&key(KeyCode::BackTab)), Some(Action::Cycle(-1)));
    for (code, step) in
        [(KeyCode::Home, Step::Top), (KeyCode::Char('G'), Step::Bottom), (KeyCode::Char(' '), Step::Pages(1))]
    {
        assert_eq!(ui.action(&key(code)), Some(Action::Step(step)));
    }
}

#[test]
fn every_shown_key_does_what_it_says() {
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    let keys = fr.last().unwrap();
    let click = |ui: &Ui, label: &str| {
        let x = keys[..keys.find(label).unwrap()].chars().count();
        ui.action(&mouse(MouseEventKind::Down(MouseButton::Left), u16::try_from(x).unwrap(), 39))
    };
    assert_eq!(click(&ui, "↑"), Some(Action::Step(Step::Rows(-1))));
    assert_eq!(click(&ui, "↓"), Some(Action::Step(Step::Rows(1))));
    assert_eq!(click(&ui, "PgUp"), Some(Action::Step(Step::Pages(-1))));
    assert_eq!(click(&ui, "PgDn"), Some(Action::Step(Step::Pages(1))));
    assert_eq!(click(&ui, "? help"), Some(Action::Help));
    assert_eq!(click(&ui, "all projects"), Some(Action::ToggleEverywhere));
    assert_eq!(click(&ui, "pager"), Some(Action::Open));
    ui.apply(Action::Help, &b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr.iter().any(|l| l.contains("hold Shift to select text")), "the help");
    assert_eq!(ui.action(&key(KeyCode::Esc)), Some(Action::Help), "a key closes the help, not the board");
    let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(ui.action(&ctrl_c), Some(Action::Quit), "Ctrl-C always quits");
}

#[test]
fn empty_states_say_what_is_hidden_and_how_to_show_it() {
    let f = fixture(&[job("old", Status::Done, 3 * 86_400, "Old")], &[job("theirs", Status::Queued, 1, "T")]);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 0);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    let text = fr.join("\n");
    assert!(text.contains("press a to show 1 older one, or press p for the 1 in other projects"), "{text}");
    assert!(text.contains("No accounts found for claude"), "{text}");
    for (w, h) in [(10, 3), (20, 5), (40, 12), (1, 1)] {
        frame(&b, &mut ui, w, h);
    }
}

#[test]
fn decisions_waiting_are_a_badge_and_a_pane() {
    let mut f = two_jobs();
    let d = Decision {
        id: "build-cap-1".into(),
        job: "build".into(),
        point: crate::bus::Point::Cap,
        created: NOW,
        attempt: 1,
        requested_at: NOW,
        parked_until: Some(NOW + 600_000_000),
        answer: None,
        answer_why: String::new(),
        steps: Vec::new(),
        action: None,
    };
    f.machine.projects[0].world.decisions.insert(d.id.clone(), d);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr[0].contains("1 decision waiting"), "{}", fr[0]);
    assert!(fr.iter().any(|l| l.contains("decisions waiting · relay decide answer")));
    assert!(fr.iter().any(|l| l.contains("build-cap-1")));
    assert!(fr.iter().any(|l| l.contains("decision waiting at cap")), "the job's pane says so too");
    ui.apply(Action::Cycle(1), &b);
    assert_eq!(ui.focus, Id::Decisions, "the pane takes focus in its turn");
}

#[test]
fn a_wheel_scroll_of_the_jobs_list_survives_the_next_read() {
    let jobs: Vec<Job> = (0..60).map(|i| job(&format!("job-{i:02}"), Status::Queued, 100 - i, "work")).collect();
    let f = fixture(&jobs, &[]);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    frame(&b, &mut ui, 80, 24);
    ui.apply(Action::Wheel(Id::Jobs, 3), &b);
    ui.apply(Action::Wheel(Id::Jobs, 3), &b);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    frame(&b, &mut ui, 80, 24);
    assert_eq!((ui.pane(Id::Jobs).offset, ui.index), (6, 0), "the read keeps the list where the wheel left it");
    ui.apply(Action::Step(Step::Rows(1)), &b);
    assert_eq!(ui.pane(Id::Jobs).offset, 1, "moving the selection brings it back into view");
}

#[test]
fn the_selected_row_is_marked_without_colour() {
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 80, 24);
    assert!(fr[row_of(&fr, "Build the") as usize].contains("› build"), "{fr:#?}");
    assert!(!fr[row_of(&fr, "Look at it") as usize].contains('›'));
}

#[test]
fn an_interrupt_while_the_pager_runs_is_the_pagers() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1};
    assert_eq!(super::ending(&[SIGINT], true), None);
    assert_eq!(super::ending(&[SIGINT, SIGUSR1], true), None);
    assert_eq!(super::ending(&[SIGINT], false), Some(SIGINT));
    assert_eq!(super::ending(&[SIGUSR1], false), None, "the board's own handoff signal");
    assert_eq!(super::ending(&[SIGTERM], true), Some(SIGTERM));
    assert_eq!(super::ending(&[SIGHUP], true), Some(SIGHUP));
}

/// The real signal thread: a Ctrl-C that ends the pager reaches the board as well, and the board
/// takes the terminal back at once. Were the interrupt judged after the hand-back, this test's
/// process would exit with 130 and the run would fail.
#[test]
fn an_interrupt_that_ends_the_pager_does_not_end_the_board() {
    let handoff = super::restore_on_signals().unwrap();
    for _ in 0..20 {
        handoff.give();
        signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
        handoff.take_back();
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
}

#[test]
fn a_pager_that_cannot_start_or_fails_says_so_and_an_interrupted_one_is_fine() {
    let err = super::run_pager("relay-no-such-pager 2>/dev/null", "text").unwrap_err().to_string();
    assert!(err.contains("relay-no-such-pager") && err.contains("exit status: 127"), "{err}");
    assert!(err.contains("no such command") && err.contains("shows when the board quits"), "{err}");
    assert!(super::run_pager("exit 3", "text").unwrap_err().to_string().contains("exit status: 3"));
    super::run_pager("cat >/dev/null", "text").unwrap();
    super::run_pager("kill -INT $$", "text").unwrap();
    super::run_pager("head -c1 >/dev/null", &"x".repeat(1 << 20)).unwrap();
}

#[test]
fn a_read_failure_shows_before_the_counts_a_narrow_title_cuts() {
    let mut f = two_jobs();
    for i in 0..8 {
        let jobs = [job(&format!("j{i}"), Status::Queued, 5, "t")];
        let jobs_dir = f.dir.path().join(format!("project-with-a-long-name-{i}"));
        f.machine.projects.push(Project {
            name: format!("project-with-a-long-name-{i}"),
            jobs: jobs_dir,
            world: world(&jobs),
            own: false,
        });
    }
    f.machine.unreadable.push("broken: Permission denied".into());
    let mut ui = Ui::new(false, true, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    ui.error = Some("read failed (board as of 12:00:00Z): Permission denied".into());
    let fr = frame(&b, &mut ui, 80, 24);
    assert!(fr[0].contains("every project · read failed"), "{}", fr[0]);
    let fr = frame(&b, &mut ui, 200, 24);
    assert!(fr[0].contains("unreadable: broken"), "{}", fr[0]);
    assert!(fr[0].find("unreadable").unwrap() < fr[0].find("demo 1 running").unwrap(), "{}", fr[0]);
}

#[test]
fn a_smaller_window_keeps_the_selection_in_view() {
    let jobs: Vec<Job> = (0..60).map(|i| job(&format!("job-{i:02}"), Status::Queued, 100 - i, "work")).collect();
    let f = fixture(&jobs, &[]);
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    frame(&b, &mut ui, 120, 40);
    ui.apply(Action::Step(Step::Bottom), &b);
    assert!(frame(&b, &mut ui, 120, 40).iter().any(|l| l.contains("› job-00")));
    let fr = frame(&b, &mut ui, 80, 24);
    assert!(fr.iter().any(|l| l.contains("› job-00")), "{fr:#?}");
    let b = f.board(&ui, 1);
    ui.sync(&b);
    assert!(frame(&b, &mut ui, 80, 24).iter().any(|l| l.contains("› job-00")), "and after the next read");
}

/// A pager is interactive: its stderr is the board's own terminal, not a pipe of the board's.
#[test]
fn the_pagers_stderr_is_the_boards() {
    let ours = std::fs::read_link("/proc/self/fd/2").unwrap();
    let check = format!("test \"$(readlink /proc/$$/fd/2)\" = '{}' || exit 9", ours.display());
    super::run_pager(&check, "text").unwrap();
}
