//! Which accounts exist and which has the most room. Directories are found on
//! disk each time and never written to the bus: events name accounts by label.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::bus::Windows;
use crate::config::Config;
use crate::world::AccountState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub label: String,
    pub dir: PathBuf,
}

/// A reading older than this is refreshed before it is trusted.
pub const STALE_MICROS: u64 = 10 * 60 * 1_000_000;

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Every usable account, in label order. Two config dirs signed in to the same
/// person are one account (the first label wins), since they share one quota.
pub fn discover(cfg: &Config) -> Vec<Account> {
    discover_in(&home(), cfg)
}

pub fn discover_in(home: &Path, cfg: &Config) -> Vec<Account> {
    let prefix = &cfg.accounts.prefix;
    let mut entries: Vec<_> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| label_for(&name, prefix).map(|l| (l, home.join(&name))))
        .filter(|(l, dir)| dir.is_dir() && !cfg.accounts.exclude.contains(l))
        .collect();
    entries.sort();
    let mut seen = BTreeSet::new();
    entries
        .into_iter()
        .filter(|(_, dir)| can_refresh(dir))
        .filter(|(_, dir)| identity(dir).is_none_or(|id| seen.insert(id)))
        .map(|(label, dir)| Account { label, dir })
        .collect()
}

/// `.claude` is `default`; `.claude.work` is `work`.
pub fn label_for(entry: &str, prefix: &str) -> Option<String> {
    let rest = entry.strip_prefix(prefix)?;
    if rest.is_empty() {
        return Some("default".into());
    }
    let label = rest.strip_prefix('.')?;
    (!label.is_empty()).then(|| label.to_owned())
}

/// Every `$HOME` entry that is or belongs to an account dir, excluded ones too.
/// The sandbox hides all of them except the session's own.
pub fn account_like_entries(home: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect()
}

/// A sign-in without a refresh token expires within hours, so it is not offered.
fn can_refresh(dir: &Path) -> bool {
    read_json(&dir.join(".credentials.json")).is_some_and(|v| {
        v.pointer("/claudeAiOauth/refreshToken").and_then(|t| t.as_str()).is_some_and(|t| !t.is_empty())
    })
}

/// Who the dir is signed in as. Used only to compare dirs; never printed.
fn identity(dir: &Path) -> Option<String> {
    read_json(&dir.join(".claude.json"))?
        .pointer("/oauthAccount/emailAddress")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Utilization of the fullest window that has not reset yet.
pub fn pressure(windows: &Windows, now_s: i64) -> f64 {
    windows.values().filter(|w| w.resets_at.is_none_or(|r| r > now_s)).map(|w| w.utilization).fold(0.0, f64::max)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Disabled,
    SignedOut,
    Limited,
    AtFloor,
    Unread,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SignedOut => "signed out (sign in, then: relay quota enable)",
            Self::Limited => "limited",
            Self::AtFloor => "at floor",
            Self::Unread => "not read yet",
        }
    }
}

/// Why the account cannot take a session now, if it cannot.
pub fn refusal(state: Option<&AccountState>, floor: f64, now_s: i64) -> Option<Refusal> {
    let Some(a) = state else { return Some(Refusal::Unread) };
    if !a.enabled {
        Some(Refusal::Disabled)
    } else if a.signed_out {
        Some(Refusal::SignedOut)
    } else if a.limited_until.is_some_and(|u| u > now_s) {
        Some(Refusal::Limited)
    } else if a.read_at.is_none() {
        Some(Refusal::Unread)
    } else if pressure(&a.windows, now_s) >= floor {
        Some(Refusal::AtFloor)
    } else {
        None
    }
}

/// Whether the reading should be taken again: never read, old, or past a reset it names.
pub fn is_stale(state: Option<&AccountState>, now_us: u64) -> bool {
    let Some(a) = state else { return true };
    let now_s = i64::try_from(now_us / 1_000_000).unwrap_or(i64::MAX);
    let Some(read_us) = a.read_at else { return true };
    let read_s = i64::try_from(read_us / 1_000_000).unwrap_or(i64::MAX);
    now_us.saturating_sub(read_us) > STALE_MICROS
        || a.windows.values().filter_map(|w| w.resets_at).any(|r| read_s < r && r <= now_s)
}

