use jiff::tz::{Offset, TimeZone};

use super::index::tests::{Machine, line, usage};
use super::*;

/// The policy example's `[prices]` block, uncommented.
fn example_prices() -> Prices {
    let text = include_str!("../../../../relay.example.toml");
    let start = text.find("#   [prices]").expect("the example declares prices");
    let block: Vec<&str> = text[start..].lines().take_while(|l| l.starts_with("#   ")).map(|l| &l[4..]).collect();
    let block = block.join("\n");
    Policy::parse(&block).unwrap().prices().clone()
}

const M: u64 = 1_000_000;

/// In UTC-5: one opus response late on Sunday 15 November (local), early Monday 16 November in UTC;
/// and on Monday 16 November local, a sonnet one, a fast opus one with 1000 searches, and one of a
/// model without a price.
fn machine() -> (Machine, Index) {
    let m = Machine::new();
    let cwd = "/work/demo";
    let mut s = line("a", "ra", "claude-opus-5-5", "2026-11-16T03:30:00Z", cwd, &usage(M, 0, 0, 0, 0));
    s += &line("b", "rb", "claude-sonnet-5-5", "2026-11-16T06:00:00Z", cwd, &usage(0, M, 0, 0, 0));
    let mut fast = usage(0, M, 0, 0, 0);
    fast["speed"] = "fast".into();
    fast["server_tool_use"]["web_search_requests"] = 1000.into();
    s += &line("c", "rc", "claude-opus-5-5", "2026-11-16T07:00:00Z", "/work/other", &fast);
    s += &line("d", "rd", "claude-fable-5-1", "2026-11-16T08:00:00Z", cwd, &usage(5, 5, 0, 0, 0));
    m.write(".claude", "-work-demo/s.jsonl", &s);
    let (index, _) = m.refresh();
    (m, index)
}

fn utc_minus_5() -> TimeZone {
    TimeZone::fixed(Offset::constant(-5))
}

fn monday() -> Date {
    jiff::civil::date(2026, 11, 16)
}

#[test]
fn the_example_prices_parse_and_price_each_class() {
    let p = example_prices();
    assert_eq!(p.models.len(), 4);
    assert_eq!(p.web_search_per_1000, Some(10.0));
    let opus = p.models["claude-opus-5-5"];
    assert_eq!(
        (opus.input, opus.output, opus.cache_write_5m, opus.cache_write_1h, opus.cache_read, opus.fast),
        (4.0, 20.0, 5.0, 8.0, 0.2, 2.0)
    );
    assert_eq!(p.models["claude-sonnet-5-5"].fast, 1.0, "no fast mode: the factor is 1");
    let d = |model: &str, fast| Dims {
        hour: 0,
        home: "h".into(),
        project: "p".into(),
        job: None,
        model: model.into(),
        fast,
    };
    let (t, web) = usd(&p, &d("claude-haiku-5-5", false), &[M, M, M, M, M, 0, 0]);
    assert!((t.unwrap() - (0.10 + 0.50 + 0.01 + 0.125 + 0.20)).abs() < 1e-9);
    assert_eq!(web, 0.0);
    assert!(
        Policy::parse(
            "[prices.models.x]\ninput = -1\noutput = 1\ncache_write_5m = 1\ncache_write_1h = 1\ncache_read = 1"
        )
        .is_err()
    );
    assert!(Policy::parse("[prices.models.x]\ninput = 1").is_err(), "a row declares every price");
}

