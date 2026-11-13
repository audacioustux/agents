//! view: read-only. Who is working on what, and what each session said.

pub mod machine;
mod stats;
pub mod usage;

use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;

use crate::bus::now_micros;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::systems::dispatch::Room;
use crate::systems::quota::AccountsView;
use crate::world::{Job, Wait, World};
use machine::{Machine, Scope};

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Jobs and accounts. With `--json`, the whole folded state.
    /// Outside a project, every project on this machine.
    Status {
        /// Include jobs that ended more than a day ago.
        #[arg(long)]
        all: bool,
        /// Every project on this machine, each job with its project.
        #[arg(long)]
        everywhere: bool,
    },
    /// Live: the status in a terminal UI, redrawn as events land, with the
    /// selected job's latest output and answer. `?` lists its keys; `q` ends it.
    /// Outside a project, every project on this machine.
    Watch {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        everywhere: bool,
    },
    /// A job's final answer, branch, spend and log.
    Result {
        name: String,
        /// First wait until the job is no longer queued or running.
        #[arg(long)]
        wait: bool,
    },
    /// The merged event log, oldest first, one JSON object per line.
    Events,
    /// Per profile and model: jobs by outcome, spend, retries, decisions, checks, reviews, waits and the
    /// accounts' shares, folded from the events already on the bus.
    Stats {
        /// Only jobs queued within this long (`30m`, `12h`, `7d`, `2w`). Default: all of them.
        #[arg(long)]
        since: Option<String>,
    },
    /// Each account's quota as a row (gauge, sparkline, pace, and what the pace comes to by the
    /// reset): by default its 7-day meter. With `--chart`, the line chart over time instead; with
    /// `--json`, the rows and readings. Outside a project, every project's.
    Usage {
        /// Another meter, by name (`five_hour`, `openai-codex/5 hours`) or as `view status` shows it (`5 hours`).
        #[arg(long)]
        meter: Option<String>,
        /// Only this account (`harness:label`); repeatable.
        #[arg(long = "account", value_name = "HARNESS:LABEL")]
        accounts: Vec<String>,
        /// Only this harness's accounts; repeatable.
        #[arg(long = "harness", value_name = "HARNESS")]
        harnesses: Vec<String>,
        /// How far back (`12h`, `7d`, `2w`).
        #[arg(long, default_value = "7d")]
        since: String,
        /// Live: open `view watch` on the rows, refolded as readings land (`c` the chart, `u` the job board, `q` quits).
        #[arg(long)]
        watch: bool,
        /// The line chart of every account's level over the window, not the rows.
        #[arg(long)]
        chart: bool,
    },
}

impl Cmd {
    /// The boards and the usage chart run outside a project too, over every project on this machine.
    pub fn runs_anywhere(&self) -> bool {
        matches!(self, Self::Status { .. } | Self::Watch { .. } | Self::Usage { .. })
    }
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    let scope = Scope { cfg: &ctx.cfg, home: Some(ctx), root: &ctx.state_root };
    match cmd {
        Cmd::Status { all, everywhere } => status(scope, ctx.json, all, everywhere),
        Cmd::Watch { all, everywhere } => crate::systems::watch::run(scope, all, everywhere, None),
        Cmd::Result { name, wait } => result(ctx, &name, wait),
        Cmd::Stats { since } => stats::run(ctx, since.as_deref()),
        Cmd::Usage { meter, accounts, harnesses, since, watch, chart } => usage::run(
            scope,
            ctx.json,
            usage::Filter { meter, accounts, harnesses, ..Default::default() },
            &since,
            watch,
            chart,
        ),
        Cmd::Events => {
            for e in ctx.events()? {
                out!("{}", e.to_json());
            }
            Ok(())
        }
    }
}

