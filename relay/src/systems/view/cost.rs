//! `relay view cost`: the API-equivalent cost of every Claude account on this machine, by local day or
//! week. The accounts are subscriptions; this is what the same tokens would cost at the list prices
//! the owner's policy declares (`[prices]`). It is folded from the transcripts the Claude homes already
//! write, through an incremental index ([`index`]); it never runs `claude`.

pub mod index;

use std::collections::BTreeMap;

use anyhow::Result;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::json;

use crate::config::{Policy, Prices};
use index::{CLASSES, Dims, Index, Refreshed, Tokens, WEB_FETCH, WEB_SEARCH};

/// How rows are grouped in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Day,
    Week,
}

/// What a row is split by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Account,
    Project,
    Model,
}

/// Which rows count.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub accounts: Vec<String>,
    pub projects: Vec<String>,
    pub models: Vec<String>,
}

impl Filter {
    fn admits(&self, d: &Dims) -> bool {
        let any = |want: &[String], f: &dyn Fn(&str) -> bool| want.is_empty() || want.iter().any(|w| f(w));
        any(&self.accounts, &|w| w == d.home)
            && any(&self.projects, &|w| w == d.project)
            && any(&self.models, &|w| w == d.model || w == super::short_model(&d.model))
    }
}

/// `claude-opus-5-5` is `opus`; an id that is not a Claude model is itself.
pub fn family(model: &str) -> String {
    match model.strip_prefix("claude-") {
        Some(rest) => rest.split('-').next().unwrap_or(rest).to_owned(),
        None => model.to_owned(),
    }
}

/// A row's dollars at the list prices, `None` when its model has no price (its web searches aside,
/// which are priced machine-wide).
fn usd(prices: &Prices, d: &Dims, t: &Tokens) -> (Option<f64>, f64) {
    #[allow(clippy::cast_precision_loss, reason = "token counts are far below 2^52")]
    let f = |n: u64| n as f64;
    let web = prices.web_search_per_1000.map_or(0.0, |p| f(t[WEB_SEARCH]) * p / 1000.0);
    let tokens = prices.models.get(&d.model).map(|p| {
        let speed = if d.fast { p.fast } else { 1.0 };
        let rates = [p.input, p.output, p.cache_read, p.cache_write_5m, p.cache_write_1h];
        rates.iter().zip(t).map(|(r, n)| r * f(*n)).sum::<f64>() / 1e6 * speed
    });
    (tokens, web)
}

/// Every token of a row that a price applies to (not the web requests).
fn token_sum(t: &Tokens) -> u64 {
    t[..WEB_SEARCH].iter().sum()
}

/// The dollars of a group of rows: the total, per model family, per split key.
#[derive(Debug, Clone, Default)]
struct Sum {
    usd: f64,
    families: BTreeMap<String, f64>,
    split: BTreeMap<String, (f64, BTreeMap<String, f64>)>,
}

