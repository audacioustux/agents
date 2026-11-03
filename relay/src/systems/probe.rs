//! probe: checks from the inside that the sandbox does what its mount plan
//! says, with controls from the outside proving the hidden paths exist, for
//! one account of every enabled harness (or the one named). Starts no model
//! session and emits nothing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::accounts::{self, harness_of};
use crate::ctx::Ctx;
use crate::harness::{Home, Invocation, Permissions, Session, Task};
use crate::sandbox::{self, Mount, Mounts};

pub fn run(ctx: &Ctx, account: Option<&str>) -> Result<()> {
    if !ctx.cfg.sandbox.enabled {
        fail!(Refused, "the sandbox is off in this config; nothing to probe");
    }
    let found = accounts::discover(&ctx.cfg);
    let homes: Vec<&Home> = if let Some(k) = account {
        let Some(home) = found.iter().find(|a| a.key() == k) else { fail!(NotFound, "unknown account {k}") };
        vec![home]
    } else {
        // The first account of each harness: the checks are the same for all.
        let mut seen = std::collections::BTreeSet::new();
        found.iter().filter(|a| seen.insert(a.harness)).collect()
    };
    if homes.is_empty() {
        fail!(NotFound, "no usable account");
    }
    let mut results = Vec::new();
    for home in homes {
        results.push((home.key(), Probe::new(ctx, home)?.run()?));
    }
    let failed = results.iter().flat_map(|(_, r)| r).filter(|r| !r.ok).count();
    let text: Vec<String> = results
        .iter()
        .flat_map(|(key, rs)| std::iter::once(format!("== {key}")).chain(rs.iter().map(Outcome::text)))
        .collect();
    let json: Vec<_> = results.iter().map(|(key, rs)| serde_json::json!({ "account": key, "checks": rs })).collect();
    ctx.reply(&serde_json::json!({ "accounts": json, "failed": failed }), &text.join("\n"))?;
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct Outcome {
    what: String,
    ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    detail: String,
}

impl Outcome {
    fn text(&self) -> String {
        let detail = if self.ok || self.detail.is_empty() { String::new() } else { format!("  {}", self.detail) };
        format!("{}  {}{detail}", if self.ok { "PASS" } else { "FAIL" }, self.what)
    }
}

/// One check: a shell test run inside the sandbox, and optionally one run
/// outside that must pass for the inside one to mean anything.
struct Check {
    what: String,
    inside: String,
    outside: Option<String>,
}

fn q(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

fn all(tests: impl IntoIterator<Item = String>) -> String {
    let t: Vec<String> = tests.into_iter().collect();
    if t.is_empty() { "true".into() } else { t.join(" && ") }
}

fn cache_marker() -> String {
    format!(".relay-probe-{}", std::process::id())
}

struct Probe<'a> {
    ctx: &'a Ctx,
    home: &'a Home,
    clone: PathBuf,
    neighbour: PathBuf,
    scratch: PathBuf,
    tmp: PathBuf,
}

impl<'a> Probe<'a> {
    fn new(ctx: &'a Ctx, home: &'a Home) -> Result<Self> {
        let id = std::process::id();
        let p = Self {
            ctx,
            home,
            clone: ctx.clones.join(format!("probe-{id}")),
            neighbour: ctx.clones.join(format!("probe-other-{id}")),
            scratch: ctx.job_scratch(&format!("probe-{id}")),
            tmp: ctx.job_scratch(&format!("probe-{id}")).join("tmp"),
        };
        for d in [&p.clone, &p.neighbour, &p.scratch, &p.tmp] {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p.neighbour.join("secret"), "x")?;
        Ok(p)
    }

    /// What a session on this account would run, for its program and variables.
    fn invocation(&self) -> Result<Invocation> {
        let id = uuid::Uuid::now_v7().to_string();
        let task = Task {
            prompt: "probe",
            model: None,
            effort: None,
            session: Session::New(&id),
            permissions: Permissions::Unrestricted,
            briefing: None,
            budget_usd: None,
            name: None,
            events: true,
            lean: true,
            scratch: &self.scratch,
        };
        let inv = harness_of(self.home).invocation(self.home, &task).map_err(|e| anyhow::anyhow!(e.0))?;
        Ok(self.ctx.cfg.harness(self.home.harness).locate(inv))
    }

    fn checks(&self, plan: &[(PathBuf, Mount)], inv: &Invocation) -> Vec<Check> {
        // A hidden path must hide what is inside it, except what a later mount shows again.
        let shown_later =
            |i: usize, p: &Path| plan[i + 1..].iter().any(|(m, k)| *k != Mount::Hidden && m.starts_with(p));
        let mut hidden = Vec::new();
        for (i, (p, _)) in plan.iter().enumerate().filter(|(_, (_, k))| *k == Mount::Hidden) {
            if p.is_file() {
                hidden.push(p.clone());
            } else {
                let inside = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
                hidden.extend(inside.filter(|e| !shown_later(i, e)).take(3));
            }
        }
        let paths = |want: fn(&Mount) -> bool| plan.iter().filter(move |(_, k)| want(k)).map(|(p, _)| p);
        let writable = paths(|k| matches!(k, Mount::Writable | Mount::From(_))).map(|p| format!("test -w {}", q(p)));
        let read_only = paths(|k| *k == Mount::ReadOnly).map(|p| format!("! test -w {}", q(p)));
        // Each cache shows relay's directory, which holds the marker, not the host's.
        let caches = plan.iter().filter_map(|(p, k)| match k {
            Mount::From(_) => Some(format!("test -e {}", q(&p.join(cache_marker())))),
            _ => None,
        });
        let repo = &self.ctx.cfg.repo;
        vec![
            Check {
                what: format!("{} hidden paths are hidden", hidden.len()),
                // A socket has no size either, so it is checked for by type.
                inside: all(hidden.iter().map(|p| format!("test ! -s {p} && test ! -S {p}", p = q(p)))),
                outside: Some(all(hidden.iter().map(|p| format!("test -e {}", q(p))))),
            },
            Check { what: "writable mounts are writable".into(), inside: all(writable), outside: None },
            Check {
                what: "read-only mounts (repository, harness config) are not writable".into(),
                inside: all(read_only),
                outside: None,
            },
            Check {
                what: "/tmp is relay's directory on disk (not RAM) and writable".into(),
                inside: format!("test -w /tmp && test -e /tmp/{}", cache_marker()),
                outside: None,
            },
            Check {
                what: "hidden directories are not writable".into(),
                inside: all(plan
                    .iter()
                    .filter(|(p, k)| *k == Mount::Hidden && p.is_dir())
                    .map(|(p, _)| format!("! test -w {}", q(p)))),
                outside: None,
            },
            Check { what: "caches are relay's own, not the host's".into(), inside: all(caches), outside: None },
            Check {
                what: "the repository is readable, not writable".into(),
                inside: format!("test -e {r}/.git && ! test -w {r}", r = q(repo)),
                outside: None,
            },
            Check {
                what: "other jobs' clones are hidden".into(),
                inside: format!("test ! -e {}/secret", q(&self.neighbour)),
                outside: Some(format!("test -e {}/secret", q(&self.neighbour))),
            },
            Check {
                what: "relay's own state is hidden".into(),
                inside: format!("test ! -e {}", q(&self.ctx.bus)),
                outside: Some(format!("test -e {}", q(&self.ctx.bus))),
            },
            Check {
                what: "the harness runs".into(),
                inside: format!("{} --version >/dev/null", q(Path::new(&inv.program))),
                outside: None,
            },
        ]
    }

    fn run(self) -> Result<Vec<Outcome>> {
        let user = accounts::home();
        let inv = self.invocation()?;
        let mounts = Mounts {
            home: self.home,
            workdir: Some(&self.clone),
            scratch: &self.scratch,
            tmp: &self.tmp,
            cwd: &self.clone,
            account: true,
        };
        let plan = sandbox::plan(self.ctx, &user, &mounts);
        let markers: Vec<PathBuf> = plan
            .iter()
            .filter_map(|(_, k)| match k {
                Mount::From(src) => Some(src.join(cache_marker())),
                _ => None,
            })
            .collect();
        std::fs::write(self.tmp.join(cache_marker()), "")?;
        for m in &markers {
            std::fs::create_dir_all(m.parent().unwrap_or(m))?;
            std::fs::write(m, "")?;
        }
        let args = sandbox::args(self.ctx, &user, &mounts, sandbox::session_env(self.ctx, &inv));
        let inside =
            |sh: &str| Command::new("bwrap").args(&args).args([OsString::from("sh"), "-c".into(), sh.into()]).output();
        let mut outcomes = Vec::new();
        let mut report = |ok: bool, what: &str, detail: &str| {
            let detail = if ok { String::new() } else { detail.to_owned() };
            outcomes.push(Outcome { what: what.to_owned(), ok, detail });
        };
        for c in self.checks(&plan, &inv) {
            if let Some(control) = &c.outside {
                let ok = Command::new("sh").args(["-c", control]).status()?.success();
                report(ok, &format!("{} (control: present outside)", c.what), "");
            }
            let out = inside(&c.inside)?;
            report(out.status.success(), &c.what, String::from_utf8_lossy(&out.stderr).trim());
        }
        // The sandbox's own regex, on what the inside sees; the harness's own
        // variables (CLAUDE_CONFIG_DIR, OMP_PROFILE) are meant to be there.
        let deny = regex::Regex::new(&self.ctx.cfg.sandbox.env_deny)?;
        let env = String::from_utf8_lossy(&inside("env")?.stdout).into_owned();
        let leaked: Vec<&str> = env
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .filter(|k| deny.is_match(k) && !inv.env.iter().any(|(own, _)| own.as_os_str() == *k))
            .collect();
        report(leaked.is_empty(), "no secret-named variables", &leaked.join(" "));
        for m in &markers {
            let _ = std::fs::remove_file(m);
        }
        Ok(outcomes)
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        for d in [&self.clone, &self.neighbour, &self.scratch, &self.tmp] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
