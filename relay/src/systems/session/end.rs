//! session: what happens when a run ends done or failed. The acceptance check runs, then the `ended`
//! chain decides: accept, resume, requeue, follow up, give a review's verdict, or stop. A run whose
//! acceptance check did not pass is never done, whoever decided.

use std::fmt::Write as _;

use anyhow::Result;

use super::{Ended, Ran, bar, port, request, review};
use crate::bus::{Action, BarResult, NO_PROGRESS, Outcome, Point, SessionEvent};
use crate::ctx::Ctx;
use crate::harness::Home;
use crate::systems::checkpoint;
use crate::systems::decide::{self, CheckRun, Handler, Request};
use crate::world::Job;

/// The chain that judges a run when it ends: `hooks.ended`, else just `rule` for a job that declares
/// something for it to judge (an acceptance check, a review, a profile that escalates).
fn chain(ctx: &Ctx, job: &Job) -> Option<Vec<Handler>> {
    decide::chain(&ctx.cfg, Point::Ended).or_else(|| decide::declares(&ctx.cfg, job).then(|| vec![Handler::Rule]))
}

/// A run that ended done or failed: the check, then the chain. Limits, sign-outs and the first
/// missing report stay relay's own.
pub(super) fn at_end(ctx: &Ctx, job: &Job, home: &Home, run: &Ran<'_>, ended: Ended, silent: bool) -> Result<Ended> {
    let Some(handlers) = chain(ctx, job) else { return Ok(ended) };
    if !matches!(ended.0, Outcome::Done | Outcome::Failed) {
        return Ok(ended);
    }
    // The bar, before anyone judges: every handler hears of it.
    let check = match (ended.0, run.dir, job.check()) {
        (Outcome::Done, Some(dir), Some(_)) => Some(bar::run(ctx, job, home, dir, run.base, run.inbox, run.dev)?),
        _ => None,
    };
    let req = ask(ctx, job, run, &ended, check.as_ref())?;
    let rule = || ended_rule(&rule_input(ctx, job, run, &ended, silent, check.as_ref()));
    let d = decide::decide(ctx, job, &req, &handlers, &rule, Action::Accept)?;
    let carried = carry_out(ctx, job, run, ended, &d)?;
    Ok(never_done_red(carried, check.as_ref()))
}

/// A run whose acceptance check did not pass is never done: the one place that is decided.
fn never_done_red(ended: Ended, check: Option<&CheckRun>) -> Ended {
    match (ended.0, check) {
        (Outcome::Done, Some(c)) if c.result != BarResult::Pass => {
            let more = ended.1.map_or(String::new(), |r| format!("; {r}"));
            let stalled = if c.no_progress { format!(", {NO_PROGRESS}") } else { String::new() };
            (Outcome::Failed, Some(format!("acceptance check {} (exit {}){stalled}{more}", c.result.as_str(), c.exit)))
        }
        _ => ended,
    }
}

/// The request every handler is asked.
fn ask(ctx: &Ctx, job: &Job, run: &Ran<'_>, ended: &Ended, check: Option<&CheckRun>) -> Result<Request> {
    let (mut req, _) = request(ctx, job, run, Point::Ended)?;
    let how = if ended.0 == Outcome::Done { "done" } else { "failed" };
    req.situation = format!("The run ended {how}{}.", ended.1.as_deref().map(|r| format!(": {r}")).unwrap_or_default());
    if let Some(dir) = run.dir {
        let now = checkpoint::point(dir, &run.inbox.join("checkpoint.index"));
        req.evidence = checkpoint::evidence(dir, &checkpoint::base(dir, &[]), &now);
    }
    if check.is_some_and(|c| c.no_progress) {
        req.situation.push_str(
            " Its acceptance check failed at the same commit as its previous check, the same way: the session \
             committed nothing between them, so resuming it again shows the check nothing new.",
        );
    }
    req.check = check.cloned();
    req.reviewing = run.reviewing.cloned();
    if let Some(r) = run.reviewing {
        let _ = write!(
            req.situation,
            " The job is a review of {}'s branch ({} at {}); the report's `verdict:` line is its verdict, given as the `review` action.",
            r.job, r.reference, r.commit
        );
    }
    Ok(req)
}

