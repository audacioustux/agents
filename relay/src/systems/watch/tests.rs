//! The board without a terminal: built from folds, drawn on ratatui's `TestBackend`, driven by
//! the events a terminal would send.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::ChartSpec;
use super::board::Board;
use super::draw::draw;
use super::pane::Step;
use super::ui::{Action, Id, Ui, View};
use crate::config::{Config, tempdir};
use crate::harness::Window;
use crate::systems::dispatch::Room;
use crate::systems::quota::gauge::Gauge;
use crate::systems::quota::{AccountsView, Shown};
use crate::systems::view::machine::Scope;
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
    j.harnesses = vec!["claude".into(), "omp".into()];
    j.models.insert("claude".into(), "claude-opus-5-5".into());
    j.models.insert("omp".into(), "openai-codex/gpt-6-luna".into());
    j
}

fn world(jobs: &[Job]) -> World {
    let mut w = World::default();
    for j in jobs {
        w.jobs_mut().insert(j.name.clone(), j.clone());
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
    let row = |i: usize| Shown {
        account: format!("  claude:a{i}"),
        state: "ready".into(),
        gauges: [(Window::Week, Gauge { used: 0.29, floor: Some(0.9), left: Some(86_400), governing: true })].into(),
        ready: true,
        whole: true,
        ..Shown::default()
    };
    AccountsView {
        json: serde_json::Value::Null,
        head: "accounts on claude · floor 90% · 1 session(s) each · next pick: none".into(),
        rows: (0..n).map(row).collect(),
    }
}

impl Fixture {
    fn board(&self, ui: &Ui, accounts_n: usize) -> Board {
        Board::new(
            &self.cfg,
            &self.machine,
            &Room::default(),
            &accounts(accounts_n),
            ui.shown.all,
            ui.shown.everywhere,
            NOW,
        )
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
        let zone = crate::os::local(jiff::Timestamp::now()).strftime("%Z").to_string();
        assert!(
            fr[0].trim_end_matches(['┓', '┐']).trim_end().ends_with(&zone),
            "the clock says its local zone: {}",
            fr[0]
        );
        assert!(fr[1].contains("job") && fr[1].contains("state") && fr[1].contains("task"), "headings: {}", fr[1]);
        assert!(!fr.iter().any(|l| l.contains("Their task")), "one project by default");
        let keys = &fr[usize::from(h) - 1];
        for hint in ["q quit", "? help", "↑", "↓", "PgUp", "PgDn", "Tab pane", "a older", "p all projects"] {
            assert!(keys.contains(hint), "{w}x{h}: {hint} missing from {keys}");
        }
        assert!(fr.iter().any(|l| l.contains("claude:a1")), "the accounts");
        assert_eq!(b.accounts[0][2], "-", "an empty cell says so");
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
    f.machine.projects[0].world.jobs_mut().insert("c".into(), newer);
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!((ui.selected.as_ref().map(|k| k.name.as_str()), ui.index), (Some("a"), 2), "a new row moves it");
    f.machine.projects[0].world.jobs_mut().remove("a");
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected.as_ref().map(|k| k.name.as_str()), Some("b"), "its row's job, once it is gone");
    f.machine.projects[0].world.jobs_mut().clear();
    let b = f.board(&ui, 0);
    ui.sync(&b);
    assert_eq!(ui.selected, None);
    f.machine.projects[0].world.jobs_mut().insert("d".into(), job("d", Status::Queued, 1, "D"));
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
    // The decision is about the job's current attempt, and its runner (this process) is alive.
    let me = std::process::id();
    let runner = crate::os::start_time(me).map(|started| (me, started));
    let build = f.machine.projects[0].world.jobs_mut().get_mut("build").unwrap();
    (build.attempts, build.runner) = (1, runner);
    let created = build.created_at;
    let d = Decision {
        id: "build-cap-1".into(),
        job: "build".into(),
        point: crate::bus::Point::Cap,
        created,
        attempt: 1,
        waiter: runner,
        requested_at: NOW,
        parked_until: Some(NOW + 600_000_000),
        answer: None,
        answer_why: String::new(),
        steps: Vec::new(),
        action: None,
    };
    f.machine.projects[0].world.decisions.insert(d.id.clone(), d.clone());
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr[0].contains("1 decision waiting"), "{}", fr[0]);
    assert!(fr.iter().any(|l| l.contains("decisions waiting · relay decide answer")));
    assert!(fr.iter().any(|l| l.contains("build-cap-1")));
    assert!(fr.iter().any(|l| l.contains("decision waiting at cap")), "the job's pane says so too");
    // At 80 columns a long job name still leaves the deadline whole, zone included.
    let long =
        Decision { id: "a-rather-long-decision-id-0042".into(), job: "a-rather-long-job-name-x".into(), ..d.clone() };
    let world = &mut f.machine.projects[0].world;
    let long_job = Job { name: long.job.clone(), ..world.jobs()["build"].clone() };
    world.jobs_mut().insert(long_job.name.clone(), long_job);
    world.decisions.insert(long.id.clone(), long);
    let b80 = f.board(&ui, 1);
    let at = crate::systems::decide::clock(NOW + 600_000_000);
    let fr80 = frame(&b80, &mut ui, 80, 24);
    assert!(fr80.iter().any(|l| l.contains("a-rather-long") && l.contains(&at)), "{at} whole at 80 columns: {fr80:#?}");
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

/// Two accounts: one picked next with a week's meter, room on it; one stale with a week's and a
/// five hours', over its floor on the one that decides and near it on the other.
fn gauged() -> AccountsView {
    let g = |used, left, governing| Gauge { used, floor: Some(0.9), left: Some(left), governing };
    let rows = vec![
        Shown {
            account: "▸ claude:ifty".into(),
            state: "ready".into(),
            gauges: [(Window::Week, g(0.28, 149 * 3600, true))].into(),
            room: "12%/d".into(),
            running: "1".into(),
            ready: true,
            sub: false,
            whole: true,
        },
        Shown {
            account: "  claude:tj".into(),
            state: "stale (read 3h ago)".into(),
            gauges: [(Window::Week, g(0.95, 41 * 3600, true)), (Window::FiveHours, g(0.85, 57 * 60, false))].into(),
            room: "—".into(),
            running: "0".into(),
            whole: true,
            ..Shown::default()
        },
    ];
    AccountsView {
        json: serde_json::Value::Null,
        head: "accounts on claude · floor 90% · 1 session(s) each · next pick ▸ claude:ifty".into(),
        rows,
    }
}

/// The accounts pane draws each meter as a gauge in its window's column: a bar with `│` at its
/// floor, green with room, yellow within ten points of the floor, red at it, the meter that
/// decides in bold.
#[test]
fn the_accounts_pane_draws_gauges() {
    use ratatui::style::{Color, Modifier};
    let (fr, buf) = accounts_pane(&gauged(), 120);
    let top = usize::from(row_of(&fr, "next pick"));
    let pane: Vec<&str> = fr[top..top + 5].iter().map(String::as_str).collect();
    assert_eq!(
        pane,
        [
            "┌ accounts on claude · floor 90% · 1 session(s) each · next pick ▸ claude:ifty ────────────────────────────────────────┐",
            "│account        5 hours                   7 days                    room/day  run  state                               │",
            "│▸ claude:ifty  —                         ███▍░░░░░░│░  28%  6d5h   12%/d     1    ready                               │",
            "│  claude:tj    ██████████│░  85%  57m    ██████████│▍  95%  1d17h  —         0    stale (read 3h ago)                 │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘",
        ]
    );
    // The first `│` on a line is the pane's border; the next ones, the gauges' floor marks.
    let mark = |y: usize, n: usize| {
        let x = fr[y].char_indices().filter(|(_, c)| *c == '│').nth(n).map(|(i, _)| fr[y][..i].chars().count());
        &buf[(u16::try_from(x.unwrap()).unwrap(), u16::try_from(y).unwrap())]
    };
    let (ifty, tj_5h, tj) = (mark(top + 2, 1), mark(top + 3, 1), mark(top + 3, 2));
    assert_eq!(ifty.fg, Color::Green, "28%: room");
    assert_eq!(tj_5h.fg, Color::Yellow, "85%: within ten points of the floor");
    assert_eq!(tj.fg, Color::Red, "95%: over it");
    assert!(tj.modifier.contains(Modifier::BOLD) && !tj_5h.modifier.contains(Modifier::BOLD), "the one that decides");
}

/// The board `w` columns wide showing the accounts `v`, as text and as drawn.
fn accounts_pane(v: &AccountsView, w: u16) -> (Vec<String>, ratatui::buffer::Buffer) {
    let fixture = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let board = Board::new(&fixture.cfg, &fixture.machine, &Room::default(), v, false, false, NOW);
    ui.sync(&board);
    let mut term = Terminal::new(TestBackend::new(w, 40)).unwrap();
    term.draw(|fr| draw(fr, &board, &mut ui)).unwrap();
    let buf = term.backend().buffer().clone();
    let fr = (0..40).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect()).collect();
    (fr, buf)
}

