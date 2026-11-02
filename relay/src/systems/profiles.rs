//! profiles: read-only. The owner's policy as relay resolved it: each profile's
//! fields, and where every merged setting came from.

use anyhow::Result;
use clap::Subcommand;
use serde_json::json;

use crate::config::{Config, Profile};
use crate::ctx::Ctx;
use crate::systems::view::table;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Cmd {
    /// The merged policy and project config's profiles and bans, and which file each came from.
    Show,
}

pub fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show => show(ctx),
    }
}

/// The model's ban, if any: what a profile resolving to it would be refused with.
fn ban<'a>(cfg: &'a Config, p: &Profile) -> Option<&'a str> {
    p.model.as_deref().and_then(|m| cfg.session.banned(m))
}

/// Each profile with its resolved fields and note, so an orchestrator can learn the policy.
pub fn list(ctx: &Ctx) -> Result<()> {
    let cfg = &ctx.cfg;
    if ctx.json {
        let profiles: serde_json::Map<_, _> = cfg
            .profiles
            .iter()
            .map(|(name, p)| {
                let mut v = serde_json::to_value(p).unwrap_or_default();
                v["banned"] = ban(cfg, p).into();
                (name.clone(), v)
            })
            .collect();
        return ctx.reply(&json!({ "policy": cfg.policy_path, "profiles": profiles }), "");
    }
    if cfg.profiles.is_empty() {
        out!("no profiles: add [profiles.NAME] to the project's relay.toml or to the policy file");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = cfg
        .profiles
        .iter()
        .map(|(name, p)| {
            let limits =
                [p.max_running.map(|n| format!("max {n} running")), p.max_per_day.map(|n| format!("max {n} per day"))];
            vec![
                name.clone(),
                p.harness.clone().unwrap_or_else(|| "-".into()),
                p.model.clone().unwrap_or_else(|| "-".into()),
                p.effort.clone().unwrap_or_default(),
                p.budget.map(|b| format!("${b}")).unwrap_or_default(),
                match p.readonly {
                    Some(true) => "read-only".into(),
                    Some(false) => "build".into(),
                    None => String::new(),
                },
                limits.into_iter().flatten().collect::<Vec<_>>().join(", "),
            ]
        })
        .collect();
    for (p, line) in cfg.profiles.values().zip(table(&rows)) {
        out!("{line}");
        if let Some(why) = ban(cfg, p) {
            out!("    banned: {why}");
        }
        if let Some(note) = &p.note {
            out!("    {note}");
        }
    }
    Ok(())
}

fn show(ctx: &Ctx) -> Result<()> {
    let cfg = &ctx.cfg;
    let from = |key: &str| cfg.origin(key).as_str();
    if ctx.json {
        let banned: serde_json::Map<_, _> = cfg
            .session
            .banned_models
            .iter()
            .map(|(re, why)| {
                (re.clone(), json!({ "reason": why, "from": from(&format!("session.banned_models.{re}")) }))
            })
            .collect();
        let profiles: serde_json::Map<_, _> = cfg
            .profiles
            .iter()
            .map(|(name, p)| {
                let fields: serde_json::Map<_, _> = serde_json::to_value(p)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| {
                        let origin = from(&format!("profiles.{name}.{k}"));
                        (k, json!({ "value": v, "from": origin }))
                    })
                    .collect();
                (name.clone(), fields.into())
            })
            .collect();
        let doc = json!({
            "policy": cfg.policy_path,
            "project": ctx.cfg_path,
            "banned_models": banned,
            "profiles": profiles,
        });
        return ctx.reply(&doc, "");
    }
    out!("policy   {}", cfg.policy_path.as_ref().map_or_else(|| "none".into(), |p| p.display().to_string()));
    out!("project  {}", ctx.cfg_path.display());
    if !cfg.session.banned_models.is_empty() {
        out!("banned models");
        for (re, why) in &cfg.session.banned_models {
            out!("  {re}  {why}  [{}]", from(&format!("session.banned_models.{re}")));
        }
    }
    for (name, p) in &cfg.profiles {
        out!("profile {name}");
        for (field, value) in p.fields() {
            out!("  {field} = {value}  [{}]", from(&format!("profiles.{name}.{field}")));
        }
    }
    Ok(())
}
