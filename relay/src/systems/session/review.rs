//! session: a review of a build job's branch. relay brings the branch to the repository the
//! reviewer can read, tells it exactly which commit to judge, reads its verdict, and for a reject
//! sends the findings back to the builder.

use std::path::Path;

use anyhow::Result;

use super::{Ran, checkpoint_rev, port};
use crate::bus::{Reason, SessionEvent, Verdict};
use crate::config::model_family;
use crate::ctx::Ctx;
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

/// What a review is told before anything else, on every start and every resume: the commit it is to
/// judge now, which can be newer than the one it saw before.
pub(super) fn preamble(r: &Reviewing) -> String {
    let base = r.base.as_deref().unwrap_or("HEAD");
    let range = format!("{base}..{}", r.reference);
    let codes: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
    format!(
        "This is a review of job {job}'s branch. relay fetched it into the repository as `{reference}`, at commit \
         {commit}: judge exactly that commit, even if you saw an earlier one. It started from {base}, so \
         `git log {range}` lists its commits and `git diff {range}` is its change. Judge the work as the brief asks.\n\n\
         End your FINAL section with a line `verdict: accept` or `verdict: reject`. For a reject, add a line \
         `reason: <code>` ({codes}) and put the findings the builder must act on in `notes`.",
        job = r.job,
        reference = r.reference,
        commit = r.commit,
        codes = codes.join(", "),
    )
}

/// A report's final section: from its last `marker` on, or the whole answer when there is no marker.
/// None: the report has no such section.
pub(super) fn final_section<'a>(answer: &'a str, marker: &str) -> Option<&'a str> {
    if marker.is_empty() { Some(answer) } else { answer.rfind(marker).map(|i| &answer[i..]) }
}

/// A field of a report's final section, `name: value` (a list bullet allowed before it), the last one
/// given, its value trimmed and lowercased. The one parser of a report: a review's verdict and a run's
/// `status:` are both read by it. A line is split at its first `:`, never cut at a byte count, so no
/// text in a report (a multibyte character where the name would end) can make it panic.
pub(super) fn final_field(section: &str, name: &str) -> Option<String> {
    section.lines().rev().find_map(|l| {
        let (key, value) = l.trim().trim_start_matches(['-', '*', ' ']).split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim().to_ascii_lowercase())
    })
}

/// A review's verdict from its report's final section: a `verdict: accept|reject` line, and for a
/// reject a `reason:` line naming a code (`other` when it names none that exists).
pub(super) fn verdict(answer: &str, marker: &str) -> Option<(Verdict, Option<Reason>)> {
    let tail = final_section(answer, marker)?;
    let field = |name: &str| final_field(tail, name);
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
    Ok(match crate::systems::queue::resume_reviewed(ctx, of, message)? {
        None => format!("{said}; {of} resumed with the findings"),
        Some(why) => format!("{said}; {of} not resumed: {why}"),
    })
}

/// The commit a build job's branch is at in its clone, when it still has one.
fn tip(b: &Job) -> Option<String> {
    let dir = b.workdir.as_ref().filter(|d| d.path().exists())?;
    dir.rev(&format!("refs/heads/{}", b.branch.as_ref()?))
}

/// After a reviewed job's run ended done: resumes the chained reviewer whose latest verdict on it was a reject
/// or was for a head the branch had left, so the board's verdict is for the new tip. The reviewer's ref is fetched again when it starts, and its
/// preamble names the new commit. Bounded as the builder's own resume is. Returns who was resumed.
pub(super) fn rereview(ctx: &Ctx, builder: &str) -> Result<Vec<String>> {
    resume_rejecting(ctx, builder, None)
}

/// After a review ended: when the builder it rejected (or whose head moved under the review) answered and ended
/// before this review was marked ended (so `rereview` found it still running), the builder's tip has moved past
/// the commit the review judged and nobody else will ask for the new one: resume it now.
pub(super) fn rereview_if_overtaken(ctx: &Ctx, reviewer: &str) -> Result<Vec<String>> {
    let world = ctx.world()?;
    let Some(of) = world.jobs().get(reviewer).and_then(|r| r.review_of()) else { return Ok(Vec::new()) };
    resume_rejecting(ctx, of, Some(reviewer))
}

/// The reviewers of `builder` (just `only`, when named) that rejected it last and can be asked again.
/// With `only`, the builder must be done and past the commit judged.
fn resume_rejecting(ctx: &Ctx, builder: &str, only: Option<&str>) -> Result<Vec<String>> {
    let world = ctx.world()?;
    let Some(b) = world.jobs().get(builder) else { return Ok(Vec::new()) };
    let rejecting: Vec<&str> = world
        .jobs()
        .values()
        .filter(|r| r.review_of() == Some(builder) && r.unchained.is_none() && only.is_none_or(|o| o == r.name))
        .filter(|r| {
            let last = b.verdicts.iter().rev().find(|v| v.reviewer == r.name);
            last.is_some_and(|v| {
                v.wants_again()
                    && (only.is_none()
                        || (b.status == crate::world::Status::Done && tip(b).is_some_and(|t| t != v.commit)))
            })
        })
        .map(|r| r.name.as_str())
        .collect();
    let mut resumed = Vec::new();
    for reviewer in rejecting {
        let stale = b.verdicts.iter().rev().find(|v| v.reviewer == reviewer).is_some_and(|v| v.stale_head.is_some());
        let why = if stale {
            format!("{builder}'s branch moved while you were judging it, so that verdict is not for its head.")
        } else {
            format!("{builder} has answered your findings and ended its run.")
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
        let p = preamble(&r);
        assert!(
            p.contains("at commit abc")
                && p.contains("git diff def..refs/relay/review/r")
                && p.contains("exactly that commit"),
            "{p}"
        );
    }
}
