//! probe: checks from the inside that the sandbox does what its mount plan
//! says, with controls from the outside proving the hidden paths exist, for
//! one account of every enabled harness (or the one named). Starts no model
//! session and emits nothing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::accounts::{self, harness_of};
use crate::config::CachePath;
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

/// The probe's process id and when it started: together they name one process ([`crate::os::start_time`]).
fn me() -> String {
    let pid = std::process::id();
    format!("{pid}-{}", crate::os::start_time(pid).unwrap_or(0))
}

/// The marker's name: [`MARKER`], then [`me`].
fn cache_marker() -> String {
    format!("{MARKER}{}", me())
}

/// Whether the probe `<pid>-<start>` is running: a process is both.
fn running(tag: &str) -> bool {
    tag.split_once('-').is_some_and(|(pid, start)| match (pid.parse::<u32>(), start.parse::<u64>()) {
        (Ok(pid), Ok(start)) => crate::os::start_time(pid) == Some(start),
        _ => false,
    })
}

use sandbox::farm::MARKER;

struct Probe<'a> {
    ctx: &'a Ctx,
    home: &'a Home,
    clone: PathBuf,
    neighbour: PathBuf,
    scratch: PathBuf,
    inbox: PathBuf,
    tmp: PathBuf,
}

impl<'a> Probe<'a> {
    fn new(ctx: &'a Ctx, home: &'a Home) -> Result<Self> {
        // All it makes is in its own directory among the probes', named for its process and when that
        // started: gc removes what a killed probe left, its clone's warmed copy of the linked caches
        // included, once no process is both; and no job's name can be taken for it.
        let dir = ctx.probes.join(me());
        let p = Self {
            ctx,
            home,
            clone: dir.join("clone"),
            neighbour: dir.join("other"),
            scratch: dir.join("scratch"),
            inbox: dir.join("inbox"),
            tmp: dir.join("scratch/tmp"),
        };
        for d in [&p.clone, &p.neighbour, &p.scratch, &p.inbox, &p.tmp] {
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
            inbox: &self.inbox,
        };
        let inv = harness_of(self.home).invocation(self.home, &task).map_err(|e| anyhow::anyhow!(e.0))?;
        self.ctx.cfg.locate(self.home.harness, inv).map_err(|e| anyhow::anyhow!(e.0))
    }

