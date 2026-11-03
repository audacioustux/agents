//! queue: what a job declares about how its result is accepted: a check its branch must pass, or the
//! build job it reviews. Declared when the job is queued and kept on its record.

use std::collections::BTreeMap;

use anyhow::Result;
use clap::Args;

use crate::bus::Acceptance;
use crate::config::{Profile, model_family};
use crate::ctx::Ctx;
use crate::world::Job;

/// The acceptance flags of `queue add`.
#[derive(Debug, Default, Args)]
pub struct Declare {
    /// The bar: a command run on the committed tip of the job's branch, in a clean checkout inside the
    /// session's sandbox, when a run ends done. The run is accepted only if it exits 0; a failure resumes the
    /// session with the output's tail. Default: the profile's `check`.
    #[arg(long)]
    check: Option<String>,
    /// Also run the check at the job's base commit, where it must fail: a check that passes without the
    /// change proves nothing (`check-proves-nothing`, never accepted). A check filtering tests needs a
    /// "no tests is a failure" flag, such as `--no-tests=fail` for nextest.
    #[arg(long)]
    check_red_first: bool,
    /// Review the build job JOB: read-only, `--after JOB`, with JOB's branch fetched into the repository
    /// before the review starts and named in the prompt. The reviewer's verdict is recorded on JOB.
    #[arg(long, value_name = "JOB", conflicts_with_all = ["fork", "no_readonly", "check"])]
    review_of: Option<String>,
}

/// What a job declared, checked against the jobs there are.
pub struct Declared<'a> {
    pub acceptance: Acceptance,
    /// The build job under review.
    pub review: Option<&'a Job>,
}

impl Declare {
    /// A review is read-only whatever the profile says.
    pub fn is_review(&self) -> bool {
        self.review_of.is_some()
    }

    /// The acceptance this line and the profile give a job that is (`readonly`) or is not a build job.
    pub fn resolve<'a>(
        &self,
        ctx: &Ctx,
        profile: Option<&Profile>,
        readonly: bool,
        jobs: &'a BTreeMap<String, Job>,
    ) -> Result<Declared<'a>> {
        let review = self.review_of.as_deref().map(|r| review_target(r, jobs)).transpose()?;
        let acceptance = match review {
            Some(builder) => Acceptance::ReviewOf(builder.name.clone()),
            None => self.check_of(profile, readonly)?,
        };
        // A check resumes the session and a reject resumes the builder: spending the ceiling bounds.
        let spends = !acceptance.is_none() || profile.is_some_and(|p| p.escalate_to.is_some());
        if spends && ctx.cfg.limits.job_ceiling_usd.is_none() {
            fail!(
                Refused,
                "a job that is checked, reviewed or escalated can be resumed again and again; set limits.job_ceiling_usd to bound what it may spend"
            );
        }
        Ok(Declared { acceptance, review })
    }

    /// The line's check, else a build job's profile default, and whether it needs the revert control.
    fn check_of(&self, profile: Option<&Profile>, readonly: bool) -> Result<Acceptance> {
        if readonly && self.check.is_some() {
            fail!(Usage, "--check judges a build job's branch; a read-only job has none");
        }
        let inherited = profile.filter(|_| !readonly && self.check.is_none());
        let Some(command) = self.check.clone().or_else(|| inherited.and_then(|p| p.check.clone())) else {
            if self.check_red_first {
                fail!(Usage, "--check-red-first needs a check");
            }
            return Ok(Acceptance::None);
        };
        // A profile's red-first flag is for its own check.
        let red = self.check_red_first || inherited.is_some_and(|p| p.check_red_first == Some(true));
        Ok(if red { Acceptance::CheckRedFirst(command) } else { Acceptance::Check(command) })
    }
}

/// The build job `name` that a review is of.
fn review_target<'a>(name: &str, jobs: &'a BTreeMap<String, Job>) -> Result<&'a Job> {
    let Some(j) = jobs.get(name) else { fail!(NotFound, "--review-of {name}: no job named {name}") };
    if j.readonly() {
        fail!(Conflict, "--review-of {name}: {name} is a read-only job; a review is of a build job's branch");
    }
    Ok(j)
}

/// Refuses a review that can only run on the model family that built `builder`, when the builder's
/// profile asks for independence. Where the reviewer might run on another family, dispatch decides;
/// a session that starts on the same family then fails (see `session`).
pub fn refuse_same_family(
    ctx: &Ctx,
    builder: &Job,
    harnesses: &[String],
    models: &BTreeMap<String, String>,
) -> Result<()> {
    let asks = builder.profile.as_deref().and_then(|p| ctx.cfg.profiles.get(p)).and_then(|p| p.independent.as_deref());
    let Some(built) = asks.and(ctx.cfg.model_of(builder)).map(|m| model_family(&m)) else {
        return Ok(());
    };
    let candidates: Vec<String> = if harnesses.is_empty() {
        ctx.cfg.harnesses().iter().map(|h| h.id().to_owned()).collect()
    } else {
        harnesses.to_vec()
    };
    let families = candidates
        .iter()
        .map(|h| models.get(h).cloned().or_else(|| ctx.cfg.harness(h).model).map(|m| model_family(&m)));
    if candidates.is_empty() || !families.into_iter().all(|f| f.as_deref() == Some(built.as_str())) {
        return Ok(());
    }
    fail!(
        Refused,
        "{} is built on the {built} family and its profile asks for an independent review; pick a reviewer model of another family",
        builder.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(check: Option<&str>, red: Option<bool>) -> Profile {
        Profile { check: check.map(Into::into), check_red_first: red, ..Profile::default() }
    }

    fn declare(check: Option<&str>, red: bool) -> Declare {
        Declare { check: check.map(Into::into), check_red_first: red, review_of: None }
    }

    #[test]
    fn a_build_job_takes_its_profiles_check_and_the_lines_wins() {
        let p = profile(Some("make test"), Some(true));
        let from_profile = declare(None, false).check_of(Some(&p), false).unwrap();
        assert_eq!(
            from_profile,
            Acceptance::CheckRedFirst("make test".into()),
            "the profile's flag goes with its check"
        );
        let own = declare(Some("make lint"), false).check_of(Some(&p), false).unwrap();
        assert_eq!(own, Acceptance::Check("make lint".into()), "the line's check is not held to the profile's flag");
        let flagged = declare(Some("make lint"), true).check_of(Some(&p), false).unwrap();
        assert_eq!(flagged, Acceptance::CheckRedFirst("make lint".into()));
        assert_eq!(declare(None, false).check_of(None, false).unwrap(), Acceptance::None);
    }

    #[test]
    fn a_read_only_job_has_no_check_to_inherit_or_ask_for() {
        let p = profile(Some("make test"), None);
        assert_eq!(declare(None, false).check_of(Some(&p), true).unwrap(), Acceptance::None);
        assert!(declare(Some("x"), false).check_of(None, true).is_err());
        assert!(declare(None, true).check_of(None, false).is_err(), "a revert control needs a check");
    }
}