/// What the `rule` handler judges a run by.
fn rule_input<'a>(
    ctx: &'a Ctx,
    job: &Job,
    run: &'a Ran<'_>,
    ended: &Ended,
    silent: bool,
    check: Option<&'a CheckRun>,
) -> EndRule<'a> {
    let answer = run.report.answer.as_str();
    let marker = ctx.cfg.session.final_marker.as_str();
    EndRule {
        outcome: ended.0,
        final_done: final_status_done(answer, marker),
        silent,
        follow_ups: job.follow_ups,
        check,
        answer,
        marker,
        reviewing: run.reviewing.is_some(),
        // This end is one more failed end than the job has had.
        escalate: decide::escalation(&ctx.cfg, job).map(|(to, after)| (to, after, job.failed_ends + 1)),
    }
}

/// Does what the chain decided, and says how the run ended.
fn carry_out(ctx: &Ctx, job: &Job, run: &Ran<'_>, ended: Ended, d: &decide::Decided) -> Result<Ended> {
    let port = port(ctx, job);
    let outcome = ended.0;
    let carried = d.carry(ctx, |change, action, decided_why| {
        Ok(match action {
            Action::Stop { why } => (Outcome::Failed, Some(if why.is_empty() { decided_why.to_owned() } else { why })),
            Action::Resume { message, budget } => {
                let message = if message.trim().is_empty() { "Continue where you left off.".into() } else { message };
                port.emit(SessionEvent::FollowUp { job: job.name.clone(), message, budget_usd: budget })?;
                (Outcome::Retry, Some(format!("resumed by decision {}", d.id)))
            }
            Action::Requeue { profile } => {
                requeue(ctx, job, &profile)?;
                (Outcome::Retry, Some(format!("requeued under profile {profile}")))
            }
            Action::FollowUp { brief, profile, after, fork, name } => {
                let spec = crate::systems::queue::FollowUp { name, brief, profile, after, fork };
                let queued = crate::systems::queue::follow_up(change, job, spec)?;
                let said = ended.1.map_or_else(
                    || format!("follow-up {queued} queued"),
                    |r| format!("{r}; follow-up {queued} queued"),
                );
                (outcome, Some(said))
            }
            Action::Review { verdict, reason, message } => {
                (outcome, Some(review::carry_out(change, job, run, verdict, reason, &message)?))
            }
            _ => ended,
        })
    })?;
    Ok(carried.unwrap_or_else(|| {
        let why =
            format!("decision {} ({}) not carried out: the job moved on since it was asked", d.id, d.action_name());
        (outcome, Some(why))
    }))
}

/// Sends the job back to the queue to start afresh under `profile`, its branch kept.
fn requeue(ctx: &Ctx, job: &Job, profile: &str) -> Result<()> {
    let p = ctx.cfg.profile(profile)?;
    let harnesses: Vec<String> = p.harness.iter().cloned().collect();
    let models = match (&p.harness, &p.model) {
        (Some(h), Some(m)) => [(h.clone(), m.clone())].into(),
        _ => std::collections::BTreeMap::new(),
    };
    port(ctx, job).emit(SessionEvent::Requeued {
        job: job.name.clone(),
        profile: profile.to_owned(),
        harnesses,
        models,
        effort: p.effort.clone(),
        budget_usd: p.budget.unwrap_or(job.budget_usd),
    })?;
    Ok(())
}

/// What the `rule` handler judges a run ended by.
pub(super) struct EndRule<'a> {
    pub outcome: Outcome,
    pub final_done: bool,
    pub silent: bool,
    pub follow_ups: u32,
    /// The acceptance check's latest run, for a run that ended done.
    pub check: Option<&'a CheckRun>,
    pub answer: &'a str,
    pub marker: &'a str,
    /// The job is a review of a branch.
    pub reviewing: bool,
    /// Where the profile escalates to, after how many failed ends, and how many there are with this one.
    pub escalate: Option<(String, u32, u32)>,
}