impl Sum {
    fn add(&mut self, family: &str, key: Option<&str>, usd: f64) {
        self.usd += usd;
        *self.families.entry(family.to_owned()).or_default() += usd;
        if let Some(k) = key {
            let s = self.split.entry(k.to_owned()).or_default();
            s.0 += usd;
            *s.1.entry(family.to_owned()).or_default() += usd;
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let cents = |x: f64| (x * 100.0).round() / 100.0;
        let fam = |m: &BTreeMap<String, f64>| {
            m.iter().map(|(k, v)| (k.clone(), json!(cents(*v)))).collect::<serde_json::Map<_, _>>()
        };
        let mut v = json!({ "usd": cents(self.usd), "families": fam(&self.families) });
        if !self.split.is_empty() {
            let mut split: Vec<_> = self.split.iter().collect();
            split.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
            v["split"] = split
                .into_iter()
                .map(|(k, (u, f))| json!({ "key": k, "usd": cents(*u), "families": fam(f) }))
                .collect::<Vec<_>>()
                .into();
        }
        v
    }
}

/// The index folded at `today` in `tz`: today, this week, and each period of the window.
#[derive(Debug, Default)]
pub struct Folded {
    pub today: f64,
    pub week: f64,
    today_sum: Sum,
    week_sum: Sum,
    /// Each period's first day, newest first, with its sum.
    periods: Vec<(Date, Sum)>,
    /// Tokens per class over the window.
    tokens: Tokens,
    /// Over the window: tokens of models without a price, per model.
    unpriced: BTreeMap<String, u64>,
    /// Web searches over the window, when no price is declared for them.
    unpriced_searches: u64,
    /// The top family this week, with its dollars.
    pub top: Option<(String, f64)>,
    /// Over the window: each relay job's dollars (attributed by its clone's path), by project and job.
    jobs: BTreeMap<(String, String), f64>,
}

/// The Monday of `d`'s week.
fn monday(d: Date) -> Date {
    d.checked_sub(jiff::Span::new().days(i64::from(d.weekday().to_monday_zero_offset()))).unwrap_or(d)
}

/// Folds the index's rows that `filter` admits, at `today` in `tz`, over `n` periods of `by`.
pub fn fold(
    index: &Index,
    prices: &Prices,
    tz: &TimeZone,
    today: Date,
    (by, n): (By, usize),
    filter: &Filter,
    split: Option<Split>,
) -> Folded {
    let period_of = |d: Date| if by == By::Week { monday(d) } else { d };
    let step = if by == By::Week { 7 } else { 1 };
    let first = period_of(today);
    let periods: Vec<Date> =
        (0..n).filter_map(|i| first.checked_sub(jiff::Span::new().days(i64::try_from(i * step).ok()?)).ok()).collect();
    let oldest = periods.last().copied().unwrap_or(first);
    let mut sums: BTreeMap<Date, Sum> = periods.iter().map(|d| (*d, Sum::default())).collect();
    let mut out = Folded::default();
    // One conversion per hour, however many rows share it.
    let mut dates: std::collections::HashMap<i64, Date> = std::collections::HashMap::new();
    for (d, t) in index.rows().filter(|(d, _)| filter.admits(d)) {
        let date = *dates.entry(d.hour).or_insert_with(|| {
            jiff::Timestamp::from_second(d.hour * 3600).map_or(today, |ts| ts.to_zoned(tz.clone()).date())
        });
        let (tokens, web) = usd(prices, d, t);
        let fam = family(&d.model);
        let key = split.map(|s| match s {
            Split::Account => d.home.clone(),
            Split::Project => d.project.clone(),
            Split::Model => d.model.clone(),
        });
        let dollars = tokens.unwrap_or(0.0) + web;
        if date == today {
            out.today_sum.add(&fam, key.as_deref(), dollars);
        }
        if monday(date) == monday(today) {
            out.week_sum.add(&fam, key.as_deref(), dollars);
        }
        if date < oldest {
            continue;
        }
        if let Some(s) = sums.get_mut(&period_of(date)) {
            s.add(&fam, key.as_deref(), dollars);
            if let Some(job) = &d.job {
                *out.jobs.entry((d.project.clone(), job.clone())).or_default() += dollars;
            }
            out.tokens.iter_mut().zip(t).for_each(|(a, b)| *a += b);
            if tokens.is_none() && token_sum(t) > 0 {
                *out.unpriced.entry(d.model.clone()).or_default() += token_sum(t);
            }
            if prices.web_search_per_1000.is_none() {
                out.unpriced_searches += t[WEB_SEARCH];
            }
        }
    }
    (out.today, out.week) = (out.today_sum.usd, out.week_sum.usd);
    out.top = out.week_sum.families.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(k, v)| (k.clone(), *v));
    out.periods = periods.into_iter().map(|d| (d, sums.remove(&d).unwrap_or_default())).collect();
    out
}

/// Whether every offset `tz` takes is whole hours, so UTC-hour buckets fall exactly on its days.
fn whole_hours(tz: &TimeZone, now: jiff::Timestamp) -> bool {
    tz.to_offset(now).seconds() % 3600 == 0
}