#[test]
fn days_and_weeks_follow_the_local_zone() {
    let (_m, index) = machine();
    let p = example_prices();
    let tz = utc_minus_5();
    let f = fold(&index, &p, &tz, monday(), (By::Day, 7), &Filter::default(), None);
    // Monday: sonnet $10, fast opus $40 and 1000 searches $10; Sunday (local): opus $4.
    assert!((f.today - 60.0).abs() < 1e-9, "{f:?}");
    assert!((f.week - 60.0).abs() < 1e-9, "Sunday was last week: {f:?}");
    assert_eq!(f.periods.len(), 7);
    assert_eq!(f.periods[0].0, monday());
    assert_eq!(f.periods[1].0, jiff::civil::date(2026, 11, 15));
    assert!((f.periods[1].1.usd - 4.0).abs() < 1e-9);
    assert!((f.periods[0].1.families["opus"] - 50.0).abs() < 1e-9, "fast opus and its searches");
    assert_eq!(f.unpriced.get("claude-fable-5-1"), Some(&10), "shown, never priced");
    assert_eq!(f.top.as_ref().map(|(k, _)| k.as_str()), Some("opus"));
    let w = fold(&index, &p, &tz, monday(), (By::Week, 8), &Filter::default(), None);
    assert_eq!(w.periods[1].0, jiff::civil::date(2026, 11, 9), "weeks start on Monday");
    assert!((w.periods[0].1.usd - 60.0).abs() < 1e-9 && (w.periods[1].1.usd - 4.0).abs() < 1e-9);
    // In UTC, all four are on Monday.
    let utc = fold(&index, &p, &TimeZone::UTC, monday(), (By::Day, 7), &Filter::default(), None);
    assert!((utc.today - 64.0).abs() < 1e-9);
    assert!(!whole_hours(&TimeZone::fixed(Offset::from_seconds(19_800).unwrap()), jiff::Timestamp::now()));
    assert!(whole_hours(&tz, jiff::Timestamp::now()));
}

#[test]
fn filters_and_splits() {
    let (_m, index) = machine();
    let p = example_prices();
    let tz = utc_minus_5();
    let only = |f: Filter| fold(&index, &p, &tz, monday(), (By::Day, 7), &f, None).today;
    assert!((only(Filter { models: vec!["sonnet-5.5".into()], ..Filter::default() }) - 10.0).abs() < 1e-9);
    assert!((only(Filter { projects: vec!["other".into()], ..Filter::default() }) - 50.0).abs() < 1e-9);
    assert_eq!(only(Filter { accounts: vec!["work".into()], ..Filter::default() }), 0.0);
    let f = fold(&index, &p, &tz, monday(), (By::Day, 7), &Filter::default(), Some(Split::Project));
    assert_eq!(f.today_sum.split.keys().collect::<Vec<_>>(), ["demo", "other"]);
    let lines = text(&f, By::Day);
    assert!(lines[0].contains("total") && lines[0].contains("opus") && lines[0].contains("sonnet"), "{lines:#?}");
    let today = lines.iter().position(|l| l.starts_with("today")).unwrap();
    assert!(lines[today].contains("$60.00"), "{lines:#?}");
    assert!(lines[today + 1].trim_start().starts_with("other") && lines[today + 1].contains("$50.00"), "{lines:#?}");
    let v = f.today_sum.to_json();
    assert_eq!(v["usd"], 60.0);
    assert_eq!(v["split"][0]["key"], "other");
}

#[test]
fn an_index_in_the_old_place_is_renamed_into_the_new_one_and_a_project_named_cost_keeps_its_directory() {
    let d = crate::config::tempdir::Dir::new();
    let state = d.path();
    let old = state.join("cost");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("index.bin"), b"the index").unwrap();
    std::fs::write(old.join("lock"), b"").unwrap();
    migrate(state).unwrap();
    assert_eq!(std::fs::read(index_dir_in(state).join("index.bin")).unwrap(), b"the index");
    assert!(!old.exists(), "nothing of the index is left behind");
    // Again, and with nothing to move: no change.
    migrate(state).unwrap();
    assert_eq!(std::fs::read(index_dir_in(state).join("index.bin")).unwrap(), b"the index");
    // A project named `cost` (its bus, its files) is not the index's directory, and is left as it is.
    std::fs::create_dir_all(old.join("bus")).unwrap();
    std::fs::write(old.join("index.bin"), b"older").unwrap();
    migrate(state).unwrap();
    assert_eq!(std::fs::read(index_dir_in(state).join("index.bin")).unwrap(), b"the index", "one already there wins");
    assert!(old.join("bus").is_dir() && old.join("index.bin").is_file());
    assert!(!index_dir_in(state).starts_with(&old));
    assert!(!crate::config::valid_name(".machine"), "a project cannot be named so");
}