pub fn ago(now_us: u64, then_us: u64) -> String {
    let s = now_us.saturating_sub(then_us) / 1_000_000;
    match s {
        0..120 => format!("{s}s"),
        120..7200 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// A board outside any project (`cmd` is one that [`Cmd::runs_anywhere`]): every project on
/// this machine, judged by the owner's policy alone.
pub fn outside(json: bool, cmd: &Cmd) -> Result<()> {
    let cfg = Config::policy_only()?;
    let root = crate::ctx::state_root();
    let scope = Scope { cfg: &cfg, home: None, root: &root };
    match cmd {
        Cmd::Status { all, .. } => status(scope, json, *all, true),
        Cmd::Watch { all, .. } => crate::systems::watch::run(scope, *all, true, None),
        Cmd::Usage { meter, accounts, harnesses, since, watch, chart } => {
            let filter = usage::Filter {
                meter: meter.clone(),
                accounts: accounts.clone(),
                harnesses: harnesses.clone(),
                ..Default::default()
            };
            usage::run(scope, json, filter, since, *watch, *chart)
        }
        _ => unreachable!("only the boards and the usage chart run outside a project"),
    }
}

/// The folded state, with what each job waits for.
fn world_json(world: &World) -> Result<serde_json::Value> {
    let mut v = serde_json::to_value(world)?;
    for j in world.jobs().values() {
        v["jobs"][&j.name]["pending"] = serde_json::to_value(world.waits(j))?;
        v["jobs"][&j.name]["readonly"] = j.readonly().into();
        v["jobs"][&j.name]["compiles"] = j.compiles().into();
    }
    Ok(v)
}

fn status(scope: Scope<'_>, json: bool, all: bool, everywhere: bool) -> Result<()> {
    if json {
        let (mut v, refreshing) = match scope.home {
            Some(ctx) if !everywhere => {
                let world = ctx.world()?;
                let neighbours = ctx.neighbours();
                let refreshing = refresh_shown(ctx, &world, &neighbours.iter().collect::<Vec<_>>())?;
                (world_json(&world)?, refreshing)
            }
            _ => {
                let m = scope.read()?;
                let projects: serde_json::Map<String, serde_json::Value> =
                    m.projects.iter().map(|p| Ok((p.name.clone(), world_json(&p.world)?))).collect::<Result<_>>()?;
                let refreshing = match scope.home {
                    Some(ctx) => refresh_shown(
                        ctx,
                        m.own(),
                        &m.projects.iter().filter(|p| !p.own).map(|p| &p.world).collect::<Vec<_>>(),
                    )?,
                    None => Vec::new(),
                };
                (serde_json::json!({ "projects": projects, "unreadable": m.unreadable }), refreshing)
            }
        };
        // Beside the folded state, never in it: the accounts whose reading was asked for, as `quota show --json` says.
        if !refreshing.is_empty() {
            v["refreshing"] = refreshing.into();
        }
        out!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let m = scope.read()?;
    let room = m.room(scope.home);
    let accounts = shown_accounts(scope, &m, &room)?;
    // On a terminal, one screen line per line, the meters' bars shrunk first; piped, every line whole.
    let width = crate::os::terminal_size().map(|(cols, _)| cols);
    let text = board(scope.cfg, &m, &room, &accounts, Shown { all, everywhere, width });
    out!("{}", fit(&text, width));
    Ok(())
}

/// Asks for a reading of each stale account of the project relay runs in (`ctx`), judged by its
/// rules among `others` (`quota::start_refresh`, which reads one at most once per staleness
/// window), and returns those whose reading was asked for. What every view of the accounts does,
/// `view status --json` included: an orchestrator that only polls that still gets fresh readings.
fn refresh_shown(ctx: &Ctx, world: &World, others: &[&World]) -> Result<Vec<String>> {
    // A room for the accounts' readings alone: nothing is admitted in it, so nothing is counted.
    let others = crate::ctx::Others { worlds: others.to_vec(), unreadable: Vec::new() };
    let room = crate::systems::quota::room_for(crate::systems::quota::Rules::Project(ctx), world, &others);
    crate::systems::quota::start_refresh(ctx, &room, world, false)
}

/// The accounts as a board shows them, each with its email on a terminal (`AccountsView::show_emails`);
/// in a project, a reading of each stale one is started first (`quota::start_refresh`), and the view
/// says so: a reading is taken when it is shown.
pub fn shown_accounts(scope: Scope<'_>, m: &Machine, room: &Room) -> Result<AccountsView> {
    let mut v = m.accounts(scope.home, room);
    if let Some(ctx) = scope.home {
        v.refreshing(crate::systems::quota::start_refresh(ctx, room, m.own(), false)?);
    }
    v.show_emails(room);
    Ok(v)
}

/// Rows as aligned text: each column as wide as its widest cell, a column
/// empty in every row left out, and the last column never padded.
pub fn table(rows: &[Vec<String>]) -> Vec<String> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let width = |c: usize| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..n).map(width).collect();
    rows.iter()
        .map(|r| {
            let cells = r.iter().enumerate().filter(|&(c, _)| widths[c] > 0);
            let padded = cells.map(|(c, s)| if c + 1 == n { s.clone() } else { format!("{s:<w$}", w = widths[c]) });
            padded.collect::<Vec<_>>().join("  ").trim_end().to_owned()
        })
        .collect()
}

/// Cuts each line to `width` columns, marking the cut.
pub(crate) fn fit(text: &str, width: Option<usize>) -> String {
    let cut = |line: &str| match width {
        Some(w) if line.chars().count() > w => {
            line.chars().take(w.saturating_sub(1)).chain(std::iter::once('…')).collect::<String>()
        }
        _ => line.to_owned(),
    };
    text.lines().map(cut).collect::<Vec<_>>().join("\n")
}

/// Whether a board lists `j` without `--all`: it is live or ended within a day.
pub fn recent(j: &Job, now: u64) -> bool {
    let day_ago = now.saturating_sub(24 * 3600 * 1_000_000);
    j.status.is_live() || j.ended_at.is_some_and(|t| t > day_ago)
}

/// The jobs worth showing in `projects` (recent ones, or all), in board order, each with its project.
pub fn listed<'a>(
    projects: impl IntoIterator<Item = &'a machine::Project>,
    all: bool,
    now: u64,
) -> Vec<(&'a machine::Project, &'a Job)> {
    let mut jobs: Vec<_> = projects
        .into_iter()
        .flat_map(|p| p.world.jobs().values().map(move |j| (p, j)))
        .filter(|(_, j)| all || recent(j, now))
        .collect();
    jobs.sort_by_key(|(_, j)| board_order(j));
    jobs
}

