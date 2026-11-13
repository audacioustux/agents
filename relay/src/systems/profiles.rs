//! profiles: read-only. The owner's policy as relay resolved it: each profile's
//! fields, and where every merged setting came from.

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use serde_json::json;

use crate::config::{Config, Profile};
use crate::reply::reply;
use crate::systems::view::table;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Cmd {
    /// The merged policy and project config's profiles and bans, and which file each came from.
    Show,
}

/// `project` is the config file read, `None` when only the owner's policy was.
pub fn run(cfg: &Config, project: Option<&Path>, json: bool, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show => show(cfg, project, json),
    }
}

/// The model's ban, if any: what a profile resolving to it would be refused with.
fn ban<'a>(cfg: &'a Config, p: &Profile) -> Option<&'a str> {
    p.model.as_deref().and_then(|m| cfg.session.banned(m))
}

/// Each profile with its resolved fields and note, so an orchestrator can learn the policy.
pub fn list(cfg: &Config, json: bool) -> Result<()> {
    if json {
        let profiles: serde_json::Map<_, _> = cfg
            .profiles
            .iter()
            .map(|(name, p)| {
                let mut v = serde_json::to_value(p).unwrap_or_default();
                // Resolved: unset, a profile's jobs compile.
                v["compiles"] = p.compiles.unwrap_or(true).into();
                v["banned"] = ban(cfg, p).into();
                (name.clone(), v)
            })
            .collect();
        return reply(true, &json!({ "policy": cfg.policy_path, "profiles": profiles }), "");
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
                if p.compiles == Some(false) { "no compile".into() } else { "compiles".into() },
                limits.into_iter().flatten().collect::<Vec<_>>().join(", "),
            ]
        })
        .collect();
    for (p, line) in cfg.profiles.values().zip(table(&rows)) {
        out!("{line}");
        if let Some(why) = ban(cfg, p) {
            out!("    banned: {why}");
        }
        if let Some(check) = &p.check {
            let red = if p.check_red_first == Some(true) { " (red first)" } else { "" };
            out!("    check{red}: {check}");
        }
        if let (Some(to), Some(n)) = (&p.escalate_to, p.escalate_after) {
            out!("    escalates to {to} after {n} failed end(s)");
        }
        if p.independent.is_some() {
            out!("    reviews must come from another model family");
        }
        if let Some(note) = &p.note {
            out!("    {note}");
        }
    }
    Ok(())
}

fn show(cfg: &Config, project: Option<&Path>, json: bool) -> Result<()> {
    let from = |key: &str| cfg.origin(key).as_str();
    if json {
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
            "project": project,
            "machine": cfg.machine,
            "banned_models": banned,
            "profiles": profiles,
        });
        return reply(true, &doc, "");
    }
    out!("policy   {}", cfg.policy_path.as_ref().map_or_else(|| "none".into(), |p| p.display().to_string()));
    out!("project  {}", project.map_or_else(|| "none".into(), |p| p.display().to_string()));
    let caps = cfg.machine.fields();
    if !caps.is_empty() {
        out!("machine");
        for (field, value) in caps {
            out!("  {field} = {value}  [policy]");
        }
        if project.is_some() && cfg.machine.min_free_disk_gb.is_some() {
            let floor = cfg.limits.min_free_disk_gb;
            out!("  limits.min_free_disk_gb = {floor}  [{}]", from("limits.min_free_disk_gb"));
        }
    }
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
