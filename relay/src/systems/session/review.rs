//! session: a review of a build job's branch. relay brings the branch to the repository the
//! reviewer can read, tells it exactly which commit to judge, reads its verdict, and for a reject
//! sends the findings back to the builder.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{Ran, checkpoint_rev, port};
use crate::bus::{Reason, SessionEvent, Verdict};
use crate::config::model_family;
use crate::ctx::Ctx;
use crate::hostdir::HostDir;
use crate::systems::checkpoint;
use crate::systems::decide::Reviewing;
use crate::world::Job;

/// Fetches the branch a review reads into the repository, as `land fetch` does, and says what the
/// reviewer is to read. Fails the review when the branch cannot be had, or when the reviewed job's
/// profile asks for an independent family and this review runs on the same one. `None` for a job
/// that reviews nothing.
pub(super) fn setup(ctx: &Ctx, job: &Job, model: Option<&str>) -> Result<Option<Reviewing>, String> {
    let Some(of) = job.review_of() else { return Ok(None) };
    let world = ctx.world().map_err(|e| format!("{e:#}"))?;
    let Some(builder) = world.jobs().get(of) else { return Err(format!("the job under review, {of}, is gone")) };
    independent(ctx, builder, model)?;
    let reference = format!("refs/relay/review/{}", job.name);
    fetch(ctx, builder, &reference).map_err(|e| format!("fetching {of}'s branch for review: {e}"))?;
    let repo = &ctx.cfg.repo;
    let commit = checkpoint_rev(repo, &reference).ok_or_else(|| format!("{reference} was not created"))?;
    let base = builder
        .base
        .clone()
        .filter(|b| checkpoint_rev(repo, &format!("{b}^{{commit}}")).is_some())
        .or_else(|| merge_base(repo, &commit));
    Ok(Some(Reviewing { job: of.to_owned(), reference, commit, base }))
}

/// Refuses a review on the family that built `builder` when its profile asks for independence.
fn independent(ctx: &Ctx, builder: &Job, model: Option<&str>) -> Result<(), String> {
    let asks = builder.profile.as_deref().and_then(|p| ctx.cfg.profiles.get(p)).and_then(|p| p.independent.as_deref());
    let (Some(_), Some(model), Some(built)) = (asks, model, ctx.cfg.model_of(builder)) else { return Ok(()) };
    let (a, b) = (model_family(model), model_family(&built));
    if a == b {
        return Err(format!(
            "independent review refused: {} was built on {built} and this review runs on {model}, both the {a} family",
            builder.name
        ));
    }
    Ok(())
}

/// Puts the builder's branch at `reference` in the repository: from its clone, else from where gc
/// or `land fetch` left its work.
fn fetch(ctx: &Ctx, builder: &Job, reference: &str) -> Result<(), String> {
    let repo = &ctx.cfg.repo;
    let live = builder.workdir.as_ref().zip(builder.branch.as_ref()).filter(|(d, _)| d.path().exists());
    if let Some((dir, branch)) = live {
        let out = Ctx::git(repo)
            .args(["fetch", "--quiet"])
            .arg(dir.path())
            .arg(format!("+{branch}:{reference}"))
            .output()
            .map_err(|e| e.to_string())?;
        return if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) };
    }
    let kept = [
        Some(format!("refs/relay/rescue/{}", builder.name)),
        builder.branch.as_ref().map(|b| format!("refs/heads/{b}")),
    ];
    let rev = kept
        .into_iter()
        .flatten()
        .find_map(|r| checkpoint_rev(repo, &r))
        .ok_or_else(|| format!("{}'s branch is in no clone or repository ref", builder.name))?;
    let ok = Ctx::git(repo).args(["update-ref", reference, &rev]).status().is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("git update-ref failed".to_owned()) }
}

fn merge_base(repo: &Path, commit: &str) -> Option<String> {
    let out = Ctx::git(repo).args(["merge-base", "HEAD", commit]).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|s| !s.is_empty())
}

/// The file in a review's inbox that holds its builder's last result.
pub(super) const BUILDER_RESULT: &str = "builder-result.md";

/// The most of a builder's result a review is handed: a report, not a transcript.
const BUILDER_RESULT_MAX: usize = 64 * 1024;