    /// The checks of the linked caches' layout ([`Mount::Symlink`], [`Mount::Rebuilt`]); each passes with
    /// none to check, for a project that declares no linked cache.
    fn linked(&self, plan: &[(PathBuf, Mount)]) -> [Check; 2] {
        // A linked cache is a symlink into the clone's copy, on the clone's mount: link() from it into the
        // clone works, where across two mounts it fails with EXDEV.
        let link = self.clone.join(".relay-probe-link");
        let linked = plan.iter().filter_map(|(p, k)| match k {
            Mount::Symlink(_) => Some(format!(
                "test -L {p} && ln {m} {l} && rm {l}",
                p = q(p),
                m = q(&p.join(cache_marker())),
                l = q(&link)
            )),
            _ => None,
        });
        // The directory holding a linked cache is the host's, rebuilt: what is in it shows, read-only.
        let rebuilt: Vec<(String, String)> = plan
            .iter()
            .filter(|(_, k)| *k == Mount::Rebuilt)
            .map(|(p, _)| {
                let shown: Vec<PathBuf> = std::fs::read_dir(p)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|e| !plan.iter().any(|(m, k)| m == e && matches!(k, Mount::Symlink(_))))
                    .take(3)
                    .collect();
                // A symlink is there as itself, whatever it points at (a dangling one too).
                let exist = all(shown.iter().map(|e| format!("{{ test -L {e} || test -e {e}; }}", e = q(e))));
                (format!("{exist} && ! test -w {}", q(p)), exist)
            })
            .collect();
        [
            Check {
                what: "linked caches share the tree's mount: link() from one into the clone works".into(),
                inside: all(linked),
                outside: None,
            },
            Check {
                what: "a directory holding a linked cache shows the host's entries, read-only".into(),
                inside: all(rebuilt.iter().map(|(i, _)| i.clone())),
                outside: Some(all(rebuilt.into_iter().map(|(_, o)| o))),
            },
        ]
    }

    fn checks(&self, plan: &[(PathBuf, Mount)], inv: &Invocation) -> Vec<Check> {
        // A hidden path must hide what is inside it, except where a later mount is: one that shows something
        // again, or one hidden itself (relay's own state, say), whose mount point is an empty directory there.
        let shown_later = |i: usize, p: &Path| plan[i + 1..].iter().any(|(m, _)| m.starts_with(p));
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
        let writable = paths(|k| matches!(k, Mount::Writable | Mount::From(_) | Mount::Symlink(_)))
            .map(|p| format!("test -w {}", q(p)));
        // A placeholder is read-only too: protected config the account does not have yet stays uncreatable.
        let read_only =
            paths(|k| matches!(k, Mount::ReadOnly | Mount::Placeholder { .. })).map(|p| format!("! test -w {}", q(p)));
        // Each cache shows relay's directory, which holds the marker, not the host's.
        let caches = plan.iter().filter_map(|(p, k)| match k {
            Mount::From(_) | Mount::Symlink(_) => Some(format!("test -e {}", q(&p.join(cache_marker())))),
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
            match self.ctx.cfg.sandbox.shm_mb {
                0 => Check {
                    what: "/dev/shm is not writable".into(),
                    inside: "! test -w /dev/shm".into(),
                    outside: None,
                },
                mb => Check {
                    what: format!("/dev/shm is writable and holds at most {mb} MiB"),
                    inside: format!(
                        "test -w /dev/shm && test \"$(df -Pk /dev/shm | awk 'NR==2 {{print $2}}')\" -le {}",
                        mb * 1024
                    ),
                    outside: None,
                },
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
        std::fs::write(self.tmp.join(cache_marker()), "")?;
        // Removed on every way out, by what owns them ([`Marks`]).
        let mut marks = seed_markers(self.ctx, &user, self.home, &self.clone)?;
        // The clone is a session's: its linked caches are a copy in its `.git`, as a session's are.
        std::fs::create_dir_all(self.clone.join(".git"))?;
        let mounts = Mounts {
            home: self.home,
            workdir: Some(&self.clone),
            scratch: &self.scratch,
            inbox: Some(&self.inbox),
            tmp: &self.tmp,
            cwd: &self.clone,
            account: true,
            caches: None,
        };
        let rels = sandbox::linkable(self.ctx, &user, &mounts);
        let copy = sandbox::own_caches(
            self.ctx,
            &self.clone.join(".git"),
            sandbox::farm::IN_CLONE,
            &self.ctx.session_copies,
            &rels,
        );
        marks.copy = copy?;
        // No walk copies a marker (it is relay's own file), so the copy gets its own.
        marks.mark_copy()?;
        let mounts = Mounts { caches: marks.copy.as_ref().map(sandbox::farm::Farm::path), ..mounts };
        let plan = sandbox::plan(self.ctx, &user, &mounts);
        let (args, passed) = sandbox::args(self.ctx, &user, &mounts, sandbox::session_env(self.ctx, &inv))?;
        // Each under the supervisor, as a session's sandbox and a check's are: it alone gives bubblewrap
        // the directories bound by handle, and it alone sees the sandbox's whole tree end (bubblewrap says
        // how its command exited before the rest of its namespace has gone).
        let inside = |sh: &str| -> std::io::Result<(crate::supervised::Output, Option<sandbox::farm::Ended>)> {
            let mut c = crate::supervised::holding("bwrap", None, &passed)?;
            c.args(&args).args([OsString::from("sh"), "-c".into(), sh.into()]);
            let (out, child) = crate::supervised::output(&mut c)?;
            Ok((out, sandbox::farm::Ended::reaped(&child)))
        };
        // Proof, for the copy, that each sandbox run on it ended: all of them, or none.
        let mut waited: Vec<Option<sandbox::farm::Ended>> = Vec::new();
        let mut outcomes = Vec::new();
        let mut report = |ok: bool, what: &str, detail: &str| {
            let detail = if ok { String::new() } else { detail.to_owned() };
            outcomes.push(Outcome { what: what.to_owned(), ok, detail });
        };
        for c in self.checks(&plan, &inv).into_iter().chain(self.linked(&plan)) {
            if let Some(control) = &c.outside {
                let ok = Command::new("sh").args(["-c", control]).status()?.success();
                report(ok, &format!("{} (control: present outside)", c.what), "");
            }
            let (out, ended) = inside(&c.inside)?;
            waited.push(ended);
            report(out.status.success(), &c.what, String::from_utf8_lossy(&out.stderr).trim());
        }
        // The sandbox's own regex, on what the inside sees; the harness's own
        // variables (CLAUDE_CONFIG_DIR, OMP_PROFILE) are meant to be there.
        let deny = regex::Regex::new(&self.ctx.cfg.sandbox.env_deny)?;
        let (env_out, ended) = inside("env")?;
        waited.push(ended);
        marks.ended = waited.iter().copied().collect::<Option<Vec<_>>>().and_then(|all| all.last().copied());
        let env = String::from_utf8_lossy(&env_out.stdout).into_owned();
        let leaked: Vec<&str> = env
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .filter(|k| deny.is_match(k) && !inv.env.iter().any(|(own, _)| own.as_os_str() == *k))
            .collect();
        report(leaked.is_empty(), "no secret-named variables", &leaked.join(" "));
        Ok(outcomes)
    }
}

/// The probe's markers, in each of relay's caches the plan mounts, and the clone's copy of the linked
/// caches (which holds them too): dropped on any way out of the probe, it takes the markers out of the
/// copy, gives the copy back (so it goes to the pool without them), then takes them out of relay's caches.
struct Marks<'a> {
    ctx: &'a Ctx,
    marked: Vec<CachePath>,
    copy: Option<sandbox::farm::Farm>,
    /// Every sandbox the probe ran on the copy was waited for to the end: the copy goes back to the pool.
    ended: Option<sandbox::farm::Ended>,
}

impl Marks<'_> {
    /// Puts the marker in each marked cache the clone's copy holds.
    fn mark_copy(&self) -> std::io::Result<()> {
        let Some(copy) = &self.copy else { return Ok(()) };
        for rel in &self.marked {
            let dir = copy.path().join(rel);
            if dir.is_dir() {
                std::fs::File::create_new(dir.join(cache_marker()))?;
            }
        }
        Ok(())
    }
}