/// Higher is better: weekly room left per day until it resets, since quota
/// still unused at a reset is lost. An unknown reset counts as a week away.
pub fn score(windows: &Windows, now_s: i64) -> f64 {
    let week = windows.get("seven_day");
    let left = 1.0 - week.map_or_else(|| pressure(windows, now_s), |w| w.utilization);
    #[allow(clippy::cast_precision_loss)]
    let days = week.and_then(|w| w.resets_at).map_or(7.0, |r| (r - now_s) as f64 / 86_400.0).max(1.0);
    left / days
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Window;
    use crate::config::tempdir::Dir;

    fn win(u: f64, r: Option<i64>) -> Window {
        Window { utilization: u, resets_at: r }
    }

    #[test]
    fn labels() {
        assert_eq!(label_for(".claude", ".claude").as_deref(), Some("default"));
        assert_eq!(label_for(".claude.work", ".claude").as_deref(), Some("work"));
        assert_eq!(label_for(".claude.json", ".claude").as_deref(), Some("json"));
        assert_eq!(label_for(".claudex", ".claude"), None);
        assert_eq!(label_for(".claude.", ".claude"), None);
    }

    #[test]
    fn discovery_needs_a_refresh_token_and_merges_same_identity() {
        let home = Dir::new();
        let mk = |name: &str, token: &str, email: &str| {
            let d = home.path().join(name);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join(".credentials.json"), format!(r#"{{"claudeAiOauth":{{"refreshToken":"{token}"}}}}"#))
                .unwrap();
            std::fs::write(d.join(".claude.json"), format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}}}}"#))
                .unwrap();
        };
        mk(".claude.a", "r", "one@x");
        mk(".claude.b", "r", "ONE@x");
        mk(".claude.c", "", "two@x");
        mk(".claude.d", "r", "three@x");
        mk(".claude.other", "r", "four@x");
        std::fs::write(home.path().join(".claude.json"), "{}").unwrap(); // a file, not an account
        let cfg = Config {
            accounts: crate::config::Accounts { exclude: vec!["other".into()], ..Default::default() },
            ..test_config()
        };
        let labels: Vec<_> = discover_in(home.path(), &cfg).into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["a", "d"]);
    }

    #[test]
    fn refusals() {
        let now = 1_000;
        let mut a = AccountState { read_at: Some(1), ..Default::default() };
        assert_eq!(refusal(None, 0.9, now), Some(Refusal::Unread));
        assert_eq!(refusal(Some(&a), 0.9, now), None);
        a.windows.insert("seven_day".into(), win(0.95, Some(now + 10)));
        assert_eq!(refusal(Some(&a), 0.9, now), Some(Refusal::AtFloor));
        a.windows.insert("seven_day".into(), win(0.95, Some(now - 10)));
        assert_eq!(refusal(Some(&a), 0.9, now), None, "a window past its reset no longer counts");
        a.limited_until = Some(now + 1);
        assert_eq!(refusal(Some(&a), 0.9, now), Some(Refusal::Limited));
    }

    #[test]
    fn staleness() {
        let now_us = 2_000 * 1_000_000;
        let mut a = AccountState { read_at: Some(now_us - 100 * 1_000_000), ..Default::default() };
        assert!(!is_stale(Some(&a), now_us));
        a.windows.insert("seven_day".into(), win(1.0, Some(1_850)));
        assert!(!is_stale(Some(&a), now_us), "read after that reset");
        a.windows.insert("seven_day".into(), win(1.0, Some(1_950)));
        assert!(is_stale(Some(&a), now_us), "read before a reset that has now passed");
        a.read_at = Some(now_us - STALE_MICROS - 1);
        a.windows.clear();
        assert!(is_stale(Some(&a), now_us));
    }

    #[test]
    fn score_prefers_room_that_would_otherwise_be_lost() {
        let now = 0;
        let soon: Windows = [("seven_day".to_owned(), win(0.5, Some(86_400)))].into();
        let later: Windows = [("seven_day".to_owned(), win(0.4, Some(6 * 86_400)))].into();
        assert!(score(&soon, now) > score(&later, now));
    }

    pub fn test_config() -> Config {
        let d = Dir::new();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg = Config::parse("name = \"t\"", d.path()).unwrap();
        std::mem::forget(d);
        cfg
    }
}