/// Where the index lives: `$XDG_STATE_HOME/relay/cost`.
pub fn index_dir() -> std::path::PathBuf {
    crate::ctx::state_root().join("cost")
}

/// Refreshes the machine's index from every Claude home under the user's home.
pub fn refresh() -> Result<(Index, Refreshed)> {
    let homes = index::homes(&crate::accounts::home());
    index::refresh(&index_dir(), &homes, &crate::ctx::data_root())
}

/// What the watch shows: today and this week, and this week's top family, at list prices.
pub fn summary(index: &Index, prices: &Prices) -> String {
    let tz = TimeZone::system();
    let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
    let f = fold(index, prices, &tz, today, (By::Day, 1), &Filter::default(), None);
    let top = f.top.filter(|(_, u)| *u > 0.0).map(|(fam, usd)| format!(" · top {fam} ${usd:.2}"));
    format!("API-equivalent cost: today ${:.2} · week ${:.2}{}", f.today, f.week, top.unwrap_or_default())
}

#[derive(Debug)]
pub struct Args {
    pub by: By,
    pub since: Option<String>,
    pub filter: Filter,
    pub split: Option<Split>,
}

pub fn run(json: bool, args: &Args) -> Result<()> {
    let n = match &args.since {
        None => {
            if args.by == By::Week {
                8
            } else {
                7
            }
        }
        Some(s) => match super::stats::parse_duration(s) {
            Some(us) => {
                let unit: u64 = if args.by == By::Week { 7 * 86_400_000_000 } else { 86_400_000_000 };
                usize::try_from(us.div_ceil(unit)).unwrap_or(usize::MAX).min(3660)
            }
            None => fail!(Usage, "--since {s}: a duration such as 30m, 12h, 7d or 2w"),
        },
    };
    let prices = Policy::load()?.map(|(p, _)| p.prices().clone()).unwrap_or_default();
    let (index, refreshed) = refresh()?;
    let tz = TimeZone::system();
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(tz.clone()).date();
    let f = fold(&index, &prices, &tz, today, (args.by, n), &args.filter, args.split);
    let zone = tz.iana_name().map_or_else(|| now.to_zoned(tz.clone()).strftime("%Z").to_string(), str::to_owned);
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&to_json(
                &index,
                &f,
                args,
                &prices,
                &refreshed,
                (&zone, whole_hours(&tz, now))
            ))?
        );
        return Ok(());
    }
    let mut lines = vec![format!(
        "API-equivalent cost: what these tokens would cost at the policy's list prices (the accounts are subscriptions); days in {zone}"
    )];
    lines.extend(text(&f, args.by));
    if !f.unpriced.is_empty() {
        let n: u64 = f.unpriced.values().sum();
        let models: Vec<&str> = f.unpriced.keys().map(String::as_str).collect();
        lines.push(format!("+ {n} unpriced tokens (models: {}): no price in the policy's [prices]", models.join(", ")));
    }
    if f.unpriced_searches > 0 {
        lines.push(format!("+ {} unpriced web searches: no prices.web_search_per_1000", f.unpriced_searches));
    }
    let c = index.counters();
    lines.push(format!(
        "counted once each: {} responses; {} repeated lines dropped, {} of them disagreeing",
        c.messages, c.copies_dropped, c.disagreements
    ));
    let (files, _) = index.sizes();
    let changed = index
        .changed_at()
        .map_or_else(|| "never".into(), |t| crate::os::local(t).strftime("%Y-%m-%d %H:%M:%S %Z").to_string());
    let busy = if refreshed.busy { "; another refresh is running: this is its last snapshot" } else { "" };
    lines.push(format!("index: {files} transcripts, last changed {changed}{busy}"));
    if !whole_hours(&tz, now) {
        lines.push(format!("{zone} is not a whole number of hours from UTC: hours are bucketed in UTC, so a day's edge is off by its minutes"));
    }
    out!("{}", lines.join("\n"));
    Ok(())
}