/// Where a job sorts on the board: by status (running, checking, queued, failed, done, stopped), and
/// within one status by time, the latest first (when it ended, else started, else was queued).
fn board_order(j: &Job) -> (u8, std::cmp::Reverse<u64>) {
    use crate::world::Status;
    let rank = match j.status {
        Status::Running => 0,
        Status::Checking => 1,
        Status::Queued => 2,
        Status::Failed => 3,
        Status::Done => 4,
        Status::Stopped => 5,
    };
    (rank, std::cmp::Reverse(j.ended_at.or(j.started_at).unwrap_or(j.created_at)))
}

/// What a job is asked to run as: its profile, else the model it names, then its effort.
pub fn kind(j: &Job) -> String {
    let what = j.profile.clone().unwrap_or_else(|| j.models.values().cloned().collect::<Vec<_>>().join(","));
    match j.effort.as_deref() {
        Some(effort) => format!("{what} {effort}"),
        None => what,
    }
}

/// The model a job runs, as (harness, full id).
///
/// Which harnesses: the one of its account while it runs on it (running or checking), or after,
/// while that harness is still among the job's (an empty list allows any); otherwise every one
/// it may run on, which is all the enabled harnesses when it names none. A requeue or reroute
/// that moves a job off its old account's harness thus shows the new harness at once.
///
/// Which model: on its account's harness, the one its dispatch recorded (any project's job), and
/// otherwise [`crate::systems::dispatch::model_for`], the rule dispatch runs by, so a harness
/// the job sets no model on shows the configured one. That needs the project's config (`cfg`),
/// which only the project relay runs in has loaded: without it (another project's job in the
/// every-project view) only the models the job itself names show, the defaults being unknown.
/// A job back in the queue has no account, so it shows every harness it may run on.
pub fn models(cfg: Option<&Config>, j: &Job) -> Vec<(String, String)> {
    let ran = j.account.as_deref().and_then(|a| a.split(':').next());
    let narrowed =
        ran.filter(|r| j.status.has_runner() || j.harnesses.is_empty() || j.harnesses.iter().any(|h| h == r));
    let recorded = narrowed.and(j.model.as_ref());
    let eligible: Vec<String> = match (narrowed, cfg) {
        (Some(r), _) => vec![r.to_owned()],
        (None, _) if !j.harnesses.is_empty() => j.harnesses.clone(),
        (None, Some(cfg)) => cfg.harnesses().iter().map(|h| h.id().to_owned()).collect(),
        (None, None) => j.models.keys().cloned().collect(),
    };
    eligible
        .into_iter()
        .filter_map(|h| {
            let model = match (recorded, cfg) {
                (Some(m), _) => Some(m.clone()),
                (None, Some(cfg)) => crate::systems::dispatch::model_for(cfg, j, &h),
                (None, None) => j.models.get(&h).cloned(),
            };
            model.map(|m| (h, m))
        })
        .collect()
}

/// A model id short enough for a table, its version kept: the provider (`openai-codex/`) and a
/// leading `claude-` dropped and a version's numbers joined by dots, so `claude-opus-5-5` is
/// `opus-5.5`. An id that is a codename after `gpt-N-` is the codename, so `gpt-6-luna` is `luna`.
pub fn short_model(id: &str) -> String {
    let id = id.rsplit('/').next().unwrap_or(id);
    let id = id.strip_prefix("claude-").unwrap_or(id);
    if let Some((version, name)) = id.strip_prefix("gpt-").and_then(|r| r.split_once('-'))
        && !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return name.to_owned();
    }
    let mut out = String::new();
    let mut prev_number = false;
    for part in id.split('-') {
        let number = part.len() <= 2 && !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !out.is_empty() {
            out.push(if number && prev_number { '.' } else { '-' });
        }
        out.push_str(part);
        prev_number = number;
    }
    out
}

/// A job's model cell: each model's short form, comma-separated.
fn model_cell(cfg: Option<&Config>, j: &Job) -> String {
    let mut shorts: Vec<String> = models(cfg, j).into_iter().map(|(_, m)| short_model(&m)).collect();
    shorts.dedup();
    shorts.join(",")
}

/// Why a job waits or ended: a queued (or held) job's dependencies or, with its project's config and the
/// room it is judged in (`own`), what holds it there, as the dispatcher judges it
/// ([`Room::hold`]: a profile limit, free disk, or every account's cause); else the reason recorded.
pub fn reason(own: Option<(&Config, &Room)>, world: &World, j: &Job, now: u64) -> Option<String> {
    let queued = j.status == crate::world::Status::Queued;
    // A job the disk floor holds waits for its prerequisites too, and what blocks it there comes before the disk.
    let waits = if queued || j.held { Wait::summary(&world.waits(j)) } else { None };
    let held = if queued && waits.is_none() { own.and_then(|(c, room)| room.hold(c, j, now)) } else { None };
    waits.or(held.map(|h| h.why)).or_else(|| held_by_floor(own.map(|(c, _)| c), j)).or_else(|| j.reason.clone())
}