/// How many cells each bar on `row` takes, left to right.
fn bars(row: &str) -> Vec<usize> {
    let bar = |c: char| "█▏▎▍▌▋▊▉░".contains(c);
    let inner = row.trim_matches('│');
    let mut out = Vec::new();
    let mut run = 0;
    for c in inner.chars() {
        if bar(c) || (c == '│' && run > 0) {
            run += 1;
        } else if run > 0 {
            out.push(run);
            run = 0;
        }
    }
    out
}

/// A long account name and a busy state beside its meters: the bars give way, down to five
/// cells, before the account's name is cut; the state, last, is cut before either.
#[test]
fn the_bars_give_way_before_a_name_is_cut() {
    let with = |account: &str| {
        let mut v = gauged();
        (v.rows[1].account, v.rows[1].state) = (account.into(), "busy (1 of 1 seat)".into());
        v
    };
    let shown = |w: u16, account: &str| {
        let (fr, _) = accounts_pane(&with(account), w);
        fr[usize::from(row_of(&fr, &account[..12]))].clone()
    };
    let name = "  claude:tangim.thepenguins";
    let row = shown(120, name);
    assert!(row.contains(&format!("│{name}  ")) && row.contains("busy (1 of 1 seat)"), "{row}");
    assert_eq!(bars(&row), [12, 12], "room for all: {row}");
    let row = shown(100, name);
    assert!(row.contains(&format!("│{name}  ")), "the name whole: {row}");
    assert!(bars(&row).iter().all(|&b| (5..12).contains(&b)), "the bars gave way: {row}");
    let row = shown(70, name);
    assert!(bars(&row).iter().all(|&b| b == 5), "a name cut only once its bars are at five cells: {row}");
}