/// Hands the review the builder's last result, written afresh on every start and resume into the review's
/// inbox, which its session reads and cannot write: the builder's own record lives in relay's state, which a
/// session never sees. Returns the file's path, the same inside the sandbox as outside.
pub(super) fn hand_over(ctx: &Ctx, inbox: &HostDir, r: &Reviewing) -> Result<PathBuf> {
    let result = std::fs::read_to_string(ctx.job_file(&r.job, "result.md")).ok();
    let text = builder_result(&r.job, result.as_deref(), &ctx.cfg.session.final_marker);
    Ok(inbox.write(BUILDER_RESULT, text.as_bytes())?)
}

/// What the builder-result file says: the builder's last result (its final section and the summary above it),
/// and plainly when that result has no final section or there is none at all.
fn builder_result(job: &str, result: Option<&str>, marker: &str) -> String {
    let head = format!("# The last result of {job}\n\n");
    let Some(text) = result.map(str::trim).filter(|t| !t.is_empty()) else {
        return format!(
            "{head}{job}'s latest run left no final message, so there is no {marker} section to read. Judge the \
             branch by its commits and its diff alone.\n"
        );
    };
    // Whether there is a final section is read from the whole message, never from what is kept of it.
    let Some(at) = final_section(text, marker) else {
        let text = checkpoint::bounded(text, BUILDER_RESULT_MAX);
        return format!(
            "{head}{job}'s latest run ended without a {marker} section. This is its last message as relay read it; \
             do not take it for a report, and judge the branch by its commits and its diff.\n\n{text}\n"
        );
    };
    // The final section whole, and as much of the summary just above it as there is room for.
    let (summary, section) = text.split_at(at);
    let summary = tail(summary, BUILDER_RESULT_MAX.saturating_sub(section.len()));
    format!(
        "{head}What {job}'s latest run ended with: its {marker} section and what it wrote above it.\n\n{summary}{section}\n"
    )
}

/// Where the final section of `text` starts: its last line that begins with `marker`, else the last place
/// `marker` appears at all (as `Report::choose` finds it). `None` when it appears nowhere.
fn final_section(text: &str, marker: &str) -> Option<usize> {
    if marker.is_empty() {
        return None;
    }
    let mut start = 0;
    let mut found = None;
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        if line.trim_start().starts_with(marker) {
            found = Some(start + indent);
        }
        start += line.len();
    }
    found.or_else(|| text.rfind(marker))
}

/// The end of `text` in at most `max` bytes, saying how much came before it.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let cut = (text.len() - max..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
    format!("… ({cut} earlier bytes left out)\n{}", &text[cut..])
}

/// What a review is told before anything else, on every start and every resume: the commit it is to
/// judge now, which can be newer than the one it saw before, and where the builder's last result is.
pub(super) fn preamble(r: &Reviewing, result: &Path) -> String {
    let base = r.base.as_deref().unwrap_or("HEAD");
    let range = format!("{base}..{}", r.reference);
    let codes: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
    format!(
        "This is a review of job {job}'s branch. relay fetched it into the repository as `{reference}`, at commit \
         {commit}: judge exactly that commit, even if you saw an earlier one. It started from {base}, so \
         `git log {range}` lists its commits and `git diff {range}` is its change. Judge the work as the brief asks.\n\n\
         {job}'s last result, its FINAL section and the summary above it as of this start, is in `{result}` \
         (read-only). It says what the builder claims; check that against the commit.\n\n\
         End your FINAL section with a line `verdict: accept` or `verdict: reject`. For a reject, add a line \
         `reason: <code>` ({codes}) and put the findings the builder must act on in `notes`.",
        job = r.job,
        reference = r.reference,
        commit = r.commit,
        result = result.display(),
        codes = codes.join(", "),
    )
}

/// A review's verdict from its report's final section: a `verdict: accept|reject` line, and for a
/// reject a `reason:` line naming a code (`other` when it names none that exists).
pub(super) fn verdict(answer: &str, marker: &str) -> Option<(Verdict, Option<Reason>)> {
    let tail = if marker.is_empty() { Some(answer) } else { answer.rfind(marker).map(|i| &answer[i..]) }?;
    let field = |name: &str| {
        tail.lines().rev().find_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            l.get(..name.len() + 1)
                .filter(|h| h[..name.len()].eq_ignore_ascii_case(name) && h.ends_with(':'))
                .map(|_| l[name.len() + 1..].trim().to_ascii_lowercase())
        })
    };
    let verdict = match field("verdict")?.split_whitespace().next()? {
        "accept" | "accepted" => Verdict::Accept,
        "reject" | "rejected" => Verdict::Reject,
        _ => return None,
    };
    let reason = (verdict == Verdict::Reject).then(|| {
        field("reason").and_then(|r| Reason::parse(r.split_whitespace().next().unwrap_or(""))).unwrap_or(Reason::Other)
    });
    Some((verdict, reason))
}

