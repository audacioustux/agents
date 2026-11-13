//! Which accounts exist and which has the most room, for every harness alike.
//! An account is a harness's home (`claude:work`, `omp:default`). Its
//! directory is found on disk each time and never written to the bus.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::bus::secs;
use crate::config::{Config, Limits};
use crate::harness::{Harness, Home, Identity, Meter, Meters, SignIn};
use crate::world::{AccountState, World};

/// A reading older than this is refreshed before it is trusted.
pub const STALE_MICROS: u64 = 10 * 60 * 1_000_000;

#[allow(clippy::disallowed_methods, reason = "whose home is the caller's choice; children are told it")]
pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Every account on this machine, of every harness, whether any project uses it or not: what
/// a view outside every project shows ([`crate::systems::quota::Rules::MachineWide`]).
pub fn discover_all() -> Vec<Home> {
    let user = home();
    crate::harness::ALL.iter().flat_map(|h| h.homes(&user)).collect()
}

/// Every usable account of every enabled harness, minus `accounts.exclude`.
pub fn discover(cfg: &Config) -> Vec<Home> {
    discover_in(crate::host::Mode::Record, cfg)
}

/// As [`discover`], the same accounts, recording the programs `PATH` finds only as `mode` allows:
/// [`crate::host::Mode::ReadOnly`] for a view that writes nothing.
pub fn discover_in(mode: crate::host::Mode, cfg: &Config) -> Vec<Home> {
    let user = home();
    cfg.harnesses_in(mode)
        .into_iter()
        .flat_map(|h| h.homes(&user))
        .filter(|a| !cfg.accounts.exclude.contains(&a.key()))
        .collect()
}

pub fn harness_of(home: &Home) -> &'static dyn Harness {
    crate::harness::by_id(home.harness).expect("homes come from the registry")
}

/// Who a home is signed in as, from its own files.
pub fn sign_in(home: &Home) -> SignIn {
    harness_of(home).sign_in(home)
}

/// The homes that are one account, by sign-in: each home that shares its
/// harness and account with an earlier one, mapped to the first of them.
/// Such homes share one quota and one seat budget.
pub fn twins(found: &[Home]) -> BTreeMap<String, String> {
    let mut first: BTreeMap<(&str, String), String> = BTreeMap::new();
    let mut twins = BTreeMap::new();
    for home in found {
        let SignIn::As(Identity { id, .. }) = sign_in(home) else { continue };
        match first.entry((home.harness, id)) {
            std::collections::btree_map::Entry::Occupied(e) => {
                twins.insert(home.key(), e.get().clone());
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert(home.key());
            }
        }
    }
    twins
}

/// The seat an account takes: its own, or its first twin's.
pub fn seat<'a>(twins: &'a BTreeMap<String, String>, account: &'a str) -> &'a str {
    twins.get(account).map_or(account, String::as_str)
}

/// Whether `harness` meters each provider apart (omp: one profile serves several providers, each
/// with a quota of its own), so its seats are counted per provider.
fn per_provider(harness: &str) -> bool {
    harness == "omp"
}

/// The provider whose quota a `model` draws on, for the harnesses that have
/// several (omp); empty for the others, and for a model with no prefix. What a
/// dispatch records; a seat is counted by [`draws`].
pub fn provider_of<'m>(model: Option<&'m str>, harness: &str) -> &'m str {
    if !per_provider(harness) {
        return "";
    }
    model.and_then(|m| m.split_once('/')).map_or("", |(provider, _)| provider)
}

/// What a session's seat on an account is counted against.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Draws {
    /// The account's one budget, on a harness with one quota per account (claude).
    Account,
    /// One provider's budget, on a harness that meters each apart (omp's model prefix).
    Provider(String),
    /// Some provider of such a harness, not known which: a run recorded with no provider (from
    /// before providers were recorded, on the configured default), or a model with no prefix. It
    /// may draw on any of them, so it counts against every one, and any session on the account
    /// counts against it.
    Unknown,
}