/// What a job the disk floor holds says: the reason it was stopped, and the free space it waits for
/// (the floor and the margin above it, from its project's config when this reader has it).
fn held_by_floor(cfg: Option<&Config>, j: &Job) -> Option<String> {
    if !j.held {
        return None;
    }
    let why = j.reason.as_deref().unwrap_or("disk floor");
    Some(match cfg {
        Some(c) => format!("held: {why}; resumes at {} GB free", crate::systems::dispatch::resume_gb(&c.limits)),
        None => format!("held: {why}; resumes when space is back"),
    })
}

/// What a job's row says after its task and reason: its latest checkpoint, check and review,
/// how far it overran, and what gc took.
pub fn notes(j: &Job) -> Vec<String> {
    let mut notes: Vec<String> = j.checkpoints.last().map(crate::bus::Check::summary).into_iter().collect();
    notes.extend(overran(j));
    notes.extend(declared(j));
    // The default (it compiles) goes unsaid; the exception is what a reader needs to see.
    notes.extend((!j.compiles()).then(|| "compiles nothing".to_owned()));
    notes.extend(collected(j).map(str::to_owned));
    notes
}

/// A job's cells: name, status, account, profile (or model) with effort, its model (short form),
/// since when, spend, disk, and its task with its [`reason`] and [`notes`]. `own` is its project's
/// config and room, when it is the project relay runs in.
pub fn job_cells(own: Option<(&Config, &Room)>, world: &World, j: &Job, now: u64) -> Vec<String> {
    let cfg = own.map(|(c, _)| c);
    let since = match j.status {
        crate::world::Status::Running | crate::world::Status::Checking => j.started_at,
        crate::world::Status::Queued => j.ended_at.or(Some(j.created_at)),
        _ => j.ended_at,
    };
    let reason = reason(own, world, j, now).map(|r| format!("[{r}]"));
    let task = std::iter::once(j.task.clone()).chain(reason).chain(notes(j)).collect::<Vec<_>>().join("  ");
    vec![
        j.name.clone(),
        j.status.as_str().to_owned(),
        j.account.as_deref().or(j.pinned.as_deref()).unwrap_or("-").to_owned(),
        kind(j),
        model_cell(cfg, j),
        since.map_or_else(|| "-".into(), |t| ago(now, t)),
        if j.cost_usd > 0.0 { format!("${:.2}", j.cost_usd) } else { String::new() },
        j.disk_bytes.filter(|_| j.status.has_runner()).map_or_else(String::new, crate::os::size_str),
        task,
    ]
}

/// How far past its cap the job's finished run spent, for its board row.
fn overran(j: &Job) -> Option<String> {
    j.overran.map(|(spent, cap)| format!("over budget: ${spent:.2} of ${cap:.2}"))
}

/// The latest acceptance check and the latest review of a job, for its board row.
fn declared(j: &Job) -> Vec<String> {
    let left = j.unchained.as_ref().map_or(String::new(), |u| format!(" ({})", u.summary()));
    let mut out: Vec<String> = j.review_of().map(|b| format!("reviews {b}{left}")).into_iter().collect();
    out.extend(j.bars.last().map(|b| format!("check {}", b.summary())));
    // Only the latest reviewed commit's verdicts: an earlier commit's reject is history, not the state.
    let latest = j.verdicts.last().map(|l| &l.commit);
    for (i, v) in j.verdicts.iter().enumerate() {
        if Some(&v.commit) == latest && !j.verdicts[i + 1..].iter().any(|n| n.reviewer == v.reviewer) {
            out.push(format!("review by {}: {}", v.reviewer, v.summary()));
        }
    }
    out
}

/// What gc took from a job's clone, for its board row.
fn collected(j: &Job) -> Option<&'static str> {
    match j.collected.as_deref() {
        Some("clone") => Some("(clone removed)"),
        Some(_) => Some("(build outputs removed)"),
        None => None,
    }
}

/// A listed job's cells, its project's name first when the board shows every project. `room` is
/// the one ours is judged in.
pub fn row_cells(cfg: &Config, room: &Room, p: &machine::Project, j: &Job, everywhere: bool, now: u64) -> Vec<String> {
    let cells = job_cells(p.own.then_some((cfg, room)), &p.world, j, now);
    if everywhere { std::iter::once(p.name.clone()).chain(cells).collect() } else { cells }
}

/// The decisions waiting for the orchestrator in `projects`, oldest first: id, job, point and until
/// when, the project first when the board shows every project.
pub fn decision_cells<'a>(
    projects: impl IntoIterator<Item = &'a machine::Project>,
    everywhere: bool,
    now: u64,
) -> Vec<Vec<String>> {
    let mut waiting: Vec<_> = projects
        .into_iter()
        .flat_map(|p| crate::systems::decide::pending(&p.world, now).into_iter().map(move |d| (p, d)))
        .collect();
    waiting.sort_by_key(|(_, d)| d.requested_at);
    waiting
        .into_iter()
        .map(|(p, d)| {
            // Bare: the board heads the column "until", and the text view says it in each row.
            let until = crate::systems::decide::clock(d.parked_until.unwrap_or(0));
            let cells = [d.id.clone(), d.job.clone(), d.point.as_str().to_owned(), until];
            everywhere.then(|| p.name.clone()).into_iter().chain(cells).collect()
        })
        .collect()
}