/// Review of #66: an 80-column board with all three windows has no room for a bar in each, so the
/// gauges drop their bars and keep the numbers whole: every percent and every reset time shows.
#[test]
fn an_80_column_board_with_three_windows_keeps_every_reset_time() {
    let g = |used, left, governing| Gauge { used, floor: Some(0.9), left: Some(left), governing };
    let mut v = gauged();
    v.rows[0].account = "▸ claude:tangim.thepenguins".into();
    v.rows[0].gauges.insert(Window::FiveHours, g(0.73, 2 * 3600, false));
    v.rows[1].gauges.insert(Window::Month, g(0.31, 29 * 86_400 + 23 * 3600, false));
    let (fr, _) = accounts_pane(&v, 80);
    let head = &fr[usize::from(row_of(&fr, "│account"))];
    assert!(head.contains("5 hours") && head.contains("7 days") && head.contains("month"), "{head}");
    let first = &fr[usize::from(row_of(&fr, "claude:tang"))];
    let second = &fr[usize::from(row_of(&fr, "claude:tj"))];
    // The one that decides is bold here, as ever on the board, not starred.
    let said: [(&String, &[&str]); 2] =
        [(first, &["73%", "2h", "28%", "6d5h"]), (second, &["85%", "57m", "95%", "1d17h", "31%", "30d"])];
    for (row, said) in said {
        for s in said {
            assert!(row.contains(&format!("{s} ")) || row.contains(&format!("{s}│")), "{s} whole: {row}");
        }
    }
    assert!(!first.contains('░') && !second.contains('█'), "no bar squeezed in: {first}\n{second}");
    // With room, the bars come back.
    let (fr, _) = accounts_pane(&v, 140);
    let wide = &fr[usize::from(row_of(&fr, "claude:tj"))];
    assert_eq!(bars(wide), [12, 12, 12], "{wide}");
}

