//! probe: checks from the inside that the sandbox does what its mount plan
//! says, with controls from the outside proving the hidden paths exist, for
//! one account of every enabled harness (or the one named). Starts no model
//! session and emits nothing.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::accounts::{self, harness_of};
use crate::ctx::Ctx;
use crate::harness::{Home, Invocation, Permissions, Session, Task};
use crate::sandbox::{self, Mount, Mounts};

pub fn run(ctx: &Ctx, account: Option<&str>) -> Result<()> {
    if !ctx.cfg.sandbox.enabled {
        bail!("the sandbox is off in this config; nothing to probe");
    }
    let found = accounts::discover(&ctx.cfg);
    let homes: Vec<&Home> = if let Some(k) = account {
        vec![found.iter().find(|a| a.key() == k).with_context(|| format!("unknown account {k}"))?]
    } else {
        // The first account of each harness: the checks are the same for all.
        let mut seen = Vec::new();
        found
            .iter()
            .filter(|a| {
                !seen.contains(&a.harness) && {
                    seen.push(a.harness);
                    true
                }
            })
            .collect()
    };
    if homes.is_empty() {
        bail!("no usable account");
    }
    let mut failed = 0;
    for home in homes {
        let _ = writeln!(std::io::stdout(), "== {}", home.key());
        failed += Probe::new(ctx, home)?.run()?;
    }
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
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

struct Probe<'a> {
    ctx: &'a Ctx,
    home: &'a Home,
    clone: PathBuf,
    neighbour: PathBuf,
    scratch: PathBuf,
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
        };
        for d in [&p.clone, &p.neighbour, &p.scratch] {
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
        let writable = plan.iter().filter(|(_, k)| *k == Mount::Writable).map(|(p, _)| format!("test -w {}", q(p)));
        let repo = &self.ctx.cfg.repo;
        vec![
            Check {
                what: format!("{} hidden paths are hidden", hidden.len()),
                inside: all(hidden.iter().map(|p| format!("test ! -s {}", q(p)))),
                outside: Some(all(hidden.iter().map(|p| format!("test -e {}", q(p))))),
            },
            Check { what: "writable mounts are writable".into(), inside: all(writable), outside: None },
            Check {
                what: "the repository is readable, not writable".into(),
                inside: format!("test -d {r}/.git && ! test -w {r}", r = q(repo)),
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

    fn run(self) -> Result<usize> {
        let user = accounts::home();
        let inv = self.invocation()?;
        let mounts = Mounts { home: self.home, workdir: Some(&self.clone), scratch: &self.scratch, cwd: &self.clone };
        let plan = sandbox::plan(self.ctx, &user, &mounts);
        let args = sandbox::args(self.ctx, &user, &mounts, sandbox::session_env(self.ctx, &inv));
        let inside =
            |sh: &str| Command::new("bwrap").args(&args).args([OsString::from("sh"), "-c".into(), sh.into()]).output();
        let mut failed = 0;
        let mut report = |ok: bool, what: &str, detail: &str| {
            failed += usize::from(!ok);
            let detail = if ok || detail.is_empty() { String::new() } else { format!("  {detail}") };
            let _ = writeln!(std::io::stdout(), "{}  {what}{detail}", if ok { "PASS" } else { "FAIL" });
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
        Ok(failed)
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        for d in [&self.clone, &self.neighbour, &self.scratch] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