/// What a text board shows: older jobs too, every project, and in how many columns.
#[derive(Debug, Clone, Copy)]
struct Shown {
    all: bool,
    everywhere: bool,
    width: Option<usize>,
}

/// The jobs and accounts, as text.
fn board(cfg: &Config, m: &Machine, room: &Room, accounts: &AccountsView, shown: Shown) -> String {
    let Shown { all, everywhere, width } = shown;
    let now = now_micros();
    let everywhere = everywhere || !m.projects.iter().any(|p| p.own);
    let jobs = listed(m.shown(everywhere), all, now);
    let rows: Vec<Vec<String>> = jobs.iter().map(|(p, j)| row_cells(cfg, room, p, j, everywhere, now)).collect();
    let mut lines = vec![if everywhere {
        "jobs in every project".to_owned()
    } else {
        format!("jobs (at most {} running)", cfg.limits.max_running)
    }];
    lines.extend(table(&rows).into_iter().map(|r| format!("  {r}")));
    if jobs.is_empty() {
        lines.push("  (none)".into());
    }
    let waiting = decision_cells(m.shown(everywhere), everywhere, now);
    if !waiting.is_empty() {
        lines.push(String::new());
        lines.push("decisions waiting (relay decide answer ID --action ...)".into());
        let waiting: Vec<Vec<String>> = waiting
            .into_iter()
            .map(|mut r| {
                if let Some(at) = r.last_mut() {
                    *at = format!("until {at}");
                }
                r
            })
            .collect();
        lines.extend(table(&waiting).into_iter().map(|r| format!("  {r}")));
    }
    if everywhere {
        lines.extend(m.unreadable.iter().map(|u| format!("  unreadable: {u}")));
    }
    lines.push(String::new());
    lines.push(accounts.text_in(width));
    lines.join("\n")
}

/// When the session last said something, and the gist of it: the last text
/// or tool name in its output, whichever harness wrote it.
pub fn activity(log: &std::path::Path) -> String {
    let Ok(meta) = std::fs::metadata(log) else { return "no output yet".into() };
    let quiet = meta.modified().ok().and_then(|m| m.elapsed().ok()).map_or(0, |d| d.as_secs());
    let tail = read_tail(log, 64 * 1024);
    let gist = tail.lines().rev().find_map(gist).unwrap_or_default();
    format!("{quiet}s since output  {gist}")
}

fn read_tail(path: &std::path::Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// A line's last tool call or text, from any JSON shape: harnesses differ,
/// but both name tools `name` and say things in `text`.
fn gist(line: &str) -> Option<String> {
    fn find(v: &serde_json::Value, out: &mut Option<String>) {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("type").and_then(|t| t.as_str()).is_some_and(|t| t.contains("tool"))
                    && let Some(n) = m.get("name").and_then(|n| n.as_str())
                {
                    *out = Some(format!("→ {n}"));
                }
                if let Some(t) = m.get("text").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty()) {
                    *out = Some(t.trim().replace('\n', " "));
                }
                m.values().for_each(|v| find(v, out));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| find(v, out)),
            _ => {}
        }
    }
    let mut out = None;
    find(&serde_json::from_str(line).ok()?, &mut out);
    out.map(|g| g.chars().take(90).collect())
}

fn result(ctx: &Ctx, name: &str, wait: bool) -> Result<()> {
    let j = &if wait { ended(ctx, name)? } else { job(&ctx.world()?, name)? };
    let answer = std::fs::read_to_string(ctx.job_file(name, "result.md"))
        .ok()
        .or_else(|| answer_in(&ctx.read_job_log(name, "log.ndjson")));
    let log = ctx.job_log(name, "log.ndjson");
    let logs = ctx.job_logs(name, "log.ndjson");
    if ctx.json {
        let mut v = serde_json::to_value(j)?;
        v["readonly"] = j.readonly().into();
        v["compiles"] = j.compiles().into();
        v["answer"] = answer.as_deref().map(str::trim_end).into();
        v["log"] = log.display().to_string().into();
        v["logs"] = logs.iter().map(|l| l.display().to_string()).collect::<Vec<_>>().into();
        return ctx.reply(&v, "");
    }
    out!(
        "{}: {}{} on {}, ${:.2}, {} turns, {} attempt(s)",
        j.name,
        j.status.as_str(),
        j.reason.as_deref().map_or(String::new(), |r| format!(" ({r})")),
        j.account.as_deref().unwrap_or("-"),
        j.cost_usd,
        j.turns,
        j.attempts
    );
    if let Some((spent, cap)) = j.overran {
        out!("over budget: spent ${spent:.2} of a ${cap:.2} cap, but its report says status: done");
    }
    if let Some(c) = j.checkpoints.last() {
        out!("{}", c.summary());
        if c.next_cap_usd.is_some() && !c.why.is_empty() {
            out!("  why: {}", c.why);
        }
    }
    if let Some(b) = j.bars.last() {
        out!(
            "check {}{}exit {}{}{}",
            b.summary(),
            if b.failing.is_empty() { ": " } else { "; " },
            b.exit,
            b.base_exit.map_or(String::new(), |e| format!(", exit {e} at the base commit")),
            b.commit.as_deref().map_or(String::new(), |c| format!(", at {}", c.get(..12).unwrap_or(c)))
        );
    }
    for v in &j.verdicts {
        out!("review by {} at {}: {}", v.reviewer, v.commit.get(..12).unwrap_or(&v.commit), v.summary());
    }
    if let (Some(b), Some(w)) = (&j.branch, &j.workdir) {
        out!("branch {b} in {}   bring it here: relay land fetch {name}", w.path().display());
    }
    for l in if logs.is_empty() { std::slice::from_ref(&log) } else { &logs[..] } {
        out!("log {}", l.display());
    }
    out!("");
    out!("{}", answer.as_deref().map_or("(no result yet)", str::trim_end));
    Ok(())
}

