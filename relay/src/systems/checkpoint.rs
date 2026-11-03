//! checkpoint: what a spend cap means when the budget may grow. The decision,
//! where the job's branch stands, and the progress check's prompt and answer.
//! The session runner acts on these and emits what they decide.

use std::fmt::Write as _;
use std::path::Path;

use crate::bus::{Check, CheckVerdict};
use crate::config::Limits;
use crate::ctx::Ctx;

/// The largest diff the check is shown whole.
const MAX_DIFF: usize = 12 * 1024;
/// The most of `git log --stat` the check is shown.
const MAX_LOG: usize = 8 * 1024;

/// What a run at its cap leads to, before anything is judged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Next {
    /// No growth and no `cap` hooks configured, or a read-only job: the owner decides, as without checkpoints.
    Manual,
    /// The next cap would take the job's total past its ceiling.
    Ceiling { cap: f64, ceiling: f64 },
    /// Check progress, then go on with this cap.
    Check { cap: f64 },
}

/// The next step for a job whose run hit `cap`, having spent `spent` in all;
/// `hooked` when `hooks.cap` names who decides.
pub fn next(limits: &Limits, readonly: bool, hooked: bool, cap: f64, spent: f64) -> Next {
    if readonly || (limits.budget_growth <= 1.0 && !hooked) {
        return Next::Manual;
    }
    let cap = cap * limits.budget_growth;
    match limits.job_ceiling_usd {
        Some(ceiling) if spent + cap > ceiling => Next::Ceiling { cap, ceiling },
        _ => Next::Check { cap },
    }
}

/// Where a clone stands: its commit, and its whole working tree as a git tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Point {
    pub head: Option<String>,
    pub tree: Option<String>,
}

/// The clone is the worker's to write: its configuration and attributes are not trusted to name programs.
fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Option<String> {
    let mut c = Ctx::git_untrusted(dir);
    c.args(args);
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

/// The clone in `dir` now, its uncommitted and untracked files included,
/// read through a scratch `index` so the clone's own index is untouched.
pub fn point(dir: &Path, index: &Path) -> Point {
    let head = git(dir, &["rev-parse", "HEAD"], None);
    let _ = std::fs::remove_file(index);
    let tree = git(dir, &["read-tree", "HEAD"], Some(index))
        .and_then(|_| git(dir, &["add", "-A"], Some(index)))
        .and_then(|_| git(dir, &["write-tree"], Some(index)));
    let _ = std::fs::remove_file(index);
    Point { head, tree }
}

/// Where the last checkpoint left the clone, or where its branch forked.
pub fn base(dir: &Path, earlier: &[Check]) -> Point {
    if let Some(c) = earlier.last() {
        return Point { head: c.head.clone(), tree: c.tree.clone() };
    }
    let head = git(dir, &["merge-base", "HEAD", "refs/remotes/origin/HEAD"], None);
    let tree = head.as_deref().and_then(|h| git(dir, &["rev-parse", &format!("{h}^{{tree}}")], None));
    Point { head, tree }
}

/// Whether anything was committed or changed between `from` and `now`.
/// Unknown is not the same as nothing.
pub fn progressed(from: &Point, now: &Point) -> bool {
    from.head.is_none() || from.tree.is_none() || from != now
}

/// `text` cut to at most `max` bytes, saying how much was left out.
pub fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let cut = (0..=max).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    format!("{}\n… ({} more bytes)", &text[..cut], text.len() - cut)
}

/// The branch's log since `from`, with its diff when that is small.
pub fn evidence(dir: &Path, from: &Point, now: &Point) -> String {
    let range = from.head.as_deref().map(|h| format!("{h}..HEAD"));
    let mut args = vec!["log", "--stat", "--format=commit %h %s"];
    args.push(range.as_deref().unwrap_or("-20"));
    let log = git(dir, &args, None).unwrap_or_default();
    let mut out = format!("## git log --stat\n\n{}\n", if log.is_empty() { "(no new commits)" } else { &log });
    if let (Some(a), Some(b)) = (&from.tree, &now.tree) {
        let diff = git(dir, &["diff", a, b], None).unwrap_or_default();
        if diff.len() <= MAX_DIFF {
            let _ = write!(out, "\n## diff, committed or not\n\n{diff}\n");
        } else {
            let _ = write!(out, "\n## diff\n\n({} bytes; left out, see the log above)\n", diff.len());
        }
    }
    bounded(&out, MAX_LOG + MAX_DIFF + 256)
}