/// The owner's ask of 2026-11-11: each account's email beside its name; when room runs short the
/// email gives way, down to `…`, before the gauge or a name does.
#[test]
fn an_accounts_email_shows_beside_it_and_gives_way_first() {
    let mut v = gauged();
    v.json = serde_json::json!({ "accounts": [
        { "account": "claude:ifty", "email": "ifty@fixture.invalid" },
        { "account": "claude:tj" },
    ] });
    let row = |w: u16| {
        let (fr, _) = accounts_pane(&v, w);
        let head = fr[usize::from(row_of(&fr, "│account"))].clone();
        (head, fr[usize::from(row_of(&fr, "claude:ifty  "))].clone(), fr[usize::from(row_of(&fr, "claude:tj"))].clone())
    };
    let (head, ifty, tj) = row(120);
    assert!(head.starts_with("│account        email                 5 hours"), "{head}");
    assert!(ifty.starts_with("│▸ claude:ifty  ifty@fixture.invalid  —  "), "{ifty}");
    assert!(tj.starts_with("│  claude:tj                          ███"), "nothing shown: {tj}");
    assert_eq!(bars(&ifty), [12], "{ifty}");
    let (_, ifty, _) = row(100);
    assert!(ifty.contains("ifty@") && ifty.contains("…  —"), "the email gives way first: {ifty}");
    assert_eq!(bars(&ifty), [12], "the gauge whole: {ifty}");
    let (_, ifty, _) = row(80);
    assert!(ifty.starts_with("│▸ claude:ifty  …  —  "), "{ifty}");
    assert!(bars(&ifty)[0] < 12, "then the gauge: {ifty}");
}

/// Machine state with one project's bus, for the chart: readings are appended to it as they land.
struct Buses {
    dir: tempdir::Dir,
    cfg: Config,
}

impl Buses {
    fn new() -> Self {
        let dir = tempdir::Dir::new();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"demo\"\n", dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join("state/demo/bus")).unwrap();
        Self { dir, cfg }
    }

    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("state")
    }

    /// A quota reading of `account`'s 7-day and 5-hour meters, landing on the bus now.
    fn read(&self, account: &str, week: f64, five: f64) {
        use std::io::Write as _;
        let meter = |name: &str, u| (name.to_owned(), crate::harness::Meter::named(name, u, Some(1_800_500_000)));
        let meters: crate::harness::Meters = [meter("seven_day", week), meter("five_hour", five)].into();
        let event = crate::bus::QuotaEvent::Reading { account: account.into(), meters, sign_in: Some("s".into()) };
        let mut v = serde_json::to_value(event).unwrap();
        v["t"] = crate::bus::now_micros().into();
        let path = self.root().join("demo/bus").join(crate::bus::system_file("quota"));
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(f, "{v}").unwrap();
    }

    /// The chart as the watch folds it outside a project, drawn on a 120x40 window.
    fn shown(&self, ui: &mut Ui) -> Vec<String> {
        let root = self.root();
        let scope = Scope { cfg: &self.cfg, home: None, root: &root };
        super::refold(scope, ui, crate::bus::now_micros());
        frame(&Board::default(), ui, 120, 40)
    }
}

fn charting(spec: ChartSpec) -> Ui {
    let mut ui = Ui::new(false, true, false);
    (ui.usage, ui.view) = (spec, View::Usage);
    ui
}

#[test]
fn u_switches_to_the_chart_and_back_and_the_chart_answers_to_few_keys() {
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr.last().unwrap().contains("p all projects  u quota"), "{}", fr.last().unwrap());
    assert_eq!(ui.action(&key(KeyCode::Char('u'))), Some(Action::Chart));
    let ctrl_u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(ui.action(&ctrl_u), Some(Action::Step(Step::Pages(-1))), "Ctrl-U is still a page");
    ui.apply(Action::Chart, &b);
    assert_eq!(ui.view, View::Usage);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(!fr.iter().any(|l| l.contains("Build the thing")), "the board is gone: {fr:#?}");
    assert!(fr.last().unwrap().contains("u board"), "{}", fr.last().unwrap());
    for k in ['a', 'p', 'j'] {
        assert_eq!(ui.action(&key(KeyCode::Char(k))), None, "{k} acts on the board");
    }
    assert_eq!(ui.action(&key(KeyCode::Char('q'))), Some(Action::Quit));
    assert_eq!(ui.action(&key(KeyCode::Char('?'))), Some(Action::Help));
    assert_eq!(ui.action(&key(KeyCode::Char('c'))), Some(Action::Trend), "the chart's key, here");
    assert!(fr.last().unwrap().contains("c chart"), "{}", fr.last().unwrap());
    // The key line is clickable.
    let x = fr.last().unwrap().find("u board").unwrap();
    let x = u16::try_from(fr.last().unwrap()[..x].chars().count()).unwrap();
    assert_eq!(ui.action(&mouse(MouseEventKind::Down(MouseButton::Left), x, 39)), Some(Action::Chart));
    assert_eq!(ui.action(&key(KeyCode::Char('u'))), Some(Action::Chart));
    ui.apply(Action::Chart, &b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr[0].contains("demo · 1 running"), "back on the board: {}", fr[0]);
}