/// What a session of `model` on `harness` draws on. A recorded `provider` (non-empty) is that
/// provider's on a per-provider harness.
pub fn draws(model: Option<&str>, provider: &str, harness: &str) -> Draws {
    if !per_provider(harness) {
        return Draws::Account;
    }
    match (provider, provider_of(model, harness)) {
        ("", "") => Draws::Unknown,
        ("", p) | (p, _) => Draws::Provider(p.to_owned()),
    }
}

/// Brings what `world` knows of each home in line with what the home's files
/// say now (call it after `hear_accounts`). A home with no sign-in is marked
/// so; one whose sign-in changed since it was last read is read again, and is
/// no longer taken for signed out. Twins share the newest reading and the
/// longest limit, so a reading or a limit on one is a reading or a limit on both.
pub fn settle(world: &mut World, found: &[Home]) {
    for home in found {
        let key = home.key();
        let current = sign_in(home);
        let a = world.accounts.entry(key).or_default();
        a.unsigned = current == SignIn::Out;
        if let SignIn::As(id) = &current
            && a.sign_in.as_ref().is_some_and(|s| *s != id.stamp)
        {
            (a.read_at, a.unreadable_at, a.signed_out, a.failing) = (None, None, false, None);
        }
    }
    for (twin, first) in twins(found) {
        let (Some(a), Some(b)) = (world.accounts.get(&twin), world.accounts.get(&first)) else { continue };
        let newer = if a.read_at > b.read_at { a } else { b };
        let (meters, read_at) = (newer.meters.clone(), newer.read_at);
        let limited_until = a.limited_until.max(b.limited_until);
        for key in [&twin, &first] {
            if let Some(s) = world.accounts.get_mut(key) {
                s.meters.clone_from(&meters);
                (s.read_at, s.limited_until) = (read_at, limited_until);
            }
        }
    }
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

/// Of the `governing` meters, the fullest one at its own floor (one about to
/// reset may be allowed further), if any.
pub fn at_floor<'m>(
    governing: &[&Meter],
    meters: &'m Meters,
    limits: &Limits,
    now_s: i64,
) -> Option<(&'m str, &'m Meter)> {
    meters
        .iter()
        .filter(|&(_, m)| governing.iter().any(|g| std::ptr::eq(*g, m)) && m.resets_at.is_none_or(|r| r > now_s))
        .filter(|(_, m)| m.utilization >= limits.floor_for(m.resets_at, now_s).0)
        .max_by(|a, b| a.1.utilization.total_cmp(&b.1.utilization))
        .map(|(k, m)| (k.as_str(), m))
}

/// One meter as an operator reads it: `100% openai-codex/5 hours, resets Nov 2 21:42Z`.
pub fn meter_line(name: &str, m: &Meter) -> String {
    let at = m.resets_at.map_or(String::new(), |r| format!(", resets {}", fmt_time(r)));
    format!("{:.0}% {name}{at}", m.utilization * 100.0)
}

/// What `m`'s floor is when it differs from `limits.floor`, in words: `floor 98%, resets in 10h`.
pub fn floor_note(limits: &Limits, m: &Meter, now_s: i64) -> Option<String> {
    let (floor, near) = limits.floor_for(m.resets_at, now_s);
    let left = m.resets_at.map_or(0, |r| r - now_s);
    near.then(|| format!("floor {:.0}%, resets in {}", floor * 100.0, fmt_left(left)))
}

/// Time left, to the hour (`1d17h`, `2d`, `10h`), to the minute under an hour (`57m`), or to the
/// day from ten days out (`30d`): never wider than five columns.
pub fn fmt_left(secs: i64) -> String {
    if secs < 3600 {
        return format!("{}m", (secs + 59) / 60);
    }
    let hours = (secs + 1800) / 3600;
    if hours >= 10 * 24 {
        // Weeks out the hours tell nothing, and dropping them keeps every time left within five columns.
        return format!("{}d", (hours + 12) / 24);
    }
    match (hours / 24, hours % 24) {
        (0, h) => format!("{h}h"),
        (d, 0) => format!("{d}d"),
        (d, h) => format!("{d}d{h}h"),
    }
}

