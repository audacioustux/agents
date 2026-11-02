//! probe: checks the sandbox from the inside, against controls from the
//! outside. Starts no model session and emits nothing.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::accounts;
use crate::ctx::Ctx;
use crate::sandbox::{self, Mounts};

pub fn run(ctx: &Ctx, account: Option<&str>) -> Result<()> {
    if !ctx.cfg.sandbox.enabled {
        bail!("the sandbox is off in this config; nothing to probe");
    }
    let found = accounts::discover(&ctx.cfg);
    let acct = match account {
        Some(l) => found.iter().find(|a| a.label == l).with_context(|| format!("unknown account {l}"))?,
        None => found.first().context("no usable account")?,
    };
    let home = accounts::home();
    std::fs::create_dir_all(&ctx.clones)?;
    let mine = ctx.clones.join(format!("probe-{}", std::process::id()));
    let other = ctx.clones.join(format!("probe-other-{}", std::process::id()));
    std::fs::create_dir_all(&mine)?;
    std::fs::create_dir_all(&other)?;
    std::fs::write(other.join("secret"), "x")?;
    let repo = &ctx.cfg.repo;
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    let others: Vec<String> = found
        .iter()
        .filter(|a| a.dir != acct.dir)
        .map(|a| format!("test -z \"$(ls -A {} 2>/dev/null)\"", q(&a.dir)))
        .collect();
    let mounted: Vec<PathBuf> =
        [repo.clone(), acct.dir.clone(), mine.clone(), ctx.state_root.clone(), ctx.data_root.clone()]
            .into_iter()
            .chain(ctx.cfg.sandbox.writable_home.iter().map(|r| home.join(r)))
            .collect();
    let (planted, canaries) = canaries(&ctx.cfg.hidden_dirs(), &mounted);
    let all_absent =
        |paths: &[PathBuf]| paths.iter().map(|p| format!("test ! -e {}", q(p))).collect::<Vec<_>>().join(" && ");
    let checks: Vec<(&str, String)> = vec![
        ("repository is readable", format!("test -d {}/.git", q(repo))),
        ("repository is read-only", format!("! touch {}/.relay-probe 2>/dev/null", q(repo))),
        ("own clone is writable", format!("touch {}/ok", q(&mine))),
        (
            "hidden directories hide their contents",
            if canaries.is_empty() { "true".into() } else { all_absent(&canaries) },
        ),
        ("other jobs' clones hidden", format!("test ! -e {}/secret", q(&other))),
        ("other accounts hidden", if others.is_empty() { "true".into() } else { others.join(" && ") }),
        ("own account visible", format!("test -f {}/.credentials.json", q(&acct.dir))),
        ("relay's state hidden", format!("test ! -e {}", q(&ctx.bus))),
        ("the agent runs", format!("{} --version >/dev/null", shell_quote(&ctx.cfg.session.command[0]))),
    ];
    let mounts = Mounts { account_dir: &acct.dir, workdir: Some(&mine), cwd: &mine };
    let args = sandbox::args(ctx, &home, &mounts, std::env::vars_os());
    let inside =
        |sh: &str| Command::new("bwrap").args(&args).args([OsString::from("sh"), "-c".into(), sh.into()]).output();
    let mut failed = 0;
    let mut report = |ok: bool, what: &str, detail: String| {
        failed += usize::from(!ok);
        let _ = writeln!(
            std::io::stdout(),
            "{}  {what}{}",
            if ok { "PASS" } else { "FAIL" },
            if ok { String::new() } else { format!("  {detail}") }
        );
    };
    for (what, sh) in &checks {
        let out = inside(sh)?;
        report(out.status.success(), what, String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    // Checked with the sandbox's own regex, read from what the inside actually sees.
    let deny = regex::Regex::new(&ctx.cfg.sandbox.env_deny)?;
    let env = String::from_utf8_lossy(&inside("env")?.stdout).into_owned();
    let leaked: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, _)| k)
        .filter(|k| deny.is_match(k) && *k != "CLAUDE_CONFIG_DIR")
        .collect();
    report(leaked.is_empty(), "no secret-named variables", leaked.join(" "));
    // Controls: outside, the same paths must be visible, or the checks above prove nothing.
    let mut controls = vec![
        ("other jobs' clones exist outside", format!("test -e {}/secret", q(&other))),
        ("relay's state exists outside", format!("test -e {}", q(&ctx.bus))),
    ];
    if !canaries.is_empty() {
        let present = canaries.iter().map(|p| format!("test -e {}", q(p))).collect::<Vec<_>>().join(" && ");
        controls.push(("hidden directories' contents exist outside", present));
    }
    for (what, sh) in &controls {
        let ok = Command::new("sh").args(["-c", sh]).status()?.success();
        report(ok, &format!("control: {what}"), String::new());
    }
    for p in planted {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&other);
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}

/// Entries in the hidden dirs that are not on the way to anything mounted
/// back, so they must be invisible inside. A canary file is planted where a
/// dir is writable; the planted files are returned first, for removal.
fn canaries(hidden: &[PathBuf], mounted: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut planted = Vec::new();
    let mut found = Vec::new();
    for d in hidden {
        let c = d.join(format!(".relay-probe-canary-{}", std::process::id()));
        if std::fs::write(&c, "x").is_ok() {
            planted.push(c);
        }
        found.extend(
            std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| !mounted.iter().any(|m| m.starts_with(p)))
                .take(3),
        );
    }
    (planted, found)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