/// The `rule` handler when a run ends: a failed acceptance check resumes the session with its output,
/// unless it repeats the check before it at the same commit, which it defers (a check that could
/// not run, proves nothing or fails at the base too, stops the job); a review's verdict is taken
/// from its report; a clean done is accepted; a first silence resumes the same session; enough failed
/// ends requeue the job under the profile it escalates to. Anything else is deferred.
pub(super) fn ended_rule(r: &EndRule<'_>) -> Option<(Action, String)> {
    match r.outcome {
        Outcome::Done => done_rule(r),
        Outcome::Failed if r.silent && r.follow_ups == 0 => {
            let message = "Continue where you left off.".to_owned();
            Some((Action::Resume { message, budget: None }, "went silent once; resumed in the same session".into()))
        }
        Outcome::Failed => {
            let (to, after, failed) = r.escalate.as_ref()?;
            (failed >= after).then(|| {
                let why = format!("{failed} failed end(s), the profile escalates to {to} after {after}");
                (Action::Requeue { profile: to.clone() }, why)
            })
        }
        _ => None,
    }
}

fn done_rule(r: &EndRule<'_>) -> Option<(Action, String)> {
    if let Some(c) = r.check {
        match c.result {
            BarResult::Pass => {}
            // Nothing was committed since the same failure: another resume would check the same code.
            BarResult::Fail if c.no_progress => return Some((Action::Defer, NO_PROGRESS.into())),
            BarResult::Fail => {
                let why = format!("the acceptance check failed (exit {}); resumed with its output", c.exit);
                return Some((Action::Resume { message: check_message(c), budget: None }, why));
            }
            BarResult::CheckProvesNothing => {
                let why = format!(
                    "check-proves-nothing: `{}` also passes at the base commit, so it shows nothing about this change",
                    c.command
                );
                return Some((Action::Stop { why: why.clone() }, why));
            }
            BarResult::BaseRed => {
                let why = format!(
                    "base-red: `{}` also fails at the base commit{}, so the failure is not this job's to fix",
                    c.command,
                    if c.failing.is_empty() { String::new() } else { format!(" ({})", c.failing.join(", ")) }
                );
                return Some((Action::Stop { why: why.clone() }, why));
            }
            BarResult::CheckCouldNotRun => {
                let why = format!(
                    "check-could-not-run: `{}` timed out, was not found or was killed: {}",
                    c.command,
                    c.tail.trim()
                );
                return Some((Action::Stop { why: why.clone() }, why));
            }
        }
    }
    if r.reviewing
        && let Some((verdict, reason)) = review::verdict(r.answer, r.marker)
    {
        let tail = r.answer.rfind(r.marker).filter(|_| !r.marker.is_empty()).map_or(r.answer, |i| &r.answer[i..]);
        let message = checkpoint::bounded(tail.trim(), 4096);
        return Some((Action::Review { verdict, reason, message }, "the review's report gives its verdict".into()));
    }
    r.final_done.then(|| (Action::Accept, "done, and its report says status: done".into()))
}

/// What the worker is told when its acceptance check failed.
fn check_message(c: &CheckRun) -> String {
    format!(
        "Your run ended, and then the acceptance check `{}` was run on your branch{} and failed (exit {}). \
         Fix what it reports, commit your work, and end with your FINAL section again. The end of its output:\n\n{}",
        c.command,
        c.commit.as_deref().map_or(String::new(), |c| format!(" at {}", c.get(..12).unwrap_or(c))),
        c.exit,
        c.tail.trim_end()
    )
}

/// What a run's report says in its final section, read by the parser a review's verdict is read by
/// ([`review::final_field`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Final {
    /// The report has no final section.
    Missing,
    /// The section's `status:` value, lowercased; empty when it gives none.
    Status(String),
}

