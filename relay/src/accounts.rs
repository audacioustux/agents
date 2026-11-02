//! Which accounts exist and which has the most room, for every harness alike.
//! An account is a harness's home (`claude:work`, `omp:default`). Its
//! directory is found on disk each time and never written to the bus.

use std::path::PathBuf;

use crate::bus::secs;
use crate::config::Config;
use crate::harness::{Harness, Home, Meter, Meters};
use crate::world::AccountState;

/// A reading older than this is refreshed before it is trusted.
pub const STALE_MICROS: u64 = 10 * 60 * 1_000_000;

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Every usable account of every enabled harness, minus `accounts.exclude`.
pub fn discover(cfg: &Config) -> Vec<Home> {
    let user = home();
    cfg.harnesses()
        .into_iter()
        .flat_map(|h| h.homes(&user))
        .filter(|a| !cfg.accounts.exclude.contains(&a.key()))
        .collect()
}

pub fn harness_of(home: &Home) -> &'static dyn Harness {
    crate::harness::by_id(home.harness).expect("homes come from the registry")
}

/// Of the `governing` meters (a subset of `meters`), the one closest to its
/// limit that has not reset yet, with its name.
pub fn tightest<'m>(governing: &[&Meter], meters: &'m Meters, now_s: i64) -> Option<(&'m str, &'m Meter)> {
    meters
        .iter()
        .filter(|&(_, m)| governing.iter().any(|g| std::ptr::eq(*g, m)) && m.resets_at.is_none_or(|r| r > now_s))
        .max_by(|a, b| a.1.utilization.total_cmp(&b.1.utilization))
        .map(|(k, m)| (k.as_str(), m))
}

/// One meter as an operator reads it: `100% openai-codex/5 hours, resets Nov 2 21:42Z`.
pub fn meter_line(name: &str, m: &Meter) -> String {
    let at = m.resets_at.map_or(String::new(), |r| format!(", resets {}", fmt_time(r)));
    format!("{:.0}% {name}{at}", m.utilization * 100.0)
}

pub fn fmt_time(unix_s: i64) -> String {
    jiff::Timestamp::from_second(unix_s).map_or_else(|_| "?".into(), |t| t.strftime("%b %-d %H:%MZ").to_string())
}