pub fn fmt_time(unix_s: i64) -> String {
    jiff::Timestamp::from_second(unix_s)
        .map_or_else(|_| "?".into(), |t| crate::os::local(t).strftime("%b %-d %H:%M %Z").to_string())
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
    /// The home holds no signed-in account.
    NotSignedIn,
    SignedOut,
    Limited,
    /// The governing meter closest to its limit is at the floor; it names that meter.
    AtFloor(String),
    Unread,
    /// Never read, and the last attempt failed: nothing will change until
    /// someone signs it in or excludes it.
    /// What the reads since said went wrong ([`Failing::describe`]), when they did.
    Unreadable(Option<String>),
    /// The last reading is too old to trust; a fresh one has been asked for. With why the reads
    /// since gave nothing, when they did not.
    Stale(Option<String>),
    /// The harness cannot tell which meters govern this job.
    Blind(String),
}

impl Refusal {
    pub fn describe(&self) -> String {
        match self {
            Self::Disabled => "disabled".into(),
            Self::NotSignedIn => "not signed in (sign in, then: relay quota refresh)".into(),
            Self::SignedOut => "signed out (sign in, then: relay quota enable)".into(),
            Self::Limited => "limited".into(),
            Self::AtFloor(meter) => format!("at floor: {meter}"),
            Self::Unread => "not read yet".into(),
            Self::Unreadable(None) => "usage unreadable (sign it in, or list it in accounts.exclude)".into(),
            Self::Stale(None) => "reading is stale".into(),
            Self::Unreadable(Some(why)) | Self::Stale(Some(why)) => format!("reading {why}"),
            Self::Blind(why) => why.clone(),
        }
    }
}

/// Why the reads since the last good one gave nothing, in words, when they did not.
fn failing(state: Option<&AccountState>) -> Option<String> {
    state?.failure().map(|f| f.describe())
}

/// The account's own state, before any job is considered.
fn account_refusal(harness: &dyn Harness, state: Option<&AccountState>, now_s: i64) -> Option<Refusal> {
    let a = state.cloned().unwrap_or_default();
    if !a.enabled {
        Some(Refusal::Disabled)
    } else if a.unsigned {
        Some(Refusal::NotSignedIn)
    } else if a.signed_out {
        Some(Refusal::SignedOut)
    } else if a.limited_until.is_some_and(|u| u > now_s) {
        Some(Refusal::Limited)
    } else if harness.caps().quota && a.read_at.is_none() {
        // Unread is not the same as having room.
        Some(if a.unreadable_at.is_some() { Refusal::Unreadable(failing(state)) } else { Refusal::Unread })
    } else {
        None
    }
}

/// What holds the account back on every project alike (no sign-in, a sign-out, a limit, no or a
/// stale reading), whatever one project's switch (`quota disable`) says: `None` when nothing does.
pub fn shared_refusal(harness: &dyn Harness, state: Option<&AccountState>, now_us: u64) -> Option<Refusal> {
    let a = AccountState { enabled: true, ..state.cloned().unwrap_or_default() };
    account_refusal(harness, Some(&a), secs(now_us))
        .or_else(|| is_stale(harness, Some(&a), now_us).then(|| Refusal::Stale(failing(state))))
}

/// Whether the account can take a job on `model` now (`now_us`, bus time),
/// and how good a fit it is.
pub fn fit(
    home: &Home,
    state: Option<&AccountState>,
    model: Option<&str>,
    limits: &Limits,
    now_us: u64,
) -> Result<f64, Refusal> {
    let h = harness_of(home);
    // An old reading may say there is room that has since been spent.
    if account_refusal(h, state, secs(now_us)).is_none() && is_stale(h, state, now_us) {
        return Err(Refusal::Stale(failing(state)));
    }
    fit_as_read(home, state, model, limits, now_us)
}

