//! view: read-only. Who is working on what, and what each session said.

mod stats;

use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;

use crate::bus::now_micros;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::world::{Job, Wait, World};

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Jobs and accounts. With `--json`, the whole folded state.
    Status {
        /// Include jobs that ended more than a day ago.
        #[arg(long)]
        all: bool,
    },
    /// Live: the status, redrawn as events land, with each running session's
    /// latest output. Ctrl-C ends it.
    Watch {
        #[arg(long)]
        all: bool,
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
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Status { all } => status(ctx, all),
        Cmd::Watch { all } => crate::systems::watch::run(ctx, all),
        Cmd::Result { name, wait } => result(ctx, &name, wait),
        Cmd::Stats { since } => stats::run(ctx, since.as_deref()),
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

fn status(ctx: &Ctx, all: bool) -> Result<()> {
    let world = ctx.world()?;
    if ctx.json {
        let mut v = serde_json::to_value(&world)?;
        for j in world.jobs.values() {
            v["jobs"][&j.name]["pending"] = serde_json::to_value(world.waits(j))?;
        }
        out!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let text = board(ctx, &world, all);
    // On a terminal, one screen line per line; piped, every line whole.
    let width = crate::os::terminal_size().map(|(cols, _)| cols);
    out!("{}", fit(&text, width));
    Ok(())
}

/// Redraws the board whenever the bus changes, and every few seconds for the clocks.
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
fn fit(text: &str, width: Option<usize>) -> String {
    let cut = |line: &str| match width {
        Some(w) if line.chars().count() > w => {
            line.chars().take(w.saturating_sub(1)).chain(std::iter::once('…')).collect::<String>()
        }
        _ => line.to_owned(),
    };
    text.lines().map(cut).collect::<Vec<_>>().join("\n")
}

/// The jobs worth showing, oldest first: live ones, those ended within a day, or all.
pub fn listed(world: &crate::world::World, all: bool) -> Vec<&crate::world::Job> {
    let day_ago = now_micros().saturating_sub(24 * 3600 * 1_000_000);
    let mut jobs: Vec<_> =
        world.jobs.values().filter(|j| all || j.status.is_live() || j.ended_at.is_some_and(|t| t > day_ago)).collect();
    jobs.sort_by_key(|j| j.created_at);
    jobs
}

/// What a job is asked to run as: its profile, else the model it names, then its effort.
pub fn kind(j: &Job) -> String {
    let what = j.profile.clone().unwrap_or_else(|| j.models.values().cloned().collect::<Vec<_>>().join(","));
    match j.effort.as_deref() {
        Some(effort) => format!("{what} {effort}"),
        None => what,
    }
}

/// Why a queued job's profile holds it back now, by `max_running` or `max_per_day`.
fn profile_hold(cfg: &Config, world: &World, j: &Job, now: u64) -> Option<String> {
    let name = j.profile.as_deref()?;
    let p = cfg.profiles.get(name)?;
    let running = world.jobs.values().filter(|o| o.status.has_runner() && o.profile.as_deref() == Some(name)).count();
    let starts = world.profile_starts.get(name).map_or(&[][..], Vec::as_slice);
    crate::systems::dispatch::profile_hold(name, p, running, starts, now).map(|(why, _)| why)
}

/// A job's cells: name, status, account, profile (or model) with effort, since when, spend, and its
/// task with any reason (a queued job also says what it waits for, or which profile limit holds it).
pub fn job_cells(cfg: &Config, world: &World, j: &Job, now: u64) -> Vec<String> {
    let since = match j.status {
        crate::world::Status::Running | crate::world::Status::Checking => j.started_at,
        crate::world::Status::Queued => j.ended_at.or(Some(j.created_at)),
        _ => j.ended_at,
    };
    let queued = j.status == crate::world::Status::Queued;
    let waits = if queued { Wait::summary(&world.waits(j)) } else { None };
    let held = if queued && waits.is_none() { profile_hold(cfg, world, j, now) } else { None };
    let reason = waits.or(held).or_else(|| j.reason.clone()).map_or(String::new(), |r| format!("  [{r}]"));
    let check = j.checkpoints.last().map_or(String::new(), |c| format!("  {}", c.summary()));
    let check = format!("{check}{}", declared(j));
    vec![
        j.name.clone(),
        j.status.as_str().to_owned(),
        j.account.as_deref().or(j.pinned.as_deref()).unwrap_or("-").to_owned(),
        kind(j),
        since.map_or_else(|| "-".into(), |t| ago(now, t)),
        if j.cost_usd > 0.0 { format!("${:.2}", j.cost_usd) } else { String::new() },
        j.disk_bytes.filter(|_| j.status.has_runner()).map_or_else(String::new, crate::os::size_str),
        format!("{}{reason}{check}{}", j.task, collected(j)),
    ]
}

/// The latest acceptance check and the latest review of a job, for its board row.
fn declared(j: &Job) -> String {
    let bar = j.bars.last().map_or(String::new(), |b| format!("  check {}", b.result.as_str()));
    let review = j.verdicts.last().map_or(String::new(), |v| format!("  review by {}: {}", v.reviewer, v.summary()));
    let of = j.review_of.as_deref().map_or(String::new(), |b| format!("  reviews {b}"));
    format!("{of}{bar}{review}")
}

/// What gc took from a job's clone, for its board row.
fn collected(j: &Job) -> &'static str {
    match j.collected.as_deref() {
        Some("clone") => "  (clone removed)",
        Some(_) => "  (build outputs removed)",
        None => "",
    }
}

/// The jobs and accounts, as text.
fn board(ctx: &Ctx, world: &crate::world::World, all: bool) -> String {
    let now = now_micros();
    let jobs = listed(world, all);
    let rows: Vec<Vec<String>> = jobs.iter().map(|j| job_cells(&ctx.cfg, world, j, now)).collect();
    let mut lines = vec![format!("jobs (at most {} running)", ctx.cfg.limits.max_running)];
    lines.extend(table(&rows).into_iter().map(|r| format!("  {r}")));
    if jobs.is_empty() {
        lines.push("  (none)".into());
    }
    let waiting = crate::systems::decide::pending(world, crate::bus::now_micros());
    if !waiting.is_empty() {
        lines.push(String::new());
        lines.push("decisions waiting (relay decide answer ID --action ...)".into());
        let rows: Vec<Vec<String>> = waiting
            .iter()
            .map(|d| {
                let until = format!("until {}", crate::systems::decide::clock(d.parked_until.unwrap_or(0)));
                vec![d.id.clone(), d.job.clone(), d.point.as_str().to_owned(), until]
            })
            .collect();
        lines.extend(table(&rows).into_iter().map(|r| format!("  {r}")));
    }
    lines.push(String::new());
    lines.push(crate::systems::quota::accounts_view(ctx, world).text());
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
    if ctx.json {
        let mut v = serde_json::to_value(j)?;
        v["answer"] = answer.as_deref().map(str::trim_end).into();
        v["log"] = log.display().to_string().into();
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
    if let Some(c) = j.checkpoints.last() {
        out!("{}", c.summary());
        if c.next_cap_usd.is_some() && !c.why.is_empty() {
            out!("  why: {}", c.why);
        }
    }
    if let Some(b) = j.bars.last() {
        out!(
            "check {}: exit {}{}{}",
            b.result.as_str(),
            b.exit,
            b.base_exit.map_or(String::new(), |e| format!(", exit {e} at the base commit")),
            b.commit.as_deref().map_or(String::new(), |c| format!(", at {}", c.get(..12).unwrap_or(c)))
        );
    }
    for v in &j.verdicts {
        out!("review by {} at {}: {}", v.reviewer, v.commit.get(..12).unwrap_or(&v.commit), v.summary());
    }
    if let (Some(b), Some(w)) = (&j.branch, &j.workdir) {
        out!("branch {b} in {}   bring it here: relay land fetch {name}", w.display());
    }
    out!("log {}\n", log.display());
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
    let Some(j) = world.jobs.get(name) else { fail!(NotFound, "no job named {name}") };
    Ok(j.clone())
}

/// The job once it has ended. Sleeps on the doorbell between reads, and wakes
/// the dispatcher whenever the bus has been quiet, so the wait cannot outlive it.
fn ended(ctx: &Ctx, name: &str) -> Result<Job> {
    let bell = ctx.doorbell()?;
    let mut quiet = true;
    loop {
        let j = job(&ctx.world()?, name)?;
        if !j.status.is_live() {
            return Ok(j);
        }
        if quiet {
            ctx.wake_dispatch()?;
        }
        quiet = !bell.wait(Duration::from_mins(5));
    }
}

#[cfg(test)]
mod tests {
    use super::{answer_in, fit, table};

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