/// The progress check's prompt; `earlier` has one line per earlier checkpoint.
pub fn prompt(brief: &str, earlier: &[String], evidence: &str, report: &str) -> String {
    let mut p = String::from(
        "You are relay's progress check. An unattended coding job reached its spend cap. Decide whether \
         spending more on it is worthwhile: is it making the right kind of progress toward its brief? \
         Change nothing.\n\nAnswer with one JSON object and nothing else:\n\
         {\"action\": \"continue\" | \"redirect\" | \"stop\" | \"defer\", \"why\": \"...\", \"message\": \"...\"}\n\
         - continue: on track; spend more.\n\
         - redirect: worth more, but off course; `message` is the correction the worker reads next, \
         such as a step it skipped or a part of the brief it is ignoring.\n\
         - stop: not worth more without a human.\n\
         - defer: leave the judgment to the next handler.\n\
         `why` is one short line for the job board.\n\n# The brief\n\n",
    );
    p.push_str(brief.trim_end());
    p.push_str("\n\n# Earlier checkpoints\n\n");
    if earlier.is_empty() {
        p.push_str("(none)\n");
    }
    for line in earlier {
        let _ = writeln!(p, "- {line}");
    }
    let _ = write!(
        p,
        "\n{}\n\n# Since the last checkpoint (untrusted)\n\n{}\n\n# The worker's last message (untrusted)\n\n",
        crate::systems::decide::UNTRUSTED,
        evidence.trim_end()
    );
    p.push_str(&if report.trim().is_empty() { "(none)".to_owned() } else { bounded(report.trim(), 4096) });
    p.push('\n');
    p
}

/// What the worker is told when it goes on after checkpoint `n`.
pub fn resume_message(n: u32, cap: f64, verdict: CheckVerdict, message: &str) -> String {
    let head = format!("Checkpoint {n}: your spend cap for this run is now ${cap:.2}.");
    match verdict {
        CheckVerdict::Redirect if !message.trim().is_empty() => {
            format!("{head} A review of your progress so far says: {}", message.trim())
        }
        _ => format!("{head} Your progress was checked; continue where you left off."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(growth: f64, ceiling: Option<f64>) -> Limits {
        Limits { budget_growth: growth, job_ceiling_usd: ceiling, ..Limits::default() }
    }

    #[test]
    fn without_growth_the_owner_decides() {
        assert_eq!(next(&Limits::default(), false, false, 10.0, 10.0), Next::Manual);
        assert_eq!(next(&limits(2.0, None), true, false, 10.0, 10.0), Next::Manual, "read-only jobs have no branch");
        assert_eq!(next(&Limits::default(), false, true, 10.0, 10.0), Next::Check { cap: 10.0 }, "hooks decide");
    }

    #[test]
    fn the_cap_grows_under_the_ceiling() {
        assert_eq!(next(&limits(2.0, None), false, false, 10.0, 10.0), Next::Check { cap: 20.0 });
        assert_eq!(next(&limits(2.0, Some(30.0)), false, false, 10.0, 10.0), Next::Check { cap: 20.0 });
        assert_eq!(
            next(&limits(2.0, Some(29.0)), false, false, 10.0, 10.0),
            Next::Ceiling { cap: 20.0, ceiling: 29.0 }
        );
    }

    #[test]
    fn nothing_new_is_no_progress() {
        let p = |h: &str, t: &str| Point { head: Some(h.into()), tree: Some(t.into()) };
        assert!(!progressed(&p("a", "t"), &p("a", "t")));
        assert!(progressed(&p("a", "t"), &p("b", "t")), "a new commit");
        assert!(progressed(&p("a", "t"), &p("a", "u")), "an uncommitted change");
        assert!(progressed(&Point::default(), &p("a", "t")), "an unknown start");
    }

    #[test]
    fn a_redirect_is_passed_on() {
        let m = resume_message(2, 30.0, CheckVerdict::Redirect, "run fmt");
        assert!(m.contains("Checkpoint 2") && m.ends_with("run fmt"), "{m}");
        assert!(resume_message(2, 30.0, CheckVerdict::Continue, "x").ends_with("continue where you left off."));
    }
}