/// Records a review's verdict on the reviewed job, and for a reject resumes the builder's session
/// with the findings, within `hooks.max_decisions`. Returns the line for the review's own record.
pub(super) fn carry_out(
    ctx: &Ctx,
    job: &Job,
    run: &Ran<'_>,
    verdict: Verdict,
    reason: Option<Reason>,
    findings: &str,
) -> Result<String> {
    let (Some(of), Some(seen)) = (job.review_of(), run.reviewing) else {
        return Ok(format!("verdict {}, but this job reviews no branch", verdict.as_str()));
    };
    let family = run.model.map(model_family);
    // Who this review is, and where the branch is, now: both can have changed since the review started.
    let world = ctx.world()?;
    let unchained = world.jobs().get(&job.name).and_then(|r| r.unchained.clone());
    let stale_head = world.jobs().get(of).and_then(tip).filter(|t| *t != seen.commit);
    let event = SessionEvent::Reviewed {
        job: job.name.clone(),
        of: of.to_owned(),
        commit: seen.commit.clone(),
        verdict,
        reason,
        family,
        stale_head: stale_head.clone(),
    };
    port(ctx, job).emit(event)?;
    let said = match reason {
        Some(r) => format!("review of {of}: {} ({})", verdict.as_str(), r.as_str()),
        None => format!("review of {of}: {}", verdict.as_str()),
    };
    if let Some(u) = unchained {
        return Ok(format!("{said}; not acted on: this review is no longer {of}'s ({})", u.summary()));
    }
    if let Some(head) = stale_head {
        let short = |c: &str| c.get(..12).unwrap_or(c).to_owned();
        return Ok(format!(
            "{said}; stale-head: {of} moved from {} to {} while this review ran, so {of} is not resumed and \
             the new head is reviewed next",
            short(&seen.commit),
            short(&head)
        ));
    }
    if verdict == Verdict::Accept {
        return Ok(said);
    }
    let message = format!(
        "A review of your branch by {} at {} rejected it ({}). Its findings follow; address them, commit your \
         work, and end with your FINAL section again.\n\n{}",
        job.name,
        seen.commit,
        reason.map_or("other", Reason::as_str),
        checkpoint::bounded(findings.trim(), 4096)
    );
    Ok(match crate::systems::queue::resume_reviewed(ctx, of, &job.name, message)? {
        None => format!("{said}; {of} resumed with the findings"),
        Some(why) => format!("{said}; {of} not resumed: {why}"),
    })
}

/// The commit a build job's branch is at in its clone, when it still has one.
pub(super) fn tip(b: &Job) -> Option<String> {
    let dir = b.workdir.as_ref().filter(|d| d.path().exists())?;
    dir.rev(&format!("refs/heads/{}", b.branch.as_ref()?))
}

/// After a reviewed job's run ended done: resumes the chained reviewer whose latest verdict on it was a reject,
/// was for a head the branch had left, or accepted a commit that is not the tip the run ended at (`Job::head`:
/// a person resumed the builder after the accept), so the board's verdict is for the new tip and what waits on
/// the chain is released by an accept of it. The reviewer's ref is fetched again when it starts, and its
/// preamble names the new commit. Bounded as the builder's own resume is. Returns who was resumed.
pub(super) fn rereview(ctx: &Ctx, builder: &str) -> Result<Vec<String>> {
    resume_rejecting(ctx, builder, None)
}

/// After a review ended: when the builder it rejected (or whose head moved under the review, or whose tip it
/// accepted) ran on and ended before this review was marked ended (so `rereview` found it still running), the
/// builder's tip has moved past the commit the review judged and nobody else will ask for the new one: resume
/// it now.
pub(super) fn rereview_if_overtaken(ctx: &Ctx, reviewer: &str) -> Result<Vec<String>> {
    let world = ctx.world()?;
    let Some(of) = world.jobs().get(reviewer).and_then(|r| r.review_of()) else { return Ok(Vec::new()) };
    resume_rejecting(ctx, of, Some(reviewer))
}