/// Higher is better: room left per day until reset, for the tightest meter,
/// since quota still unused at a reset is lost. An unknown reset counts as a
/// week away; an unmetered account (no meters) scores 0 and is tried last.
pub fn score(meters: &[&Meter], now_s: i64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let per_day = |m: &Meter| {
        let days = m.resets_at.map_or(7.0, |r| (r - now_s) as f64 / 86_400.0).max(1.0);
        (1.0 - m.utilization) / days
    };
    meters.iter().filter(|m| m.resets_at.is_none_or(|r| r > now_s)).map(|m| per_day(m)).reduce(f64::min).unwrap_or(0.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Disabled,
    SignedOut,
    Limited,
    /// The governing meter closest to its limit is at the floor; it names that meter.
    AtFloor(String),
    Unread,
    /// The last reading is too old to trust; a fresh one has been asked for.
    Stale,
    /// The harness cannot tell which meters govern this job.
    Blind(String),
}

impl Refusal {
    pub fn describe(&self) -> String {
        match self {
            Self::Disabled => "disabled".into(),
            Self::SignedOut => "signed out (sign in, then: relay quota enable)".into(),
            Self::Limited => "limited".into(),
            Self::AtFloor(meter) => format!("at floor: {meter}"),
            Self::Unread => "not read yet".into(),
            Self::Stale => "reading is stale".into(),
            Self::Blind(why) => why.clone(),
        }
    }
}

/// The account's own state, before any job is considered.
fn account_refusal(harness: &dyn Harness, state: Option<&AccountState>, now_s: i64) -> Option<Refusal> {
    let a = state.cloned().unwrap_or_default();
    if !a.enabled {
        Some(Refusal::Disabled)
    } else if a.signed_out {
        Some(Refusal::SignedOut)
    } else if a.limited_until.is_some_and(|u| u > now_s) {
        Some(Refusal::Limited)
    } else if harness.caps().quota && a.read_at.is_none() {
        // Unread is not the same as having room.
        Some(Refusal::Unread)
    } else {
        None
    }
}

/// Whether the account can take a job on `model` now (`now_us`, bus time),
/// and how good a fit it is.
pub fn fit(
    home: &Home,
    state: Option<&AccountState>,
    model: Option<&str>,
    floor: f64,
    now_us: u64,
) -> Result<f64, Refusal> {
    let h = harness_of(home);
    let now_s = secs(now_us);
    if let Some(r) = account_refusal(h, state, now_s) {
        return Err(r);
    }
    // An old reading may say there is room that has since been spent.
    if is_stale(h, state, now_us) {
        return Err(Refusal::Stale);
    }
    let empty = Meters::new();
    let meters = state.map_or(&empty, |s| &s.meters);
    let governing = h.meters_for(model, meters).map_err(|e| Refusal::Blind(e.0))?;
    if let Some((name, m)) = tightest(&governing, meters, now_s)
        && m.utilization >= floor
    {
        return Err(Refusal::AtFloor(meter_line(name, m)));
    }
    Ok(score(&governing, now_s))
}

/// Whether the reading should be taken again: never read, old, or past a reset it names.
pub fn is_stale(harness: &dyn Harness, state: Option<&AccountState>, now_us: u64) -> bool {
    if !harness.caps().quota {
        return false;
    }
    let Some(read_us) = state.and_then(|a| a.read_at) else { return true };
    let (now_s, read_s) = (secs(now_us), secs(read_us));
    now_us.saturating_sub(read_us) > STALE_MICROS
        || state
            .into_iter()
            .flat_map(|a| a.meters.values())
            .filter_map(|m| m.resets_at)
            .any(|r| read_s < r && r <= now_s)
}

/// Whether to take a reading now: it is stale, and no failed read is still
/// backing off (one waits as long as a good reading would last).
pub fn due(harness: &dyn Harness, state: Option<&AccountState>, now_us: u64) -> bool {
    is_stale(harness, state, now_us)
        && state.and_then(|a| a.unreadable_at).is_none_or(|t| now_us.saturating_sub(t) > STALE_MICROS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(u: f64, r: Option<i64>) -> Meter {
        Meter { utilization: u, resets_at: r }
    }

    fn claude(label: &str) -> Home {
        Home { harness: "claude", label: label.into(), dir: PathBuf::from("/x") }
    }

    #[test]
    fn fit_and_refusals() {
        let now_us = 1_000 * 1_000_000;
        let now = secs(now_us);
        let read = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        assert_eq!(fit(&claude("a"), None, None, 0.9, now_us), Err(Refusal::Unread));
        assert!(fit(&claude("a"), Some(&read), None, 0.9, now_us).is_ok());
        let old = AccountState { read_at: Some(now_us - STALE_MICROS - 1), ..read.clone() };
        assert_eq!(fit(&claude("a"), Some(&old), None, 0.9, now_us), Err(Refusal::Stale));
        let mut full = read.clone();
        full.meters.insert("seven_day".into(), m(0.95, Some(now + 10)));
        let held = fit(&claude("a"), Some(&full), None, 0.9, now_us).unwrap_err();
        assert!(held.describe().starts_with("at floor: 95% seven_day, resets "), "{}", held.describe());
        full.meters.insert("seven_day".into(), m(0.95, Some(now - 10)));
        assert!(fit(&claude("a"), Some(&full), None, 0.9, now_us).is_ok(), "a meter past its reset no longer counts");
        let limited = AccountState { limited_until: Some(now + 1), ..read.clone() };
        assert_eq!(fit(&claude("a"), Some(&limited), None, 0.9, now_us), Err(Refusal::Limited));
    }

    #[test]
    fn omp_fit_depends_on_the_model() {
        let now_us = 1_000 * 1_000_000;
        let now = secs(now_us);
        let omp = Home { harness: "omp", label: "default".into(), dir: PathBuf::from("/o") };
        let mut st = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        st.meters.insert("openai-codex/7 days".into(), m(0.2, Some(now + 86_400 * 5)));
        assert!(fit(&omp, Some(&st), Some("openai-codex/gpt-5.2"), 0.9, now_us).is_ok());
        assert!(matches!(fit(&omp, Some(&st), Some("anthropic/x"), 0.9, now_us), Err(Refusal::Blind(_))));
        assert!(matches!(fit(&omp, Some(&st), None, 0.9, now_us), Err(Refusal::Blind(_))));
    }

    #[test]
    fn score_prefers_room_that_would_otherwise_be_lost() {
        let soon = m(0.5, Some(86_400));
        let later = m(0.4, Some(6 * 86_400));
        assert!(score(&[&soon], 0) > score(&[&later], 0));
        assert!((score(&[], 0)).abs() < f64::EPSILON, "unmetered scores 0");
    }

    #[test]
    fn staleness() {
        let h = crate::harness::by_id("claude").unwrap();
        let now_us = 2_000 * 1_000_000;
        let mut a = AccountState { read_at: Some(now_us - 100 * 1_000_000), ..Default::default() };
        assert!(!is_stale(h, Some(&a), now_us));
        a.meters.insert("seven_day".into(), m(1.0, Some(1_950)));
        assert!(is_stale(h, Some(&a), now_us), "read before a reset that has now passed");
        let failed = AccountState { unreadable_at: Some(now_us - 100 * 1_000_000), ..Default::default() };
        assert!(is_stale(h, Some(&failed), now_us) && !due(h, Some(&failed), now_us), "a failed read backs off");
        let failed = AccountState { unreadable_at: Some(now_us - STALE_MICROS - 1), ..failed };
        assert!(due(h, Some(&failed), now_us), "and is tried again once a reading would be stale");
    }
}