/// The session's last answer in its stream, for a job whose `result.md` is gone.
fn answer_in(log: &str) -> Option<String> {
    log.lines().rev().find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        if v["type"] != "result" {
            return None;
        }
        v["result"].as_str().map(str::to_owned)
    })
}

fn job(world: &World, name: &str) -> Result<Job> {
    let Some(j) = world.jobs().get(name) else { fail!(NotFound, "no job named {name}") };
    Ok(j.clone())
}

/// The job once it has ended and its runner has exited (`Job::runner_alive`), so
/// nothing of that run is still at work on its files. Sleeps on the doorbell
/// between reads, and wakes the dispatcher whenever the bus has been quiet, so
/// the wait cannot outlive it.
/// A live job that does not move on its own (`World::moves_on_its_own`: queued
/// behind a dependency that will never be done) would never end: that is a
/// conflict (exit 4), naming what it waits for.
/// How often a wait looks again for an ended job's runner to exit.
const RUNNER_EXIT_LOOK: Duration = Duration::from_millis(100);

fn ended(ctx: &Ctx, name: &str) -> Result<Job> {
    let bell = ctx.doorbell()?;
    let mut quiet = true;
    loop {
        let world = ctx.world()?;
        let j = job(&world, name)?;
        // A job the disk floor holds has not ended: the dispatcher resumes it when space is back.
        if !j.status.is_live() && !j.held {
            if !j.runner_alive() {
                return Ok(j);
            }
            // A runner's exit rings no bell: look again shortly.
            bell.wait(RUNNER_EXIT_LOOK);
            continue;
        }
        if !world.moves_on_its_own(&j) {
            let why = Wait::summary(&world.waits(&j)).unwrap_or_else(|| "cannot start on its own".into());
            fail!(
                Conflict,
                "{name} {why}, so it will not end on its own: queue that job again, or relay queue release {name}"
            );
        }
        if quiet {
            ctx.wake_dispatch()?;
        }
        quiet = !bell.wait(Duration::from_mins(5));
    }
}

#[cfg(test)]
mod tests {
    use super::{answer_in, board_order, fit, models, short_model, table};

    #[test]
    fn a_model_id_is_short_and_keeps_its_version() {
        for (id, short) in [
            ("claude-opus-5-5", "opus-5.5"),
            ("claude-sonnet-5-5", "sonnet-5.5"),
            ("claude-haiku-4-5-20251001", "haiku-4.5-20251001"),
            ("claude-3-5-sonnet", "3.5-sonnet"),
            ("openai-codex/gpt-6-luna", "luna"),
            ("gpt-6-luna", "luna"),
            ("openai-codex/gpt-5.1-codex-max", "gpt-5.1-codex-max"),
            ("gpt-4o", "gpt-4o"),
            ("some-model", "some-model"),
            ("", ""),
        ] {
            assert_eq!(short_model(id), short, "{id}");
        }
    }