/// The reviewers of `builder` (just `only`, when named) whose last verdict does not stand for what it ended
/// with ([`crate::world::Reviewed::due_again`]), and can be asked again.
fn resume_rejecting(ctx: &Ctx, builder: &str, only: Option<&str>) -> Result<Vec<String>> {
    let world = ctx.world()?;
    let Some(b) = world.jobs().get(builder) else { return Ok(Vec::new()) };
    let rejecting: Vec<&str> = world
        .jobs()
        .values()
        .filter(|r| r.review_of() == Some(builder) && r.unchained.is_none() && only.is_none_or(|o| o == r.name))
        .filter(|r| {
            let last = b.verdicts.iter().rev().find(|v| v.reviewer == r.name);
            // The builder's end and the review's ask by the same test, so neither order of the two loses it.
            last.is_some_and(|v| v.due_again(b))
        })
        .map(|r| r.name.as_str())
        .collect();
    let mut resumed = Vec::new();
    for reviewer in rejecting {
        let last = b.verdicts.iter().rev().find(|v| v.reviewer == reviewer);
        let why = match last {
            Some(v) if v.stale_head.is_some() => {
                format!("{builder}'s branch moved while you were judging it, so that verdict is not for its head.")
            }
            Some(v) if !v.wants_again() => format!(
                "{builder} worked on after you accepted {}, so your accept is not for its tip.",
                v.commit.get(..12).unwrap_or(&v.commit)
            ),
            _ => format!("{builder} has answered your findings and ended its run."),
        };
        let message = format!(
            "{why} Judge its branch again, now at the commit named above, and end with your FINAL section and a verdict."
        );
        if crate::systems::queue::resume_reviewer(ctx, reviewer, builder, message)?.is_none() {
            resumed.push(reviewer.to_owned());
        }
    }
    Ok(resumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project in a scratch directory whose job ceiling refuses every resume a review asks for, so a re-review
    /// that is due shows as the chain's halt and starts nothing.
    fn project(dir: &crate::config::tempdir::Dir) -> Ctx {
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let cfg = crate::config::Config::parse("name = \"demo\"\n[limits]\njob_ceiling_usd = 0.5\n", root).unwrap();
        let ctx = Ctx::unclaimed(cfg, root.join("relay.toml"), root.join("state"), root.join("data"));
        std::fs::create_dir_all(ctx.bus.join("session")).unwrap();
        std::fs::create_dir_all(&ctx.jobs).unwrap();
        ctx
    }

    fn put(ctx: &Ctx, file: &str, t: u64, event: serde_json::Value) {
        use std::io::Write as _;
        let mut v = event;
        v["t"] = t.into();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(ctx.bus.join(file)).unwrap();
        writeln!(f, "{v}").unwrap();
    }

    fn json(e: impl serde::Serialize) -> serde_json::Value {
        serde_json::to_value(e).unwrap()
    }

    /// The bus up to the builder ending on a tip past the one `rev` accepted, while `rev` has not ended yet.
    fn accepted_then_overtaken(ctx: &Ctx) {
        use crate::bus::{DispatchEvent, Outcome, QueueEvent};
        let dispatched = |job: &str| {
            json(DispatchEvent::Dispatched {
                job: job.into(),
                account: "c:w".into(),
                model: None,
                provider: String::new(),
                runner_pid: 7,
                runner_started: 1,
            })
        };
        let ended = |job: &str, head: &str| {
            let head = Some(head.to_owned()).filter(|h| !h.is_empty());
            json(SessionEvent::Ended {
                job: job.into(),
                outcome: Outcome::Done,
                cost_usd: 0.0,
                turns: 1,
                reason: None,
                head,
            })
        };
        let queued = r#"{"type":"queued","job":"build","task":"x","readonly":false,"effort":null,"budget_usd":1.0,"pinned":null}"#;
        put(ctx, "queue.ndjson", 1, serde_json::from_str(queued).unwrap());
        put(ctx, "dispatch.ndjson", 2, dispatched("build"));
        put(ctx, "session/build.1.1.ndjson", 3, ended("build", "c1"));
        let review = r#"{"type":"queued","job":"rev","task":"x","readonly":true,"effort":null,"budget_usd":1.0,"pinned":null,"after":["build"],"acceptance":{"review-of":"build"}}"#;
        put(ctx, "queue.ndjson", 4, serde_json::from_str(review).unwrap());
        put(ctx, "dispatch.ndjson", 5, dispatched("rev"));
        let accept = SessionEvent::Reviewed {
            job: "rev".into(),
            of: "build".into(),
            commit: "c1".into(),
            verdict: Verdict::Accept,
            reason: None,
            family: None,
            stale_head: None,
        };
        put(ctx, "session/rev.4.1.ndjson", 6, json(accept));
        let resumed = QueueEvent::Resumed {
            job: "build".into(),
            message: "more".into(),
            budget_usd: None,
            models: std::collections::BTreeMap::new(),
        };
        put(ctx, "queue.ndjson", 7, json(resumed));
        put(ctx, "dispatch.ndjson", 8, dispatched("build"));
        put(ctx, "session/build.1.2.ndjson", 9, ended("build", "c2"));
    }

    /// Whether the chain asked `rev` to judge again: the project's ceiling refuses it, which the bus records.
    fn asked_again(ctx: &Ctx) -> bool {
        ctx.world().unwrap().jobs()["build"].review_halted.as_deref().is_some_and(|h| h.contains("ceiling"))
    }

    /// Copilot on PR #69: an accept the builder went past is judged again whichever end comes last, the builder's
    /// (it resumes the review) or the review's (it was still running when the builder ended, so it asks itself).
    #[test]
    fn an_accept_the_builder_went_past_is_judged_again_in_either_order() {
        let ended_rev = |ctx: &Ctx| {
            let e = SessionEvent::Ended {
                job: "rev".into(),
                outcome: crate::bus::Outcome::Done,
                cost_usd: 0.0,
                turns: 1,
                reason: None,
                head: None,
            };
            put(ctx, "session/rev.4.1.ndjson", 10, json(e));
        };
        // The review ends first; the builder's end asks.
        let dir = crate::config::tempdir::Dir::new();
        let ctx = project(&dir);
        accepted_then_overtaken(&ctx);
        ended_rev(&ctx);
        rereview(&ctx, "build").unwrap();
        assert!(asked_again(&ctx), "the builder's end: {:?}", ctx.world().unwrap().jobs()["build"]);

        // The builder ends first, while the review still runs: its end cannot resume it; the review's end asks.
        let dir = crate::config::tempdir::Dir::new();
        let ctx = project(&dir);
        accepted_then_overtaken(&ctx);
        rereview(&ctx, "build").unwrap();
        assert!(!asked_again(&ctx), "a running review is not resumed");
        ended_rev(&ctx);
        rereview_if_overtaken(&ctx, "rev").unwrap();
        assert!(asked_again(&ctx), "the review's end: {:?}", ctx.world().unwrap().jobs()["build"]);
    }

    /// Copilot on PR #69 (review.rs:306): a rejected builder that answers without a commit, and ends before the
    /// review it answered is marked ended, ends on the tip that was rejected. It answered all the same: the
    /// review's end asks for the re-review, or what waits on the chain stays at "re-review due" for ever.
    #[test]
    fn a_reject_answered_on_the_same_tip_before_the_review_ended_is_judged_again() {
        use crate::bus::{DispatchEvent, Outcome, QueueEvent};
        let dir = crate::config::tempdir::Dir::new();
        let ctx = project(&dir);
        let dispatched = |job: &str| {
            json(DispatchEvent::Dispatched {
                job: job.into(),
                account: "c:w".into(),
                model: None,
                provider: String::new(),
                runner_pid: 7,
                runner_started: 1,
            })
        };
        let ended = |job: &str, head: Option<&str>| {
            let head = head.map(str::to_owned);
            json(SessionEvent::Ended {
                job: job.into(),
                outcome: Outcome::Done,
                cost_usd: 0.0,
                turns: 1,
                reason: None,
                head,
            })
        };
        let queued = r#"{"type":"queued","job":"build","task":"x","readonly":false,"effort":null,"budget_usd":1.0,"pinned":null}"#;
        put(&ctx, "queue.ndjson", 1, serde_json::from_str(queued).unwrap());
        put(&ctx, "dispatch.ndjson", 2, dispatched("build"));
        put(&ctx, "session/build.1.1.ndjson", 3, ended("build", Some("c1")));
        let review = r#"{"type":"queued","job":"rev","task":"x","readonly":true,"effort":null,"budget_usd":1.0,"pinned":null,"after":["build"],"acceptance":{"review-of":"build"}}"#;
        put(&ctx, "queue.ndjson", 4, serde_json::from_str(review).unwrap());
        put(&ctx, "dispatch.ndjson", 5, dispatched("rev"));
        let reject = SessionEvent::Reviewed {
            job: "rev".into(),
            of: "build".into(),
            commit: "c1".into(),
            verdict: Verdict::Reject,
            reason: Some(Reason::TestFailure),
            family: None,
            stale_head: None,
        };
        put(&ctx, "session/rev.4.1.ndjson", 6, json(reject));
        let resumed = QueueEvent::Resumed {
            job: "build".into(),
            message: "findings".into(),
            budget_usd: None,
            models: std::collections::BTreeMap::new(),
        };
        put(&ctx, "queue.ndjson", 7, json(resumed));
        put(&ctx, "dispatch.ndjson", 8, dispatched("build"));
        // It answered with no commit: the same tip.
        put(&ctx, "session/build.1.2.ndjson", 9, ended("build", Some("c1")));
        rereview(&ctx, "build").unwrap();
        assert!(!asked_again(&ctx), "the review is still running: its own end asks");
        put(&ctx, "session/rev.4.1.ndjson", 10, ended("rev", None));
        rereview_if_overtaken(&ctx, "rev").unwrap();
        assert!(asked_again(&ctx), "the review's end: {:?}", ctx.world().unwrap().jobs()["build"]);
    }

    #[test]
    fn a_verdict_is_read_from_the_final_section_only() {
        assert_eq!(verdict("FINAL\nverdict: Accept", "FINAL"), Some((Verdict::Accept, None)));
        assert_eq!(
            verdict("FINAL\n- verdict: reject\n- reason: vibes", "FINAL"),
            Some((Verdict::Reject, Some(Reason::Other))),
            "a reject whose reason is no code is `other`"
        );
        assert_eq!(verdict("verdict: accept\nFINAL\nstatus: done", "FINAL"), None);
        assert_eq!(verdict("FINAL\nverdict: maybe", "FINAL"), None);
    }

    #[test]
    fn the_preamble_names_the_commit_and_the_range() {
        let r = Reviewing {
            job: "b".into(),
            reference: "refs/relay/review/r".into(),
            commit: "abc".into(),
            base: Some("def".into()),
        };
        let p = preamble(&r, Path::new("/in/builder-result.md"));
        assert!(
            p.contains("at commit abc")
                && p.contains("git diff def..refs/relay/review/r")
                && p.contains("exactly that commit")
                && p.contains("b's last result, its FINAL section and the summary above it as of this start, is in `/in/builder-result.md`"),
            "{p}"
        );
    }

    #[test]
    fn the_builders_result_is_handed_over_as_it_is_and_its_absence_is_said() {
        let with = builder_result("b", Some("Did the parser.\n\nFINAL\nstatus: done\n"), "FINAL");
        assert!(
            with.contains("its FINAL section and what it wrote above it")
                && with.contains("Did the parser.\n\nFINAL\nstatus: done"),
            "{with}"
        );
        let without = builder_result("b", Some("I ran out of turns."), "FINAL");
        assert!(
            without.contains("ended without a FINAL section") && without.contains("I ran out of turns."),
            "{without}"
        );
        for none in [None, Some("  \n")] {
            let none = builder_result("b", none, "FINAL");
            assert!(none.contains("left no final message, so there is no FINAL section"), "{none}");
        }
        // Copilot on PR #69: a summary past the bound must not take the final section with it.
        let section = "FINAL\nstatus: done\nnotes: all of it";
        let summary = "é".repeat(BUILDER_RESULT_MAX);
        let big = builder_result("b", Some(&format!("{summary}\n{section}")), "FINAL");
        assert!(
            big.contains("its FINAL section and what it wrote above it") && big.ends_with(&format!("{section}\n")),
            "kept"
        );
        assert!(
            big.len() < BUILDER_RESULT_MAX + 512 && big.contains("earlier bytes left out)\néé"),
            "the summary's end"
        );
        let whole = format!("FINAL\n{}", "y".repeat(BUILDER_RESULT_MAX + 10));
        let huge = builder_result("b", Some(&format!("summary\n{whole}")), "FINAL");
        assert!(
            huge.ends_with(&format!("{whole}\n")) && !huge.contains("summary\n"),
            "a final section past the bound is whole"
        );
        let long = builder_result("b", Some(&"x".repeat(BUILDER_RESULT_MAX + 10)), "FINAL");
        assert!(long.len() < BUILDER_RESULT_MAX + 512 && long.contains("(10 more bytes)"), "bounded");
    }
}