#[test]
fn a_reading_landing_while_the_quota_rows_are_shown_is_in_the_next_draw() {
    let bus = Buses::new();
    bus.read("claude:a", 0.30, 0.1);
    let mut ui = charting(ChartSpec::default());
    let fr = frame(&Board::default(), &mut ui, 120, 40);
    assert!(fr.iter().any(|l| l.contains("reading the quota history")), "before the first fold: {fr:#?}");
    let fr = bus.shown(&mut ui);
    assert!(fr[0].contains("quota since") && fr[0].contains("7-day meters"), "{}", fr[0]);
    assert!(fr.iter().any(|l| l.starts_with("claude:a ") && l.contains(" 30%") && l.contains("resets")), "{fr:#?}");
    bus.read("claude:a", 0.55, 0.2);
    let fr = bus.shown(&mut ui);
    assert!(fr.iter().any(|l| l.starts_with("claude:a ") && l.contains(" 55%")), "{fr:#?}");
    bus.read("omp:w", 0.10, 0.2);
    let fr = bus.shown(&mut ui);
    assert!(fr.iter().any(|l| l.starts_with("omp:w ")), "a new account joins: {fr:#?}");
    assert!(ui.error.is_none());
}

#[test]
fn c_switches_the_quota_between_its_rows_and_the_line_chart() {
    let bus = Buses::new();
    bus.read("claude:a", 0.30, 0.1);
    let mut ui = charting(ChartSpec::default());
    let board = Board::default();
    let fr = bus.shown(&mut ui);
    assert!(!fr.iter().any(|l| l.contains("at the last reading")), "rows first: {fr:#?}");
    assert!(fr.last().unwrap().contains("c chart"), "{}", fr.last().unwrap());
    ui.apply(Action::Trend, &board);
    let fr = frame(&board, &mut ui, 120, 40);
    assert!(fr.iter().any(|l| l.contains("claude:a  30% at the last reading")), "{fr:#?}");
    assert!(fr.last().unwrap().contains("c rows"), "{}", fr.last().unwrap());
    ui.apply(Action::Trend, &board);
    let fr = frame(&board, &mut ui, 120, 40);
    assert!(!fr.iter().any(|l| l.contains("at the last reading")), "rows again: {fr:#?}");
    // `view usage --watch --chart` opens on the chart; the job board has no such key.
    let mut opened = charting(ChartSpec { chart: true, ..ChartSpec::default() });
    let fr = bus.shown(&mut opened);
    assert!(fr.iter().any(|l| l.contains("30% at the last reading")), "{fr:#?}");
    let mut on_board = Ui::new(false, true, false);
    assert_eq!(on_board.action(&key(KeyCode::Char('c'))), None);
    on_board.apply(Action::Chart, &board);
    assert_eq!(on_board.action(&key(KeyCode::Char('c'))), Some(Action::Trend));
}