    /// A project config with both harnesses on, each with a default model.
    fn both() -> (crate::config::tempdir::Dir, crate::config::Config) {
        let dir = crate::config::tempdir::Dir::new();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        // Both harnesses as programs of the test's own, and omp's `bun` recorded: whatever this machine has.
        let program = |name: &str| {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.path().join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let (claude, omp, bun) = (program("claude"), program("omp"), program("bun"));
        let record = dir.path().join("programs.json");
        std::fs::write(&record, serde_json::json!({ "bun": bun }).to_string()).unwrap();
        let toml = format!(
            "name = \"demo\"\n[harness.claude]\nenabled = true\nprogram = {:?}\nmodel = \"claude-sonnet-5-5\"\n\
             [harness.omp]\nenabled = true\nprogram = {:?}\nmodel = \"openai-codex/gpt-6-luna\"\n",
            claude.display().to_string(),
            omp.display().to_string()
        );
        let mut cfg = crate::config::Config::parse(&toml, dir.path()).unwrap();
        cfg.programs = Some(record);
        (dir, cfg)
    }

    fn pairs(v: Vec<(String, String)>) -> Vec<(&'static str, &'static str)> {
        v.into_iter().map(|(h, m)| (&*h.leak(), &*m.leak())).collect()
    }

    #[test]
    fn a_job_shows_every_model_it_may_run_on_until_an_account_is_the_one_that_ran() {
        use crate::world::{Job, Status};
        let mut j = Job::default();
        j.models.insert("claude".into(), "claude-opus-5-5".into());
        j.models.insert("omp".into(), "openai-codex/gpt-6-luna".into());
        assert_eq!(pairs(models(None, &j)), [("claude", "claude-opus-5-5"), ("omp", "openai-codex/gpt-6-luna")]);
        j.account = Some("omp:work".into());
        j.status = Status::Running;
        assert_eq!(pairs(models(None, &j)), [("omp", "openai-codex/gpt-6-luna")]);
    }

    #[test]
    fn a_requeue_that_changes_harness_shows_the_new_models_not_the_old_accounts() {
        use crate::world::{Job, Status};
        // Ran on claude, then requeued onto omp: the old account stays on the job until dispatch.
        let mut j = Job { status: Status::Queued, account: Some("claude:a".into()), ..Job::default() };
        j.harnesses = vec!["omp".into()];
        j.models.insert("omp".into(), "openai-codex/gpt-6-luna".into());
        assert_eq!(pairs(models(None, &j)), [("omp", "openai-codex/gpt-6-luna")]);
        assert_eq!(short_model(&models(None, &j)[0].1), "luna");
        // While it is still running on that account, the account's harness is the one.
        j.status = Status::Running;
        j.harnesses = vec!["omp".into(), "claude".into()];
        j.models.insert("claude".into(), "claude-opus-5-5".into());
        assert_eq!(pairs(models(None, &j)), [("claude", "claude-opus-5-5")]);
        // Ended with its harness still among the job's: that one ran.
        j.status = Status::Done;
        assert_eq!(pairs(models(None, &j)), [("claude", "claude-opus-5-5")]);
    }

    #[test]
    fn a_harness_the_job_sets_no_model_on_shows_the_configured_one() {
        use crate::world::{Job, Status};
        let (_dir, cfg) = both();
        // No overrides at all: every enabled harness's configured model.
        let mut j = Job { status: Status::Queued, ..Job::default() };
        assert_eq!(
            pairs(models(Some(&cfg), &j)),
            [("claude", "claude-sonnet-5-5"), ("omp", "openai-codex/gpt-6-luna")]
        );
        // A partial override leaves the other harness at its default.
        j.harnesses = vec!["claude".into(), "omp".into()];
        j.models.insert("claude".into(), "claude-opus-5-5".into());
        assert_eq!(pairs(models(Some(&cfg), &j)), [("claude", "claude-opus-5-5"), ("omp", "openai-codex/gpt-6-luna")]);
        // Without the project's config (another project's job), only the overrides show.
        assert_eq!(pairs(models(None, &j)), [("claude", "claude-opus-5-5")]);
        // The same rule as dispatch's, for the one that runs.
        j.account = Some("omp:w".into());
        j.status = Status::Running;
        let want = crate::systems::dispatch::model_for(&cfg, &j, "omp").unwrap();
        assert_eq!(models(Some(&cfg), &j), [("omp".to_owned(), want)]);
    }

    /// A config with claude alone, one seat per account, and a profile of at most one running job.
    fn one_seat() -> (crate::config::tempdir::Dir, crate::config::Config) {
        let dir = crate::config::tempdir::Dir::new();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let toml = "name = \"demo\"\n[limits]\nper_account = 1\nmin_free_disk_gb = 0\n\
                    [harness.omp]\nenabled = false\n[profiles.hunt]\nmax_running = 1\n";
        let cfg = crate::config::Config::parse(toml, dir.path()).unwrap();
        (dir, cfg)
    }

    const NOW: u64 = 1_800_000_000 * 1_000_000;

    /// One claude account, read just now, with `jobs`; and the room they are judged in.
    fn claude_room(jobs: &[crate::world::Job]) -> (crate::world::World, crate::systems::dispatch::Room) {
        let mut w = crate::world::World::default();
        let read = crate::world::AccountState { read_at: Some(NOW - 1), ..Default::default() };
        w.accounts.insert("claude:a".into(), read);
        for j in jobs {
            w.jobs_mut().insert(j.name.clone(), j.clone());
        }
        let home = crate::harness::Home { harness: "claude", label: "a".into(), dir: "/x".into() };
        let room = crate::systems::dispatch::Room::of(vec![home], w.accounts.clone(), &w, &[], true);
        (w, room)
    }

    /// Audit finding 15: the board says why a queued job is held as the dispatcher judges it:
    /// every account's cause, not only a profile limit; and a forced job no longer shows the
    /// profile limit a decision let it past.
    #[test]
    fn a_queued_job_shows_what_holds_it_as_dispatch_judges_it() {
        use crate::world::{Job, Status};
        let (_dir, cfg) = one_seat();
        let running = Job {
            name: "r".into(),
            status: Status::Running,
            account: Some("claude:a".into()),
            profile: Some("hunt".into()),
            ..Job::default()
        };
        let waiting = Job { name: "q".into(), status: Status::Queued, ..Job::default() };
        let (w, room) = claude_room(&[running, waiting.clone()]);
        let reason = |j: &Job| super::reason(Some((&cfg, &room)), &w, j, NOW);
        assert_eq!(reason(&waiting).as_deref(), Some("no account has room (claude:a busy (1 of 1 seat))"));
        let hunt = Job { profile: Some("hunt".into()), ..waiting.clone() };
        assert_eq!(reason(&hunt).as_deref(), Some("profile hunt: 1 running (max 1)"));
        let forced = Job { forced: true, ..hunt };
        assert_eq!(
            reason(&forced).as_deref(),
            Some("no account has room (claude:a busy (1 of 1 seat))"),
            "forced past its profile, it waits only for a seat"
        );
        let (w, room) = claude_room(std::slice::from_ref(&waiting));
        assert_eq!(super::reason(Some((&cfg, &room)), &w, &waiting, NOW), None, "nothing holds it");
    }

    /// A job the disk floor holds says so, with what it saw and the free space it waits for; one a
    /// person stopped says only why it stopped.
    #[test]
    fn a_held_job_shows_its_reason_and_the_space_it_waits_for() {
        use crate::world::{Job, Status};
        let (_dir, mut cfg) = one_seat();
        cfg.limits.min_free_disk_gb = 60.0;
        cfg.limits.disk_resume_margin_gb = 3.0;
        let held = Job {
            name: "h".into(),
            status: Status::Stopped,
            held: true,
            reason: Some("disk: 56G free, under 60G".into()),
            ..Job::default()
        };
        let (w, room) = claude_room(std::slice::from_ref(&held));
        assert_eq!(
            super::reason(Some((&cfg, &room)), &w, &held, NOW).as_deref(),
            Some("held: disk: 56G free, under 60G; resumes at 63 GB free")
        );
        assert_eq!(
            super::reason(None, &w, &held, NOW).as_deref(),
            Some("held: disk: 56G free, under 60G; resumes when space is back"),
            "another project's job, without its config"
        );
        // Its prerequisite failed: what needs an operator is said, not the disk it no longer waits for.
        let failed = Job { name: "p".into(), status: Status::Failed, ..Job::default() };
        let child = Job { name: "c".into(), after: vec!["p".into()], ..held.clone() };
        let (w, room) = claude_room(&[failed, child.clone()]);
        assert_eq!(super::reason(Some((&cfg, &room)), &w, &child, NOW).as_deref(), Some("waits for p (failed)"));
        assert_eq!(super::reason(None, &w, &child, NOW).as_deref(), Some("waits for p (failed)"));
        let stopped = Job { held: false, reason: Some("stopped by request".into()), ..held };
        assert_eq!(super::reason(Some((&cfg, &room)), &w, &stopped, NOW).as_deref(), Some("stopped by request"));
    }

    #[test]
    fn the_board_sorts_by_status_then_the_latest_first() {
        use crate::world::{Job, Status};
        let job = |name: &str, status, created_at, ended_at| Job {
            name: name.into(),
            status,
            created_at,
            ended_at,
            ..Job::default()
        };
        let mut jobs = [
            job("old-done", Status::Done, 1, Some(2)),
            job("queued", Status::Queued, 5, None),
            job("new-done", Status::Done, 3, Some(9)),
            job("failed", Status::Failed, 4, Some(6)),
            job("running", Status::Running, 2, None),
            job("stopped", Status::Stopped, 7, Some(8)),
            job("older-queued", Status::Queued, 1, None),
        ];
        jobs.sort_by_key(board_order);
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, ["running", "queued", "older-queued", "failed", "new-done", "old-done", "stopped"]);
    }

    #[test]
    fn the_last_answer_is_found_in_a_stream() {
        let log = "{\"type\":\"system\"}\n{\"type\":\"result\",\"result\":\"first\"}\nnot json\n{\"type\":\"result\",\"result\":\"last\"}\n";
        assert_eq!(answer_in(log).as_deref(), Some("last"));
        assert_eq!(answer_in("{\"type\":\"system\"}"), None);
    }

    #[test]
    fn columns_are_as_wide_as_their_content() {
        let rows = [
            vec!["a".to_owned(), String::new(), "x".to_owned(), "last".to_owned()],
            vec!["long".to_owned(), String::new(), "yy".to_owned(), "z".to_owned()],
        ];
        assert_eq!(table(&rows), ["a     x   last", "long  yy  z"], "the empty column is left out");
    }

    #[test]
    fn the_board_fits_the_window() {
        let text = "short\na line far too long for it\nthird\nfourth";
        assert_eq!(fit(text, Some(10)), "short\na line fa…\nthird\nfourth");
        assert_eq!(fit(text, None), text, "piped: whole lines");
        assert!(fit("·×·×·×", Some(4)).chars().count() == 4, "counted in characters, not bytes");
    }
}