/// The rows as text: today, this week, then each period, with a column per model family.
fn text(f: &Folded, by: By) -> Vec<String> {
    let mut fams: BTreeMap<&str, f64> = BTreeMap::new();
    for s in f.periods.iter().map(|(_, s)| s).chain([&f.today_sum, &f.week_sum]) {
        s.families.iter().for_each(|(k, v)| *fams.entry(k).or_default() += v);
    }
    let mut fams: Vec<(&str, f64)> = fams.into_iter().filter(|(_, v)| *v > 0.0).collect();
    fams.sort_by(|a, b| b.1.total_cmp(&a.1));
    let money = |x: f64| format!("${x:.2}");
    let cells = |label: String, s: &Sum| -> Vec<Vec<String>> {
        let row = |label: String, usd: f64, by_fam: &BTreeMap<String, f64>| {
            std::iter::once(label)
                .chain(std::iter::once(money(usd)))
                .chain(fams.iter().map(|(k, _)| by_fam.get(*k).map_or_else(|| "-".into(), |v| money(*v))))
                .collect::<Vec<_>>()
        };
        let mut rows = vec![row(label, s.usd, &s.families)];
        let mut split: Vec<_> = s.split.iter().collect();
        split.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        rows.extend(split.into_iter().map(|(k, (u, fm))| row(format!("  {k}"), *u, fm)));
        rows
    };
    let mut rows = vec![
        std::iter::once(String::new())
            .chain(std::iter::once("total".into()))
            .chain(fams.iter().map(|(k, _)| (*k).to_owned()))
            .collect(),
    ];
    rows.extend(cells("today".into(), &f.today_sum));
    rows.extend(cells("this week".into(), &f.week_sum));
    rows.push(Vec::new());
    for (d, s) in &f.periods {
        let label = match by {
            By::Day => d.strftime("%Y-%m-%d %a").to_string(),
            By::Week => format!("week of {}", d.strftime("%Y-%m-%d")),
        };
        rows.extend(cells(label, s));
    }
    super::table(&rows)
}

fn to_json(
    index: &Index,
    f: &Folded,
    args: &Args,
    prices: &Prices,
    refreshed: &Refreshed,
    (zone, whole): (&str, bool),
) -> serde_json::Value {
    let tokens: serde_json::Map<_, _> =
        CLASSES.iter().zip(f.tokens).map(|(c, n)| ((*c).to_owned(), json!(n))).collect();
    let rows: Vec<_> = f
        .periods
        .iter()
        .map(|(d, s)| {
            let mut v = s.to_json();
            v["start"] = d.to_string().into();
            v
        })
        .collect();
    let c = index.counters();
    let (files, keys) = index.sizes();
    json!({
        "basis": "API-equivalent: the list prices in the policy's [prices]; the accounts are subscriptions",
        "by": args.by,
        "since": args.since,
        "zone": zone,
        "whole_hour_zone": whole,
        "today": f.today_sum.to_json(),
        "this_week": f.week_sum.to_json(),
        "rows": rows,
        "tokens": tokens,
        "web_fetch_requests": f.tokens[WEB_FETCH],
        "unpriced": f.unpriced.iter().map(|(m, n)| json!({ "model": m, "tokens": n })).collect::<Vec<_>>(),
        "unpriced_web_searches": f.unpriced_searches,
        "jobs": f.jobs.iter().map(|((p, j), u)| json!({ "project": p, "job": j, "usd": (u * 100.0).round() / 100.0 })).collect::<Vec<_>>(),
        "priced_models": prices.models.keys().collect::<Vec<_>>(),
        "dedup": { "messages": c.messages, "copies_dropped": c.copies_dropped, "disagreements": c.disagreements, "synthetic": c.synthetic, "unreadable": c.unreadable },
        "index": {
            "path": index_dir(),
            "transcripts": files,
            "keys": keys,
            "changed_at": index.changed_at().map(|t| t.to_string()),
            "refresh": refreshed,
        },
    })
}

#[cfg(test)]
mod tests;
