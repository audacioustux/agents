//! view: read-only. Who is working on what, and what each session said.

use anyhow::Result;
use clap::Subcommand;

use crate::bus::now_micros;
use crate::ctx::Ctx;

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
    Result { name: String },
    /// The merged event log, oldest first, one JSON object per line.
    Events,
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Status { all } => status(ctx, all),
        Cmd::Watch { all } => crate::systems::watch::run(ctx, all),
        Cmd::Result { name } => result(ctx, &name),
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
        out!("{}", serde_json::to_string_pretty(&world)?);
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

/// A job's cells: name, status, account, since when, spend, and its task with any reason.
pub fn job_cells(j: &crate::world::Job, now: u64) -> Vec<String> {
    let since = match j.status {
        crate::world::Status::Running => j.started_at,
        crate::world::Status::Queued => j.ended_at.or(Some(j.created_at)),
        _ => j.ended_at,
    };
    let reason = j.reason.as_deref().map_or(String::new(), |r| format!("  [{r}]"));
    vec![
        j.name.clone(),
        j.status.as_str().to_owned(),
        j.account.as_deref().or(j.pinned.as_deref()).unwrap_or("-").to_owned(),
        since.map_or_else(|| "-".into(), |t| ago(now, t)),
        if j.cost_usd > 0.0 { format!("${:.2}", j.cost_usd) } else { String::new() },
        format!("{}{reason}", j.task),
    ]
}

/// The jobs and accounts, as text.
fn board(ctx: &Ctx, world: &crate::world::World, all: bool) -> String {
    let now = now_micros();
    let jobs = listed(world, all);
    let rows: Vec<Vec<String>> = jobs.iter().map(|j| job_cells(j, now)).collect();
    let mut lines = vec![format!("jobs (at most {} running)", ctx.cfg.limits.max_running)];
    lines.extend(table(&rows).into_iter().map(|r| format!("  {r}")));
    if jobs.is_empty() {
        lines.push("  (none)".into());
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

fn result(ctx: &Ctx, name: &str) -> Result<()> {
    let world = ctx.world()?;
    let Some(j) = world.jobs.get(name) else { fail!(NotFound, "no job named {name}") };
    let answer = std::fs::read_to_string(ctx.job_file(name, "result.md")).ok();
    let log = ctx.job_file(name, "log.ndjson");
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
    if let (Some(b), Some(w)) = (&j.branch, &j.workdir) {
        out!("branch {b} in {}   bring it here: relay land fetch {name}", w.display());
    }
    out!("log {}\n", log.display());
    out!("{}", answer.as_deref().map_or("(no result yet)", str::trim_end));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{fit, table};

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