impl Final {
    pub(super) fn of(answer: &str, marker: &str) -> Self {
        review::final_section(answer, marker)
            .map_or(Self::Missing, |s| Self::Status(review::final_field(s, "status").unwrap_or_default()))
    }

    /// The report says its work is done.
    pub(super) fn done(&self) -> bool {
        matches!(self, Self::Status(s) if s.starts_with("done"))
    }

    /// What the report says, in plain words.
    fn said(&self, marker: &str) -> String {
        let section = if marker.is_empty() { "report".to_owned() } else { format!("{marker} section") };
        match self {
            Self::Missing => format!("its report has no {section}"),
            Self::Status(s) if s.is_empty() => format!("its {section} gives no status"),
            Self::Status(s) => format!("its {section} says status: {s}"),
        }
    }
}

/// Whether the report's final section says `status: done`.
pub(super) fn final_status_done(answer: &str, marker: &str) -> bool {
    Final::of(answer, marker).done()
}

/// A checkpoint at a spend cap that stopped the run: which one, and its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Halt {
    pub checkpoint: u32,
    pub why: String,
}

/// A job's terminal state, decided in this one place from how its run ended (`ended`), what its own
/// report says (`report`), and the checkpoint that stopped it, if one did (`halt`). Without a halt the
/// run ends as it ended. A halt after a report whose status is done ends the job done, the checkpoint's
/// words kept as its note: the work was finished, and more spend would not help. A halt without one
/// is a stop that waits for a human, and its reason says so, with what the report said.
pub(super) fn terminal(ended: Ended, report: &Final, halt: Option<&Halt>, marker: &str) -> Ended {
    let Some(h) = halt else { return ended };
    let words = if h.why.is_empty() { String::new() } else { format!(" ({})", h.why) };
    if report.done() {
        return (Outcome::Done, Some(format!("finished; stopped at checkpoint {}{words}", h.checkpoint)));
    }
    let said = report.said(marker);
    (
        Outcome::Failed,
        Some(format!(
            "held for a human: stopped at checkpoint {}{words} and {said}; resume with --budget to go on",
            h.checkpoint
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule<'a>(outcome: Outcome, f: impl FnOnce(&mut EndRule<'a>)) -> Option<Action> {
        let mut r = EndRule {
            outcome,
            final_done: false,
            silent: false,
            follow_ups: 0,
            check: None,
            answer: "",
            marker: "FINAL",
            reviewing: false,
            escalate: None,
        };
        f(&mut r);
        ended_rule(&r).map(|r| r.0)
    }

    #[test]
    fn the_end_rule_accepts_a_clean_done_and_resumes_one_silence() {
        assert_eq!(rule(Outcome::Done, |r| r.final_done = true), Some(Action::Accept));
        assert_eq!(rule(Outcome::Done, |_| {}), None, "no status: done, so another handler judges");
        let resumed = rule(Outcome::Failed, |r| r.silent = true);
        assert!(matches!(resumed, Some(Action::Resume { .. })), "{resumed:?}");
        let twice = rule(Outcome::Failed, |r| {
            r.silent = true;
            r.follow_ups = 1;
        });
        assert_eq!(twice, None, "silent twice");
        assert_eq!(rule(Outcome::Failed, |_| {}), None, "an error is judged elsewhere");
    }

    fn ran(result: BarResult) -> CheckRun {
        CheckRun {
            command: "cargo test".into(),
            commit: Some("0123456789abcdef".into()),
            result,
            exit: 1,
            base_exit: None,
            failing: vec!["x".into()],
            no_progress: false,
            tail: "test x ... FAILED".into(),
        }
    }

    fn with_check(check: &CheckRun) -> Option<Action> {
        rule(Outcome::Done, |r| {
            r.final_done = true;
            r.check = Some(check);
        })
    }

    #[test]
    fn a_failed_bar_resumes_with_its_output() {
        let Some(Action::Resume { message, .. }) = with_check(&ran(BarResult::Fail)) else { panic!() };
        assert!(
            message.contains("cargo test") && message.contains("FAILED") && message.contains("0123456789ab"),
            "{message}"
        );
        assert_eq!(with_check(&ran(BarResult::Pass)), Some(Action::Accept), "green and reported done");
    }

    #[test]
    fn a_failed_bar_that_repeats_the_last_one_is_handed_on_not_resumed() {
        let stuck = CheckRun { no_progress: true, ..ran(BarResult::Fail) };
        let r = EndRule {
            outcome: Outcome::Done,
            final_done: true,
            silent: false,
            follow_ups: 0,
            check: Some(&stuck),
            answer: "",
            marker: "FINAL",
            reviewing: false,
            escalate: None,
        };
        assert_eq!(ended_rule(&r), Some((Action::Defer, NO_PROGRESS.to_owned())));
        let (outcome, why) = never_done_red((Outcome::Done, None), Some(&stuck));
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(
            why.as_deref(),
            Some("acceptance check fail (exit 1), no progress: same commit, same check failure")
        );
    }

    #[test]
    fn a_bar_that_proves_nothing_could_not_run_or_is_base_red_stops_the_job() {
        for (result, name) in [
            (BarResult::CheckProvesNothing, "check-proves-nothing"),
            (BarResult::CheckCouldNotRun, "check-could-not-run"),
            (BarResult::BaseRed, "base-red"),
        ] {
            let Some(Action::Stop { why }) = with_check(&ran(result)) else { panic!("{result:?}") };
            assert!(why.starts_with(name), "{why}");
        }
    }

    #[test]
    fn a_red_run_is_never_done() {
        let done: Ended = (Outcome::Done, None);
        assert_eq!(never_done_red(done.clone(), None), done);
        assert_eq!(never_done_red(done.clone(), Some(&ran(BarResult::Pass))), done);
        for result in [BarResult::Fail, BarResult::CheckCouldNotRun, BarResult::CheckProvesNothing, BarResult::BaseRed]
        {
            let (outcome, why) = never_done_red(done.clone(), Some(&ran(result)));
            assert_eq!(outcome, Outcome::Failed);
            assert!(why.unwrap().contains(result.as_str()));
        }
        let failed: Ended = (Outcome::Failed, Some("boom".into()));
        assert_eq!(never_done_red(failed.clone(), Some(&ran(BarResult::Fail))), failed);
    }

    #[test]
    fn failed_ends_escalate_the_profile_when_enough_have_failed() {
        let at = |failed| rule(Outcome::Failed, |r| r.escalate = Some(("strong".into(), 2, failed)));
        assert_eq!(at(1), None);
        assert_eq!(at(2), Some(Action::Requeue { profile: "strong".into() }));
        assert_eq!(at(3), Some(Action::Requeue { profile: "strong".into() }));
        let first_silence = rule(Outcome::Failed, |r| {
            r.silent = true;
            r.escalate = Some(("strong".into(), 1, 1));
        });
        assert!(matches!(first_silence, Some(Action::Resume { .. })), "a silence is resumed before it escalates");
    }

    #[test]
    fn a_review_gives_its_verdict_from_its_final_section() {
        use crate::bus::{Reason, Verdict};
        let reject = "looked\nFINAL\nstatus: done\nverdict: reject\nreason: test-failure\nnotes: parse() panics";
        let a = rule(Outcome::Done, |r| {
            r.reviewing = true;
            r.answer = reject;
        });
        let Some(Action::Review { verdict, reason, message }) = a else { panic!("{a:?}") };
        assert_eq!((verdict, reason), (Verdict::Reject, Some(Reason::TestFailure)));
        assert!(message.starts_with("FINAL") && message.contains("parse() panics"), "{message}");
        let plain = rule(Outcome::Done, |r| {
            r.reviewing = true;
            r.final_done = true;
            r.answer = "FINAL\nstatus: done";
        });
        assert_eq!(plain, Some(Action::Accept), "a review that gives no verdict ends as any done run");
    }

    #[test]
    fn a_report_saying_done_is_recognised() {
        assert!(final_status_done("work\nFINAL\nstatus: done\ncommits: x", "FINAL"));
        assert!(final_status_done("FINAL\n- Status: Done | partial", "FINAL"), "as a review's fields are read");
        assert!(!final_status_done("FINAL\nstatus: partial", "FINAL"));
        assert!(!final_status_done("status: done\nno marker", "FINAL"));
        assert_eq!(Final::of("no marker", "FINAL"), Final::Missing);
        // A last line whose seventh byte ends inside a character, where `status:` would end: never a panic.
        assert!(final_status_done("FINAL\nstatus: done\n🏁完", "FINAL"));
        assert!(final_status_done("работа 🏁\nFINAL\n完了: да\nstatus: done — 完成\n🏁完\nverdict完: x", "FINAL"));
        assert_eq!(Final::of("FINAL\nstätus: done\n🏁完", "FINAL"), Final::Status(String::new()));
        assert_eq!(
            review::verdict("FINAL\nverdict: accept\n🏁完完", "FINAL"),
            Some((crate::bus::Verdict::Accept, None))
        );
        assert_eq!(
            review::verdict("FINAL\nverdict: reject\nreason🏁完: x", "FINAL"),
            Some((crate::bus::Verdict::Reject, Some(crate::bus::Reason::Other)))
        );
        assert_eq!(Final::of("FINAL\nnotes: x", "FINAL"), Final::Status(String::new()));
    }

    fn halt() -> Halt {
        Halt { checkpoint: 2, why: "the unit is done; more spend wouldn't help".into() }
    }

    #[test]
    fn a_checkpoint_stop_after_a_done_report_ends_done_with_its_words() {
        let report = Final::of("work\nFINAL\nstatus: done\ncommits: abc", "FINAL");
        let (outcome, reason) = terminal((Outcome::Failed, None), &report, Some(&halt()), "FINAL");
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(
            reason.as_deref(),
            Some("finished; stopped at checkpoint 2 (the unit is done; more spend wouldn't help)")
        );
    }

    #[test]
    fn a_checkpoint_stop_without_a_done_report_is_held_for_a_human() {
        let held =
            |answer: &str| terminal((Outcome::Failed, None), &Final::of(answer, "FINAL"), Some(&halt()), "FINAL");
        let (outcome, reason) = held("still working on it");
        assert_eq!(outcome, Outcome::Failed);
        let reason = reason.unwrap_or_default();
        assert!(reason.starts_with("held for a human: stopped at checkpoint 2 (the unit is done"), "{reason}");
        assert!(reason.contains("its report has no FINAL section"), "{reason}");
        assert!(reason.ends_with("resume with --budget to go on"), "{reason}");
        for status in ["partial", "stopped", "blocked"] {
            let (outcome, reason) = held(&format!("FINAL\nstatus: {status}"));
            assert_eq!(outcome, Outcome::Failed, "{status}");
            let reason = reason.unwrap_or_default();
            assert!(reason.contains(&format!("its FINAL section says status: {status}")), "{reason}");
        }
        let (_, reason) = held("FINAL\ncommits: none");
        assert!(reason.unwrap_or_default().contains("its FINAL section gives no status"));
    }

    #[test]
    fn without_a_checkpoint_stop_a_run_ends_as_it_ended() {
        let done = Final::of("FINAL\nstatus: done", "FINAL");
        let failed: Ended = (Outcome::Failed, Some("boom".into()));
        assert_eq!(terminal(failed.clone(), &done, None, "FINAL"), failed);
        let ok: Ended = (Outcome::Done, None);
        assert_eq!(terminal(ok.clone(), &Final::Missing, None, "FINAL"), ok);
    }
}