impl Drop for Marks<'_> {
    fn drop(&mut self) {
        if let Some(copy) = self.copy.take() {
            for rel in &self.marked {
                let _ = std::fs::remove_file(copy.path().join(rel).join(cache_marker()));
            }
            if let Some(ended) = self.ended {
                copy.give_back(ended);
            }
        }
        for rel in &self.marked {
            let _ = sandbox::farm::remove_in_cache(&self.ctx.caches, rel, &cache_marker());
        }
    }
}

/// Puts the marker in each of relay's caches the plan mounts, so a cache shown inside is known to be
/// relay's; it is there before the clone takes its copy of the linked caches, so the copy has it too.
/// Through handles from relay's caches' root that follow no symlink, never over a file there. What it
/// returns owns them; on failure, those put are gone already.
fn seed_markers<'a>(ctx: &'a Ctx, user: &Path, home: &Home, clone: &Path) -> std::io::Result<Marks<'a>> {
    let (scratch, tmp) = (clone.join(".probe-scratch"), clone.join(".probe-tmp"));
    let mounts = Mounts {
        home,
        workdir: Some(clone),
        scratch: &scratch,
        inbox: None,
        tmp: &tmp,
        cwd: clone,
        account: true,
        caches: None,
    };
    let plan = sandbox::plan(ctx, user, &mounts);
    let mounted = ctx.cfg.sandbox.caches.iter().filter(|r| {
        let src = ctx.caches.join(r);
        plan.iter().any(|(_, k)| *k == Mount::From(src.clone()))
    });
    let mut marks = Marks { ctx, marked: Vec::new(), copy: None, ended: None };
    for rel in mounted {
        // A probe that was killed removed nothing: its markers go now, once no process is both its pid and
        // its start time (a marker named by a pid alone, from before, says neither: it goes too).
        for name in sandbox::farm::names_in_cache(&ctx.caches, rel).unwrap_or_default() {
            let gone = name.strip_prefix(MARKER).is_some_and(|tag| !running(tag));
            if gone {
                let _ = sandbox::farm::remove_in_cache(&ctx.caches, rel, &name);
            }
        }
        sandbox::farm::put_in_cache(&ctx.caches, rel, &cache_marker())?;
        marks.marked.push(rel.clone());
    }
    Ok(marks)
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        let _ = crate::hostdir::remove_tree(&self.ctx.probes.join(me()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, tempdir::Dir};

    /// PR #67 review 4: the probe put its marker in every configured cache by path, those the plan leaves
    /// out included, following any symlink on the way: a link planted in relay's caches had it make
    /// directories and files outside them. It marks only the caches the plan mounts, through handles that
    /// follow no symlink.
    #[test]
    fn markers_go_only_in_mounted_caches_and_through_no_symlink() {
        markers_case();
    }

    /// PR #67 review 5: markers were removed only on the paths that reached the end of a probe, so an early
    /// return (a `.git` that could not be made, a probe command that could not run) left them in relay's
    /// caches. What seeding returns owns them: dropped, it removes them.
    #[test]
    fn markers_go_when_what_seeded_them_is_dropped() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cache")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".cache\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        let seeded = seed_markers(&ctx, &user, &home, &clone).unwrap();
        assert!(ctx.caches.join(".cache").join(cache_marker()).exists());
        drop(seeded);
        assert!(!ctx.caches.join(".cache").join(cache_marker()).exists(), "a marker outlived the probe");
    }

    /// The markers go from the clone's copy of the linked caches before it is given back to the pool, and
    /// from relay's caches; a killed probe's markers go at the next probe.
    /// Copilot on (e): a marker was named by the probe's pid alone, so a dead probe's marker under a pid
    /// reused by this probe was taken for this probe's own: it stayed, and making this one's failed with
    /// EEXIST and stopped the probe. A marker names its probe's pid and start time.
    #[test]
    fn a_dead_probes_marker_under_a_reused_pid_neither_stays_nor_stops_a_probe() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cache")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".cache\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        let me = std::process::id();
        let cache = ctx.caches.join(".cache");
        std::fs::create_dir_all(&cache).unwrap();
        // Left by probes that had this pid before: one named the old way, one that started at another time.
        std::fs::write(cache.join(format!("{MARKER}{me}")), "").unwrap();
        std::fs::write(cache.join(format!("{MARKER}{me}-1")), "").unwrap();
        let marks = seed_markers(&ctx, &user, &home, &clone).unwrap();
        assert!(!cache.join(format!("{MARKER}{me}")).exists() && !cache.join(format!("{MARKER}{me}-1")).exists());
        drop(marks);
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0, "a marker left behind");
    }

    #[test]
    fn markers_leave_the_copy_before_it_is_pooled_and_a_dead_probes_go_too() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".bun/install/cache")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".bun/install/cache\"]\n\
                    linked_caches = [\".bun/install/cache\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let clone = root.join("clone");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        let rel = &ctx.cfg.sandbox.caches[0];
        std::fs::create_dir_all(ctx.caches.join(rel)).unwrap();
        // A marker of a probe whose process is gone (no process has id 0 here).
        std::fs::write(ctx.caches.join(rel).join(format!("{MARKER}0")), "").unwrap();
        let mut marks = seed_markers(&ctx, &user, &home, &clone).unwrap();
        assert!(!ctx.caches.join(rel).join(format!("{MARKER}0")).exists(), "a dead probe's marker stays");
        marks.copy = sandbox::own_caches(
            &ctx,
            &clone.join(".git"),
            sandbox::farm::IN_CLONE,
            &ctx.session_copies,
            std::slice::from_ref(rel),
        )
        .unwrap();
        assert!(!marks.copy.as_ref().unwrap().path().join(rel).join(cache_marker()).exists(), "copied with the cache");
        marks.mark_copy().unwrap();
        // As its runs prove, each seen to its end by its supervisor: here one process, exited with a code.
        let (_, child) = crate::supervised::output(&mut crate::supervised::command("true").unwrap()).unwrap();
        marks.ended = sandbox::farm::Ended::reaped(&child);
        assert!(marks.copy.as_ref().unwrap().path().join(rel).join(cache_marker()).exists(), "the copy has its own");
        drop(marks);
        let pooled: Vec<_> = sandbox::farm::idle(&ctx.session_copies);
        assert_eq!(pooled.len(), 1);
        assert!(
            !ctx.session_copies.join(&pooled[0].0).join(rel).join(cache_marker()).exists(),
            "pooled with the marker"
        );
        assert!(!ctx.caches.join(rel).join(cache_marker()).exists(), "left in relay's cache");
    }

    fn markers_case() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cache")).unwrap();
        std::fs::create_dir_all(user.join(".npm")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".cache\", \".npm\", \".absent\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&ctx.caches).unwrap();
        std::os::unix::fs::symlink(&outside, ctx.caches.join(".npm")).unwrap();
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        let seeded = seed_markers(&ctx, &user, &home, &clone);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "written through the planted link");
        assert!(!ctx.caches.join(".absent").exists(), "a cache the plan leaves out was made");
        assert!(seeded.is_err(), "a symlink in the way is refused");
        assert!(!ctx.caches.join(".cache").join(cache_marker()).exists() || seeded.is_err());
    }
}