#[test]
fn the_filters_of_view_usage_watch_apply() {
    let bus = Buses::new();
    bus.read("claude:a", 0.30, 0.11);
    bus.read("omp:w", 0.40, 0.22);
    let spec = |f: crate::systems::view::usage::Filter| ChartSpec { filter: f, ..ChartSpec::default() };
    let filter = crate::systems::view::usage::Filter::default;
    let fr = bus
        .shown(&mut charting(spec(crate::systems::view::usage::Filter { accounts: vec!["omp:w".into()], ..filter() })));
    assert!(fr.iter().any(|l| l.starts_with("omp:w ")), "{fr:#?}");
    assert!(!fr.iter().any(|l| l.contains("claude:a")), "--account: {fr:#?}");
    let fr = bus.shown(&mut charting(spec(crate::systems::view::usage::Filter {
        harnesses: vec!["claude".into()],
        ..filter()
    })));
    assert!(fr.iter().any(|l| l.starts_with("claude:a ")) && !fr.iter().any(|l| l.contains("omp:w")), "--harness");
    let fr = bus.shown(&mut charting(spec(crate::systems::view::usage::Filter {
        meter: Some("five_hour".into()),
        ..filter()
    })));
    assert!(fr[0].contains("meter five_hour"), "{}", fr[0]);
    assert!(fr.iter().any(|l| l.starts_with("claude:a 5 hours") && l.contains(" 11%")), "--meter");
    // `--since`: a window that ends before the readings landed shows none.
    let mut ui = charting(ChartSpec { window: 1, ..ChartSpec::default() });
    std::thread::sleep(std::time::Duration::from_millis(5));
    let fr = bus.shown(&mut ui);
    assert!(fr.iter().any(|l| l.contains("no readings")), "--since: {fr:#?}");
}

/// Every reading left out (a bare label names no harness): the chart says why it has none, in a
/// window with room, in either view.
#[test]
fn a_view_with_no_rows_still_says_what_was_left_out() {
    let bus = Buses::new();
    bus.read("adventor", 0.30, 0.1);
    for chart in [false, true] {
        let mut ui = charting(ChartSpec { chart, ..ChartSpec::default() });
        let fr = bus.shown(&mut ui);
        assert!(fr.iter().any(|l| l.contains("no readings")), "chart={chart}: {fr:#?}");
        assert!(fr.iter().any(|l| l.contains("left out: readings under a bare label")), "chart={chart}: {fr:#?}");
    }
}

/// A window as short as 80x4 has three lines for the quota: the heading, a line for a row or for the
/// notice that rows are left out, and as much of the footer as is left; the key line stays below.
#[test]
fn a_short_window_gives_the_footer_up_before_the_rows() {
    let bus = Buses::new();
    bus.read("claude:a", 0.30, 0.1);
    let mut ui = charting(ChartSpec::default());
    bus.shown(&mut ui);
    let fr = frame(&Board::default(), &mut ui, 80, 4);
    assert_eq!(fr.len(), 4);
    assert!(fr[0].starts_with("quota since"), "{fr:#?}");
    assert!(fr[1].starts_with("claude:a"), "the row stays, the footer gives way: {fr:#?}");
    assert!(fr[3].contains("q quit"), "the key line stays: {fr:#?}");
    // More rows than the line left: the notice says so.
    let bus = Buses::new();
    for a in ["claude:a", "omp:w", "claude:b"] {
        bus.read(a, 0.30, 0.1);
    }
    let mut ui = charting(ChartSpec::default());
    bus.shown(&mut ui);
    let fr = frame(&Board::default(), &mut ui, 80, 4);
    assert!(fr[1..3].iter().any(|l| l.contains("more lines than the window shows")), "{fr:#?}");
    assert!(fr[3].contains("q quit"), "{fr:#?}");
}

#[test]
fn the_help_lists_u() {
    let f = two_jobs();
    let mut ui = Ui::new(false, false, true);
    let b = f.board(&ui, 1);
    ui.sync(&b);
    ui.apply(Action::Help, &b);
    let fr = frame(&b, &mut ui, 120, 40);
    assert!(fr.iter().any(|l| l.contains("the quota rows, or the job board")), "{fr:#?}");
    assert!(fr.iter().any(|l| l.contains("the line chart, or the rows")), "{fr:#?}");
}

#[test]
fn the_chart_fits_a_small_window_and_reads_nothing_it_cannot_write() {
    let bus = Buses::new();
    bus.read("claude:a", 0.30, 0.1);
    let before = std::fs::read_dir(bus.dir.path()).unwrap().count();
    let mut ui = charting(ChartSpec::default());
    bus.shown(&mut ui);
    for chart in [false, true] {
        ui.usage.chart = chart;
        for (w, h) in [(80, 24), (40, 12), (20, 5), (30, 2)] {
            let fr = frame(&Board::default(), &mut ui, w, h);
            assert_eq!(fr.len(), usize::from(h));
        }
    }
    assert_eq!(std::fs::read_dir(bus.dir.path()).unwrap().count(), before, "nothing written");
}