/// As [`fit`], taking the last reading as it is however old: how the account stood when it was
/// read. Never a reason to start a session (an old reading may say there is room since spent);
/// dispatch asks it only to tell whether a reading under way is worth waiting for.
pub fn fit_as_read(
    home: &Home,
    state: Option<&AccountState>,
    model: Option<&str>,
    limits: &Limits,
    now_us: u64,
) -> Result<f64, Refusal> {
    let h = harness_of(home);
    let now_s = secs(now_us);
    if let Some(r) = account_refusal(h, state, now_s) {
        return Err(r);
    }
    let empty = Meters::new();
    let meters = state.map_or(&empty, |s| &s.meters);
    let governing = h.meters_for(model, meters).map_err(|e| Refusal::Blind(e.0))?;
    if let Some((name, m)) = at_floor(&governing, meters, limits, now_s) {
        let note = floor_note(limits, m, now_s).map_or(String::new(), |n| format!(" ({n})"));
        return Err(Refusal::AtFloor(format!("{}{note}", meter_line(name, m))));
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

/// When (bus time) the account's last reading went, or will go, stale: 10 minutes after it was
/// taken, or at the first reset it names after it was taken, whichever is sooner. `None` unread.
pub fn stale_since(state: Option<&AccountState>) -> Option<u64> {
    let a = state?;
    let read_us = a.read_at?;
    let resets = a.meters.values().filter_map(|m| m.resets_at).filter(|&r| r > secs(read_us));
    let resets = resets.filter_map(|r| u64::try_from(r).ok()).map(|r| r.saturating_mul(1_000_000));
    resets.chain([read_us.saturating_add(STALE_MICROS + 1)]).min()
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
        Meter { utilization: u, resets_at: r, ..Meter::default() }
    }

    fn claude(label: &str) -> Home {
        Home { harness: "claude", label: label.into(), dir: PathBuf::from("/x") }
    }

    #[test]
    fn fit_and_refusals() {
        let now_us = 1_000 * 1_000_000;
        let now = secs(now_us);
        let read = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        assert_eq!(fit(&claude("a"), None, None, &Limits::default(), now_us), Err(Refusal::Unread));
        let failed = AccountState { unreadable_at: Some(now_us - 1), ..Default::default() };
        assert_eq!(fit(&claude("a"), Some(&failed), None, &Limits::default(), now_us), Err(Refusal::Unreadable(None)));
        assert!(fit(&claude("a"), Some(&read), None, &Limits::default(), now_us).is_ok());
        let old = AccountState { read_at: Some(now_us - STALE_MICROS - 1), ..read.clone() };
        assert_eq!(fit(&claude("a"), Some(&old), None, &Limits::default(), now_us), Err(Refusal::Stale(None)));
        let mut full = read.clone();
        full.meters.insert("seven_day".into(), m(0.95, Some(now + 10)));
        let held = fit(&claude("a"), Some(&full), None, &Limits::default(), now_us).unwrap_err();
        assert!(held.describe().starts_with("at floor: 95% seven_day, resets "), "{}", held.describe());
        full.meters.insert("seven_day".into(), m(0.95, Some(now - 10)));
        assert!(
            fit(&claude("a"), Some(&full), None, &Limits::default(), now_us).is_ok(),
            "a meter past its reset no longer counts"
        );
        let limited = AccountState { limited_until: Some(now + 1), ..read.clone() };
        assert_eq!(fit(&claude("a"), Some(&limited), None, &Limits::default(), now_us), Err(Refusal::Limited));
    }

    #[test]
    fn a_meter_about_to_reset_has_its_own_floor() {
        use crate::config::NearReset;
        let now_us = 1_000 * 1_000_000;
        let now = secs(now_us);
        let limits =
            Limits { floor_near_reset: Some(NearReset { within_hours: 12.0, floor: 0.98 }), ..Limits::default() };
        let mut st = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        st.meters.insert("seven_day".into(), m(0.91, Some(now + 10 * 3600)));
        assert!(fit(&claude("a"), Some(&st), None, &limits, now_us).is_ok(), "91% a few hours from a reset");
        assert!(fit(&claude("a"), Some(&st), None, &Limits::default(), now_us).is_err(), "without the setting");
        st.meters.insert("seven_day".into(), m(0.99, Some(now + 10 * 3600)));
        let held = fit(&claude("a"), Some(&st), None, &limits, now_us).unwrap_err().describe();
        assert!(held.starts_with("at floor: 99% seven_day") && held.ends_with("(floor 98%, resets in 10h)"), "{held}");
        // Per meter: the far meter keeps the ordinary floor even when another is about to reset.
        st.meters.insert("seven_day".into(), m(0.91, Some(now + 10 * 3600)));
        st.meters.insert("five_hour".into(), m(0.92, Some(now + 20 * 3600)));
        let held = fit(&claude("a"), Some(&st), None, &limits, now_us).unwrap_err().describe();
        assert!(held.starts_with("at floor: 92% five_hour") && !held.contains("floor 98%"), "{held}");
    }

    /// A claude home under `root` signed in as `uuid` on `plan`; `None` leaves no `.claude.json`.
    fn signed_in(root: &std::path::Path, label: &str, account: Option<(&str, &str)>) -> Home {
        let dir = root.join(label);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some((uuid, plan)) = account {
            let json = format!(r#"{{"oauthAccount":{{"accountUuid":"{uuid}","planDisplayName":"{plan}"}}}}"#);
            std::fs::write(dir.join(".claude.json"), json).unwrap();
        }
        Home { harness: "claude", label: label.into(), dir }
    }

    #[test]
    fn settle_shares_twins_marks_unsigned_homes_and_rereads_a_new_sign_in() {
        let d = crate::config::tempdir::Dir::new();
        let (one, two, three) = ("uuid-one", "uuid-two", "uuid-three");
        let found = [
            signed_in(d.path(), "a", Some((one, "Ultra"))),
            signed_in(d.path(), "b", Some((one, "Ultra"))),
            signed_in(d.path(), "c", Some((two, "Pro"))),
            signed_in(d.path(), "d", None),
        ];
        assert_eq!(twins(&found), BTreeMap::from([("claude:b".to_owned(), "claude:a".to_owned())]));
        let mut w = World::default();
        let read = AccountState { read_at: Some(5), limited_until: Some(9), ..Default::default() };
        w.accounts.insert("claude:a".into(), read);
        settle(&mut w, &found);
        assert_eq!((w.accounts["claude:b"].read_at, w.accounts["claude:b"].limited_until), (Some(5), Some(9)));
        assert!(w.accounts["claude:d"].unsigned && !w.accounts["claude:c"].unsigned);

        let SignIn::As(id) = sign_in(&found[2]) else { panic!("signed in") };
        let out = AccountState { read_at: Some(5), signed_out: true, sign_in: Some(id.stamp), ..Default::default() };
        w.accounts.insert("claude:c".into(), out);
        settle(&mut w, &found);
        assert!(w.accounts["claude:c"].signed_out, "the sign-in is the one it was found signed out under");
        // A reading that works on a twin ends what a failed read of this home said.
        let failed = crate::world::Failing { why: crate::bus::ReadFailure::NoMeters, tries: 2, since: 1, last: 4 };
        w.accounts.get_mut("claude:b").unwrap().failing = Some(failed.clone());
        w.accounts.get_mut("claude:b").unwrap().read_at = Some(3);
        settle(&mut w, &found);
        assert_eq!(w.accounts["claude:b"].failure(), None, "claude:a was read at 5, after the failure at 4");
        // A new sign-in is a new account: nothing said about the old one stands.
        w.accounts.get_mut("claude:c").unwrap().failing = Some(failed);
        let again = signed_in(d.path(), "c", Some((three, "Pro")));
        settle(&mut w, &[again]);
        let c = &w.accounts["claude:c"];
        assert!(!c.signed_out && c.read_at.is_none(), "a new sign-in is read again and no longer taken for lapsed");
        assert!(c.failing.is_none(), "and a failed read of the old sign-in says nothing of it");
    }

    #[test]
    fn a_home_with_no_sign_in_is_never_a_fit() {
        let mut st = AccountState { read_at: Some(1_000_000), ..Default::default() };
        assert!(fit(&claude("a"), Some(&st), None, &Limits::default(), 2_000_000).is_ok());
        st.unsigned = true;
        assert_eq!(fit(&claude("a"), Some(&st), None, &Limits::default(), 2_000_000), Err(Refusal::NotSignedIn));
    }

    /// Review of #66: antigravity meters its model groups apart. A Gemini job is judged by the
    /// Gemini week, not by the spent Claude & GPT one; a Claude job by the shared group's.
    #[test]
    fn an_omp_job_is_judged_by_the_groups_that_cover_its_model() {
        let now_us = 1_000 * 1_000_000;
        let reset = Some(secs(now_us) + 86_400 * 3);
        let omp = Home { harness: "omp", label: "default".into(), dir: PathBuf::from("/o") };
        let mut st = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        for (name, used) in
            [("google-antigravity/Gemini (Weekly)", 0.3), ("google-antigravity/Claude & GPT (shared) (Weekly)", 1.0)]
        {
            st.meters.insert(name.into(), Meter::named(name, used, reset));
        }
        let fit = |model| fit(&omp, Some(&st), Some(model), &Limits::default(), now_us);
        assert!(fit("google-antigravity/gemini-3-pro").is_ok(), "{:?}", fit("google-antigravity/gemini-3-pro"));
        for model in ["google-antigravity/claude-opus-4-6", "google-antigravity/gpt-oss-120b"] {
            assert!(matches!(fit(model), Err(Refusal::AtFloor(_))), "{model}: {:?}", fit(model));
        }
        assert!(matches!(fit("google-antigravity/llama-9"), Err(Refusal::Blind(_))), "no group covers it");
    }

    #[test]
    fn omp_fit_depends_on_the_model() {
        let now_us = 1_000 * 1_000_000;
        let now = secs(now_us);
        let omp = Home { harness: "omp", label: "default".into(), dir: PathBuf::from("/o") };
        let mut st = AccountState { read_at: Some(now_us - 1), ..Default::default() };
        st.meters
            .insert("openai-codex/7 days".into(), Meter::named("openai-codex/7 days", 0.2, Some(now + 86_400 * 5)));
        assert!(fit(&omp, Some(&st), Some("openai-codex/gpt-5.2"), &Limits::default(), now_us).is_ok());
        assert!(matches!(
            fit(&omp, Some(&st), Some("anthropic/x"), &Limits::default(), now_us),
            Err(Refusal::Blind(_))
        ));
        assert!(matches!(fit(&omp, Some(&st), None, &Limits::default(), now_us), Err(Refusal::Blind(_))));
    }

    #[test]
    fn time_left_is_in_days_hours_or_minutes() {
        for (secs, left) in [(0, "0m"), (59, "1m"), (3_420, "57m"), (3_600, "1h"), (10 * 3600, "10h")] {
            assert_eq!(fmt_left(secs), left, "{secs}");
        }
        for (secs, left) in [(86_400, "1d"), (2 * 86_400 + 600, "2d"), (41 * 3600, "1d17h"), (149 * 3600, "6d5h")] {
            assert_eq!(fmt_left(secs), left, "{secs}");
        }
        // From ten days out the hours are dropped, so a time left is never wider than five columns.
        for (secs, left) in [(9 * 86_400 + 23 * 3600, "9d23h"), (10 * 86_400, "10d"), (29 * 86_400 + 23 * 3600, "30d")]
        {
            assert_eq!(fmt_left(secs), left, "{secs}");
        }
        assert!((0..400).map(|d| fmt_left(d * 86_400 + 13 * 3600).len()).all(|n| n <= 5));
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
