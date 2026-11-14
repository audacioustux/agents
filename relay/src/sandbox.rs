//! What a session can see, as a declarative mount plan, and its bubblewrap
//! command line. The same for every harness.
//!
//! The machine is read-only underneath. On top, in layers (a later layer
//! wins; within a layer, parents come before children):
//!
//! | layer | what | mount |
//! | --- | --- | --- |
//! | 1 | `sandbox.hide` (default: the repository's siblings); the user's runtime dir and host sockets | hidden |
//! | 2 | the repository, and its git directories when it is a linked worktree | read-only |
//! | 3 | every home of every harness but the session's own; its own home; what in it the host runs or obeys (settings, hooks, scripts) | hidden; writable; read-only |
//! | 0 | the directory holding each linked cache path of a job with a tree (see layer 4): the host's entries, each bound back | read-only |
//! | 4 | `sandbox.caches`: relay's own directory for each, never the host's; each of `sandbox.linked_caches`, for a job with a tree, a symlink into its own copy of them, in its tree's mount | writable |
//! | 5 | `sandbox.hide_home` (credential stores) | hidden |
//! | 6 | relay's own state and clones | hidden |
//! | 7 | the job's own clone and directory; its inbox (what relay writes for it) | writable; read-only |
//!
//! `link()` fails across two mounts (EXDEV), even on one filesystem, and some package managers (bun,
//! pnpm, uv) install by linking from their store into the tree. So a job with a tree (a session's
//! clone, a check's checkout) does not get the caches a project declares in `sandbox.linked_caches` as
//! binds of their own: it gets a private copy of them inside its tree's mount, taken from a pool by
//! rename ([`farm::Farm`]), and each such cache path is a symlink into that copy. Every other cache is a
//! bind, as it always was: a copy warms on its own, so a package is fetched once per copy. A symlink cannot
//! stand in the read-only host directory that holds the cache path, so that directory is layer 0: an
//! empty tmpfs with each of the host's entries bound back read-only (a symlink copied as itself), made
//! read-only like a hidden one. Where the cache path's directory is hidden already, the symlink goes in
//! that tmpfs. Where something else covers it, something else is mounted at or under the cache path
//! (a hidden path included), the cache stays a bind, as for a job with no tree, and is not copied: the
//! copy is taken for what [`linkable`] says, the judge the plan asks too. Caches nested in one another
//! are refused when the config is loaded: a session could reach the inner one's directory through the
//! outer one's bind and plant a symlink on relay's way to it.
//!
//! Layer 3 is one layer, ordered by depth, so a protected directory that holds other homes (omp's
//! `profiles`) is mounted before they are hidden inside it.
//!
//! A path that does not exist cannot be mounted, and is left out, unless a writable mount above it
//! would let the session create it: then a path the plan keeps from the session (protected config
//! that is not there yet, such as a `settings.json` or a `hooks/` the account never had) gets a
//! [`Mount::Placeholder`], made empty on the host and mounted read-only, so the session cannot be the
//! one to create it.
//!
//! `/tmp` is not a layer: it is the job's scratch directory on disk (`Ctx::job_tmp`), bound there,
//! never a RAM tmpfs. A hidden directory is an empty tmpfs made read-only once everything inside it
//! has been mounted, so a write to it fails instead of filling RAM.
//!
//! A read-only mount does not stop a process connecting to a Unix socket in
//! it, so sockets are hidden, not just read-only. The network stays shared,
//! since the session needs its provider's API, and with it abstract sockets.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::config::CachePath;
use crate::ctx::Ctx;
use crate::supervised::Passed;

pub mod farm;

#[derive(Debug)]
pub struct Mounts<'a> {
    pub home: &'a crate::harness::Home,
    pub workdir: Option<&'a Path>,
    /// The session's own directory (its session store), writable: relay writes nothing in it.
    pub scratch: &'a Path,
    /// What relay wrote for the session (its prompt file), read-only: a session that could write
    /// there could plant a symlink for relay's next write to follow.
    pub inbox: Option<&'a Path>,
    /// A directory on disk, bound at `/tmp` (and named by `TMPDIR`); never RAM.
    pub tmp: &'a Path,
    pub cwd: &'a Path,
    /// Whether the session's own account is mounted. A check run on its work is not the session: it
    /// gets no sign-in to read and nothing of the account to write.
    pub account: bool,
    /// The job's own copy of the caches ([`farm::Farm`]), inside a writable mount that holds its tree:
    /// each cache is a symlink into it. None: each is a bind of the project's caches.
    pub caches: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    Hidden,
    ReadOnly,
    Writable,
    /// Writable, with this directory's contents instead of the host's.
    From(PathBuf),
    /// A hidden or read-only path that is not there, under a writable mount: made empty first (a
    /// directory, or a file; see [`seed`]), then mounted read-only, so the session cannot create it.
    Placeholder {
        dir: bool,
    },
    /// The host's directory as it is, each entry bound back read-only (a symlink copied as itself) on an
    /// empty tmpfs that is made read-only once everything inside it has been mounted: so that a cache
    /// path in it can be a symlink. Entries the host makes later do not show.
    Rebuilt,
    /// A symlink to this path: a cache, into the job's own copy of it.
    Symlink(PathBuf),
}

type Layer = Vec<(PathBuf, Mount)>;

/// The layers below the caches and above them, each ordered by depth, and what each protected path is.
fn layers(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>) -> (Layer, Layer, BTreeMap<PathBuf, crate::harness::Kind>) {
    let cfg = &ctx.cfg;
    let own = crate::accounts::harness_of(m.home);
    let own_writable = if m.account { own.writable(m.home, user_home) } else { Vec::new() };
    // Its own home, and its sign-in where that lives outside the home (puku-cli's), are not another's.
    let other_homes = crate::harness::ALL
        .iter()
        .flat_map(|h| h.home_paths(user_home))
        .filter(|p| !m.account || (*p != m.home.dir && !own_writable.contains(p)));
    let own_protected = if m.account { own.protected_kinds(m.home) } else { Vec::new() };
    // What the adapter says each protected path is; anything else kept from the session that is not
    // there (a hidden store under the account) is a directory.
    let kinds: BTreeMap<PathBuf, crate::harness::Kind> = own_protected.iter().cloned().collect();
    let all = |paths: Vec<PathBuf>, mount: Mount| paths.into_iter().map(|p| (p, mount.clone())).collect::<Vec<_>>();
    let by_depth = |mut layer: Vec<(PathBuf, Mount)>| {
        layer.sort_by_key(|(p, _)| p.components().count());
        layer
    };
    let before: Vec<(PathBuf, Mount)> = [
        all(cfg.hidden_dirs().into_iter().chain([runtime_dir()]).chain(host_sockets()).collect(), Mount::Hidden),
        all([cfg.repo.clone()].into_iter().chain(ctx.git_dirs.iter().cloned()).collect(), Mount::ReadOnly),
        all(other_homes.collect(), Mount::Hidden)
            .into_iter()
            .chain(all(own_writable, Mount::Writable))
            .chain(all(own_protected.into_iter().map(|(p, _)| p).collect(), Mount::ReadOnly))
            .collect(),
    ]
    .into_iter()
    .flat_map(by_depth)
    .collect();
    let after: Vec<(PathBuf, Mount)> = [
        all(cfg.sandbox.hide_home.iter().map(|r| user_home.join(r)).collect(), Mount::Hidden),
        all(vec![ctx.state_root.clone(), ctx.data_root.clone()], Mount::Hidden),
        all(m.workdir.into_iter().chain([m.scratch]).map(Path::to_path_buf).collect(), Mount::Writable)
            .into_iter()
            .chain(all(m.inbox.map(Path::to_path_buf).into_iter().collect(), Mount::ReadOnly))
            .collect(),
    ]
    .into_iter()
    .flat_map(by_depth)
    .collect();
    (before, after, kinds)
}

/// Where a linked cache's symlink stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stand {
    /// In its directory, rebuilt from the host's entries.
    Rebuilt(PathBuf),
    /// In the empty tmpfs of a hidden directory around it.
    Hidden,
}

/// The linked caches (`sandbox.linked_caches`) a job with a tree gets as a symlink into its own copy, each
/// with where its symlink stands. The one judge of it: the copy holds these caches and nothing else
/// ([`linkable`], [`farm::Farm::take`]), and the plan links these and nothing else. A cache is left a bind
/// (and so never copied) when its path is not there on the host; when its path, or any directory on its
/// whole way (the home and what is above it included), is a symlink (a tmpfs cannot be mounted on one, and
/// a hide rule naming where it leads would mask a bind but not the copy); when a mount's path, by any of
/// its [`ways`], leads into or around it; when another declared cache does, or leads to the same place (a
/// bind around it would cover the symlink's directory, one onto it would land on the symlink, and a job
/// could reach the way to it through either); when anything is mounted at or under its path (a hidden
/// path among them: copied, it would show in the job's tree); when a later layer covers its path or a
/// directory around it (the symlink would be hidden, the copy not); and when what holds its directory is
/// a mount other than a hidden one.
fn linking(cfg: &crate::config::Config, user_home: &Path, before: &Layer, after: &Layer) -> Vec<(CachePath, Stand)> {
    let paths: Vec<PathBuf> = cfg.sandbox.caches.iter().map(|r| user_home.join(r)).collect();
    let cache_ways: Vec<Vec<PathBuf>> = paths.iter().map(|p| ways(p)).collect();
    let ways_of = |layer: &Layer| layer.iter().map(|(q, k)| (ways(q), k.clone())).collect::<Vec<_>>();
    let (before, after) = (ways_of(before), ways_of(after));
    // Whether a path, by any of its ways, is `of` or leads into it; and whether it is `of` or a directory around it.
    let within = |q: &[PathBuf], of: &Path| q.iter().any(|w| w.starts_with(of));
    let around = |q: &[PathBuf], of: &Path| q.iter().any(|w| of.starts_with(w));
    let mut out = Vec::new();
    for (i, (r, p)) in cfg.sandbox.caches.iter().zip(&paths).enumerate() {
        let Some(parent) = p.parent() else { continue };
        if !cfg.sandbox.linked_caches.contains(r) || !p.exists() {
            continue;
        }
        // The path must be the directory itself, all the way from the root: its directory is rebuilt at the
        // path as written (bubblewrap mounts no tmpfs on a symlink), and with a symlink on it a hide rule
        // naming where it leads would mask a bind but not the copy. Such a cache stays a bind.
        if p.ancestors().any(|a| a.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())) {
            continue;
        }
        // With no symlink on it, the cache's path is its real one; every other path is compared by each of
        // its ways, so an alias on it at any depth (a hide rule written by a symlink or by the real path, one
        // that is itself a symlink leading out of the cache) cannot hide an overlap.
        let (real_p, real_parent) = (real(p), real(parent));
        // Every other entry, by its place in the list: one that leads to this same directory is another cache.
        let nested = cache_ways.iter().enumerate().any(|(j, q)| j != i && (within(q, &real_p) || around(q, &real_p)));
        let under = before.iter().chain(&after).any(|(q, _)| within(q, &real_p));
        let covered = after.iter().any(|(q, _)| around(q, &real_p));
        if nested || under || covered {
            continue;
        }
        // What the cache path's directory is, once the layers before the caches are mounted.
        match before.iter().rev().find(|(q, _)| around(q, &real_parent)).map(|(_, k)| k) {
            None => out.push((r.clone(), Stand::Rebuilt(parent.to_path_buf()))),
            Some(Mount::Hidden) => out.push((r.clone(), Stand::Hidden)),
            Some(_) => {}
        }
    }
    out
}

/// Where `p` leads, every symlink on it resolved: its longest existing part made real, the rest (not there
/// yet) appended as written.
fn real(p: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut at = p;
    loop {
        if let Ok(found) = at.canonicalize() {
            return missing.iter().rev().fold(found, |acc: PathBuf, n: &&std::ffi::OsStr| acc.join(n));
        }
        match (at.parent(), at.file_name()) {
            (Some(up), Some(n)) => {
                missing.push(n);
                at = up;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Every place `p` names: itself as written, and, for each directory on its way, that directory made
/// [`real`] with the rest as written. So it is where `p` leads, and also each name it passes through:
/// `~/.cargo/git/secret`, a symlink to elsewhere, is both elsewhere and the entry `secret` in the real
/// `~/.cargo/git`. The one form every overlap between a cache and another path is judged in: a mount
/// lands where its path leads, and what a bind shows under a name is masked by that name.
fn ways(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    for a in p.ancestors() {
        let rest = p.strip_prefix(a).unwrap_or(Path::new(""));
        let way = real(a).join(rest);
        if !out.contains(&way) {
            out.push(way);
        }
    }
    out
}

/// The linked caches the plan would link for these mounts, whatever their `caches` says: what a job's copy
/// of the caches is taken for ([`own_caches`]), so that it holds exactly what the plan links.
pub fn linkable(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>) -> Vec<CachePath> {
    let (before, after, _) = layers(ctx, user_home, m);
    linking(&ctx.cfg, user_home, &before, &after).into_iter().map(|(r, _)| r).collect()
}

/// The mounts, in the order bubblewrap must apply them.
pub fn plan(ctx: &Ctx, user_home: &Path, m: &Mounts<'_>) -> Vec<(PathBuf, Mount)> {
    let (before, after, kinds) = layers(ctx, user_home, m);
    let by_depth = |mut layer: Layer| {
        layer.sort_by_key(|(p, _)| p.components().count());
        layer
    };
    let linked = m.caches.map(|farm| (farm, linking(&ctx.cfg, user_home, &before, &after)));
    let mut rebuilt: Vec<PathBuf> = Vec::new();
    let caches: Layer = ctx
        .cfg
        .sandbox
        .caches
        .iter()
        .map(|r| {
            let p = user_home.join(r);
            let stand = linked.as_ref().and_then(|(farm, l)| l.iter().find(|(q, _)| q == r).map(|(_, s)| (farm, s)));
            match stand {
                Some((farm, s)) => {
                    if let Stand::Rebuilt(parent) = s
                        && !rebuilt.contains(parent)
                    {
                        rebuilt.push(parent.clone());
                    }
                    (p, Mount::Symlink(farm.join(r)))
                }
                None => (p, Mount::From(ctx.caches.join(r))),
            }
        })
        .collect();
    let rebuilt = by_depth(rebuilt.into_iter().map(|p| (p, Mount::Rebuilt)).collect());
    let mut out: Vec<(PathBuf, Mount)> = Vec::new();
    for (p, mount) in rebuilt.into_iter().chain(before).chain(by_depth(caches)).chain(after) {
        if p.exists() {
            out.push((p, mount));
            continue;
        }
        let kept_from_session = matches!(mount, Mount::Hidden | Mount::ReadOnly);
        // Under relay's own cache directory (`From`), what the session creates is not the host's.
        let creatable = out.iter().any(|(w, k)| *k == Mount::Writable && p.starts_with(w));
        if kept_from_session && creatable {
            let dir = kinds.get(&p).is_none_or(|k| *k == crate::harness::Kind::Dir);
            out.push((p.clone(), Mount::Placeholder { dir }));
        }
    }
    out
}

/// The job's own copy of the caches ([`farm::Farm`]), put at `name` in `parent`, a directory in the mount
/// that holds its tree, from the idle copies in `pool` (beside it, on its filesystem), of the caches
/// `rels`: what [`linkable`] says for the job's mounts, and nothing else. A copy an interrupted run left
/// at `name` is used again only with `left`, the proof that run has ended, and retired without
/// ([`farm::Farm::take`]). None without the sandbox, with
/// none, or when it cannot be taken (said on stderr, since the job then installs by copying): then each
/// cache is a bind of the project's caches, as for a job with no tree, and a copy an interrupted run left at `name`
/// is removed, since it would still be in the job's tree (with what a hide rule added since covers). An
/// error when that removal fails: the job must not start beside it.
pub fn own_caches(
    ctx: &Ctx,
    parent: &Path,
    name: &str,
    pool: &Path,
    rels: &[CachePath],
    left: Option<farm::Ended>,
) -> std::io::Result<Option<farm::Farm>> {
    if !ctx.cfg.sandbox.enabled {
        return Ok(None);
    }
    if !rels.is_empty() {
        match farm::Farm::take(parent, name, pool, rels, left) {
            Ok(copy) => return Ok(Some(copy)),
            Err(e) => eprintln!(
                "relay: the linked caches stay binds for this run (a package manager there copies instead of \
                 linking): taking a copy at {} failed: {e}",
                parent.join(name).display()
            ),
        }
    }
    farm::clear(parent, name, pool)?;
    Ok(None)
}

/// Makes the placeholder `p` exist on the host, empty: a directory, or a file (`{}` for JSON, so the
/// owner's harness finds valid, empty settings there later). Only through its parent opened without
/// following a symlink, and only where nothing is (`mkdir`, `O_EXCL | O_NOFOLLOW`).
fn seed(p: &Path, dir: bool) -> std::io::Result<()> {
    use rustix::fs::{Mode, OFlags};
    let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let at = rustix::fs::open(parent, flags, Mode::empty())?;
    if dir {
        rustix::fs::mkdirat(&at, name, Mode::from_raw_mode(0o755))?;
    } else {
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = rustix::fs::openat(&at, name, flags, Mode::from_raw_mode(0o644))?;
        if p.extension().is_some_and(|x| x == "json") {
            std::io::Write::write_all(&mut std::fs::File::from(file), b"{}\n")?;
        }
    }
    Ok(())
}

/// Bubblewrap's arguments that put back, in the empty tmpfs at `dir`, each of the host's entries in it
/// but the cache paths in `links`: read-only, and a symlink as itself. An entry that is gone by the time
/// bubblewrap binds it is left out (`--ro-bind-try`).
fn rebuilt(dir: &Path, links: &[&Path]) -> Vec<OsString> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    names.sort();
    let mut a = Vec::new();
    for p in names.into_iter().filter(|p| !links.contains(&p.as_path())) {
        match std::fs::read_link(&p) {
            Ok(target) => a.extend(["--symlink".into(), target.into(), p.into()]),
            Err(_) => a.extend(["--ro-bind-try".into(), p.clone().into(), p.into()]),
        }
    }
    a
}

/// `$XDG_RUNTIME_DIR` (`/run/user/<uid>`): the user's D-Bus, systemd and agent sockets.
fn runtime_dir() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
}

/// Unix sockets under `/run` (outside the runtime dir, which is hidden whole),
/// such as a container engine's, a few levels deep.
fn host_sockets() -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_socket() {
                out.push(e.path());
            } else if t.is_dir() && depth > 0 && e.path() != runtime_dir() {
                walk(&e.path(), depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new("/run"), 4, &mut out);
    out
}

/// Bubblewrap's arguments, and the directories it binds by open handle (`--bind-fd`), each with which
/// argument names it: the sandbox's supervisor gives bubblewrap those, and no other child of the runner
/// ever has them ([`Passed`]). They must stay open until the sandbox has ended. An error when a bind
/// source cannot be prepared: the sandbox is refused.
pub fn args(
    ctx: &Ctx,
    user_home: &Path,
    m: &Mounts<'_>,
    env: Vec<(OsString, OsString)>,
) -> std::io::Result<(Vec<OsString>, Vec<Passed>)> {
    let mut keep = Vec::new();
    let mut a: Vec<OsString> =
        ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--bind"].map(OsString::from).to_vec();
    a.extend([m.tmp.as_os_str().to_owned(), "/tmp".into()]);
    // A hidden directory is an empty tmpfs, made read-only once every mount has been made inside it:
    // a write fails loudly instead of filling RAM.
    // `/dev/shm` is a RAM tmpfs too, capped at `sandbox.shm_mb` (a write past it fails), or at 0 sealed
    // like a hidden directory.
    let shm_mb = ctx.cfg.sandbox.shm_mb;
    let mut sealed = Vec::new();
    if shm_mb == 0 {
        sealed.push(PathBuf::from("/dev/shm"));
    } else {
        a.extend(["--size".into(), (shm_mb << 20).to_string().into()]);
    }
    a.extend(["--tmpfs", "/dev/shm"].map(OsString::from));
    let plan = plan(ctx, user_home, m);
    let links: Vec<&Path> =
        plan.iter().filter(|(_, k)| matches!(k, Mount::Symlink(_))).map(|(p, _)| p.as_path()).collect();
    for (p, mount) in &plan {
        let p = p.clone();
        let (flag, src) =
            match mount {
                Mount::Rebuilt => {
                    a.extend(["--tmpfs".into(), p.clone().into()]);
                    a.extend(rebuilt(&p, &links));
                    sealed.push(p);
                    continue;
                }
                Mount::Symlink(target) => {
                    a.extend(["--dir".into(), p.parent().unwrap_or(&p).into()]);
                    a.extend(["--symlink".into(), target.into(), p.into()]);
                    continue;
                }
                Mount::Hidden if p.is_dir() => {
                    sealed.push(p.clone());
                    ("--tmpfs", None)
                }
                Mount::Hidden => ("--ro-bind", Some(Path::new("/dev/null"))),
                Mount::ReadOnly => ("--ro-bind", Some(p.as_path())),
                Mount::Writable => ("--bind", Some(p.as_path())),
                Mount::From(src) => {
                    // Opened from relay's caches' root one directory at a time, following no symlink, and bound by
                    // that handle: a link planted on the way refuses the sandbox rather than lead elsewhere.
                    let rel =
                        ctx.cfg.sandbox.caches.iter().find(|r| ctx.caches.join(r) == *src).ok_or_else(|| {
                            std::io::Error::other(format!("{} is none of relay's caches", src.display()))
                        })?;
                    let dir = farm::open_cache_dir(&ctx.caches, rel).map_err(|e| {
                        std::io::Error::new(e.kind(), format!("preparing the cache {}: {e}", rel.display()))
                    })?;
                    a.extend(["--bind-fd".into(), crate::supervised::HANDLE.into(), p.into()]);
                    keep.push(Passed { arg: a.len() - 2, dir });
                    continue;
                }
                // Made empty on the host, then mounted read-only like any protected path. Should that fail
                // (its parent is missing, or not a real directory), bubblewrap's own empty, read-only stand-in.
                Mount::Placeholder { dir } => match seed(&p, *dir) {
                    Ok(()) => ("--ro-bind", Some(p.as_path())),
                    Err(_) if p.exists() => ("--ro-bind", Some(p.as_path())),
                    Err(_) if *dir => {
                        sealed.push(p.clone());
                        ("--tmpfs", None)
                    }
                    Err(_) => ("--ro-bind", Some(Path::new("/dev/null"))),
                },
            };
        a.push(flag.into());
        a.extend(src.map(OsString::from));
        a.push(p.into());
    }
    for p in sealed {
        a.extend(["--remount-ro".into(), p.into()]);
    }
    a.extend(
        ["--unshare-pid", "--unshare-ipc", "--unshare-uts", "--die-with-parent", "--new-session", "--clearenv"]
            .map(OsString::from),
    );
    for (k, v) in env.into_iter().chain([("TMPDIR".into(), "/tmp".into())]) {
        a.extend(["--setenv".into(), k, v]);
    }
    a.extend(["--chdir".into(), m.cwd.into()]);
    Ok((a, keep))
}

/// The variables that say where a process on the host stands (its working and temporary
/// directories), which a session never takes from any layer of its environment: it stands in its own
/// clone, with its own `/tmp`. A development environment's record leaves them out too
/// (`systems::devshell`), reading this same list.
pub const WHERE_IT_STANDS: [&str; 5] = ["TMPDIR", "TEMP", "TMP", "PWD", "OLDPWD"];

/// The environment a session gets: [`Ctx::job_env`] (built from the config, never
/// the caller's) minus names matching `sandbox.env_deny`, then the invocation's own variables
/// (which may name what the filter removes, such as `CLAUDE_CONFIG_DIR`).
pub fn session_env(ctx: &Ctx, inv: &crate::harness::Invocation) -> Vec<(OsString, OsString)> {
    let deny = Regex::new(&ctx.cfg.sandbox.env_deny).expect("validated at load");
    let mut env: Vec<(OsString, OsString)> = ctx
        .job_env()
        .without(&deny)
        .vars()
        .filter(|(k, _)| !WHERE_IT_STANDS.contains(&k.to_string_lossy().as_ref()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Signing keys are hidden, so git inside cannot sign: say so up front
    // rather than let every session find out by a failed commit.
    let git = [
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "tag.gpgsign"),
        ("GIT_CONFIG_VALUE_1", "false"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    env.retain(|(name, _)| !name.to_string_lossy().starts_with("GIT_CONFIG_"));
    env.extend(git);
    for (k, v) in &inv.env {
        env.retain(|(name, _)| name != k);
        env.extend(v.clone().map(|v| (k.clone(), v)));
    }
    // Last, so no layer above (`session.env`, a development shell) can put another interpreter first.
    if !inv.path_first.is_empty() {
        let rest = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
        let dirs = inv.path_first.iter().cloned().chain(std::env::split_paths(&rest));
        env.retain(|(name, _)| name != "PATH");
        env.push(("PATH".into(), crate::host::path_of(dirs)));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, tempdir::Dir};
    use crate::harness::Home;

    fn ctx(root: &Path) -> Ctx {
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = Config::parse("name = \"t\"\n[sandbox]\nhide = []\ncaches = []\nhide_home = []\n", &repo).unwrap();
        Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"))
    }

    fn plan_for(root: &Path, home: &Home) -> Vec<(PathBuf, Mount)> {
        let ctx = ctx(root);
        let (scratch, tmp) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"));
        let m = Mounts {
            home,
            workdir: None,
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &ctx.cfg.repo,
            account: true,
            caches: None,
        };
        plan(&ctx, &root.join("user"), &m)
    }

    fn mount_of<'p>(plan: &'p [(PathBuf, Mount)], p: &Path) -> Option<&'p Mount> {
        plan.iter().find(|(q, _)| q == p).map(|(_, m)| m)
    }

    /// Audit finding 4: protected config the account does not have yet was left out of the plan, so the
    /// session could create it (a `settings.json` with a hook the owner's next claude runs).
    #[test]
    fn protected_config_that_is_not_there_gets_a_placeholder() {
        let d = Dir::new();
        let dir = d.path().join("user/.claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "mine").unwrap();
        let home = Home { harness: "claude", label: "default".into(), dir: dir.clone() };
        let plan = plan_for(d.path(), &home);
        assert_eq!(mount_of(&plan, &dir.join("settings.json")), Some(&Mount::Placeholder { dir: false }), "{plan:?}");
        assert_eq!(mount_of(&plan, &dir.join("hooks")), Some(&Mount::Placeholder { dir: true }));
        assert_eq!(
            mount_of(&plan, &dir.join("CLAUDE.md")),
            Some(&Mount::ReadOnly),
            "what is there is mounted as before"
        );
        let at = |p: &Path| plan.iter().position(|(q, _)| q == p).unwrap();
        assert!(at(&dir) < at(&dir.join("hooks")), "the writable home comes first, its placeholders on top");
        // Nothing is created by planning; a path under no writable mount gets no placeholder at all.
        assert!(!dir.join("settings.json").exists());
        assert!(mount_of(&plan, &d.path().join("user/.ssh")).is_none());
    }

    /// A placeholder is what the adapter says the path is, whatever its name: puku's `PUKU.md` is a
    /// file and its `rules` and `workflows` directories. Its sign-in lives outside the home and is
    /// its own session's to renew, while another home's is hidden.
    #[test]
    fn puku_placeholders_take_their_kind_from_the_adapter_and_its_sign_in_is_its_own() {
        let d = Dir::new();
        let user = d.path().join("user");
        let dir = user.join(".puku-cli");
        for p in [dir.clone(), user.join(".config/pukucode"), user.join(".config/pukucode.work")] {
            std::fs::create_dir_all(p).unwrap();
        }
        let home = Home { harness: "puku", label: "default".into(), dir: dir.clone() };
        let plan = plan_for(d.path(), &home);
        assert_eq!(mount_of(&plan, &dir.join("PUKU.md")), Some(&Mount::Placeholder { dir: false }), "{plan:?}");
        assert_eq!(mount_of(&plan, &dir.join("settings.json")), Some(&Mount::Placeholder { dir: false }));
        assert_eq!(mount_of(&plan, &dir.join("rules")), Some(&Mount::Placeholder { dir: true }));
        assert_eq!(mount_of(&plan, &dir.join("workflows")), Some(&Mount::Placeholder { dir: true }));
        assert_eq!(mount_of(&plan, &user.join(".config/pukucode")), Some(&Mount::Writable));
        assert_eq!(mount_of(&plan, &user.join(".config/pukucode.work")), Some(&Mount::Hidden));
    }

    /// Re-review of #40: a harness's helpers come first on a run's `PATH` whatever `session.env` names,
    /// and the account's own variables (puku's `HOME`) win over the job's.
    #[test]
    fn helpers_lead_the_path_and_the_accounts_variables_win() {
        let d = Dir::new();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let conf = "name = \"t\"\n[session.env]\nPATH = \"/usr/local/bin:/opt/node/bin\"\nHOME = \"/job\"\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), d.path().join("state"), d.path().join("data"));
        let mut inv = crate::harness::Invocation::new("puku-cli");
        inv.env.push(("HOME".into(), Some("/home/u".into())));
        inv.path_first.push(PathBuf::from("/opt/node/bin"));
        let env = session_env(&ctx, &inv);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(get("PATH").as_deref(), Some("/opt/node/bin:/usr/local/bin"));
        assert_eq!(get("HOME").as_deref(), Some("/home/u"));
    }

    /// The default omp profile is `~/.omp`, which holds the others: its session may not make a new
    /// profile (relay would discover it as an account), and the protected `profiles` must not show the
    /// other profiles again, which are hidden inside it.
    #[test]
    fn omp_profiles_are_protected_and_still_hidden() {
        let d = Dir::new();
        let root = d.path().join("user/.omp");
        std::fs::create_dir_all(root.join("agent")).unwrap();
        let home = Home { harness: "omp", label: "default".into(), dir: root.clone() };
        let plan = plan_for(d.path(), &home);
        assert_eq!(mount_of(&plan, &root.join("profiles")), Some(&Mount::Placeholder { dir: true }), "{plan:?}");
        std::fs::create_dir_all(root.join("profiles/work")).unwrap();
        let plan = plan_for(d.path(), &home);
        let at = |p: &Path| plan.iter().position(|(q, _)| q == p).unwrap();
        assert_eq!(mount_of(&plan, &root.join("profiles")), Some(&Mount::ReadOnly));
        assert_eq!(mount_of(&plan, &root.join("profiles/work")), Some(&Mount::Hidden));
        assert!(at(&root.join("profiles")) < at(&root.join("profiles/work")), "hidden after, so it stays hidden");
    }

    /// The caches and a job's tree on one mount: with a copy of the caches in the tree's mount, each linked
    /// cache path is a symlink into it, standing in its directory rebuilt from the host's entries (or in the
    /// hidden tmpfs already there); without one, for a cache not declared linked, or where something else is
    /// mounted under the cache path, it is a bind of relay's caches as before.
    #[test]
    fn a_cache_is_a_symlink_into_the_jobs_copy_when_it_has_one() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        for p in [".cache", ".cargo/registry", ".cargo/git", ".npm/_cacache", ".rustup", "keep"] {
            std::fs::create_dir_all(user.join(p)).unwrap();
        }
        std::os::unix::fs::symlink("keep", user.join("link")).unwrap();
        std::fs::write(user.join(".rc"), "x").unwrap();
        let conf = format!(
            "name = \"t\"\n[sandbox]\nhide = [{:?}]\nhide_home = [\".cargo/git/secret\"]\n\
             caches = [\".cache\", \".cargo/registry\", \".cargo/git\", \".npm/_cacache\", \".absent\", \".rustup\"]\n\
             linked_caches = [\".cache\", \".cargo/registry\", \".cargo/git\", \".npm/_cacache\", \".absent\"]\n",
            user.join(".npm")
        );
        let cfg = Config::parse(&conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp, farm) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"), root.join("tree/f"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = |caches| Mounts {
            home: &home,
            workdir: Some(&farm),
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &farm,
            account: false,
            caches,
        };
        let plan = plan(&ctx, &user, &m(Some(&farm)));
        let at = |p: &Path| plan.iter().position(|(q, _)| q == p).unwrap_or_else(|| panic!("{p:?} in {plan:#?}"));
        let link = |r: &str| Some(Mount::Symlink(farm.join(r)));
        assert_eq!(mount_of(&plan, &user.join(".cache")).cloned(), link(".cache"));
        assert_eq!(mount_of(&plan, &user.join(".cargo/registry")).cloned(), link(".cargo/registry"));
        assert_eq!(mount_of(&plan, &user).cloned(), Some(Mount::Rebuilt), "{plan:#?}");
        assert_eq!(mount_of(&plan, &user.join(".cargo")).cloned(), Some(Mount::Rebuilt));
        assert!(at(&user) == 0 && at(&user.join(".cargo")) == 1, "rebuilt before anything is mounted in it");
        // In a directory hidden already, the symlink goes in its tmpfs; a cache with a hidden path under it, or
        // one the host does not have, is a bind as before (the latter then left out, as before).
        assert_eq!(mount_of(&plan, &user.join(".npm/_cacache")).cloned(), link(".npm/_cacache"));
        assert_eq!(mount_of(&plan, &user.join(".npm")).cloned(), Some(Mount::Hidden));
        assert!(matches!(mount_of(&plan, &user.join(".cargo/git")), Some(Mount::From(_))));
        assert!(mount_of(&plan, &user.join(".absent")).is_none());
        assert!(matches!(mount_of(&plan, &user.join(".rustup")), Some(Mount::From(_))), "not declared linked");

        let (args, _keep) = args(&ctx, &user, &m(Some(&farm)), Vec::new()).unwrap();
        let s = |p: &Path| OsString::from(p);
        let has = |w: &[OsString]| args.windows(w.len()).any(|x| x == w);
        assert!(has(&["--tmpfs".into(), s(&user)]) && has(&["--remount-ro".into(), s(&user)]), "{args:?}");
        assert!(has(&["--ro-bind-try".into(), s(&user.join("keep")), s(&user.join("keep"))]));
        assert!(has(&["--ro-bind-try".into(), s(&user.join(".rc")), s(&user.join(".rc"))]));
        assert!(has(&["--symlink".into(), "keep".into(), s(&user.join("link"))]), "a symlink as itself");
        assert!(has(&["--symlink".into(), s(&farm.join(".cache")), s(&user.join(".cache"))]));
        assert!(!has(&["--ro-bind-try".into(), s(&user.join(".cache")), s(&user.join(".cache"))]), "not the host's");

        // Without a copy (a job with no tree), nothing is rebuilt and every cache is relay's bind.
        let plan = super::plan(&ctx, &user, &m(None));
        assert!(plan.iter().all(|(_, k)| !matches!(k, Mount::Rebuilt | Mount::Symlink(_))), "{plan:#?}");
        assert!(matches!(mount_of(&plan, &user.join(".cache")), Some(Mount::From(_))));
    }

    /// PR #67 review 6: whether a cache could be linked was decided on its path as written, but the host's
    /// path can be a symlink: `~/.cargo/git -> .private`, with `hide_home` naming `.private/secret`. As a
    /// bind, the hide mount lands inside the cache's bind and masks `secret`; linked, it lands on the host's
    /// directory, and the copy (in the job's tree) held `secret`. A cache whose host path has a symlink on
    /// it is not linked: it stays a bind.
    /// Fable and Copilot on (e): a hide rule could reach a linked cache through a symlink of its own
    /// (`~/.private -> .cargo/git`, `hide_home = [".private/secret"]`): compared as written, it was not
    /// under the cache, so the cache was linked, and a copy reused from the pool holding `secret` was bound
    /// over the hide mount. Hide rules are compared by where they lead, and a cache one leads into or
    /// around is not linked; the pooled copy is then not taken at all.
    /// Copilot at fdbf183: a cache's directory in relay's caches could be a symlink a session planted
    /// (through `.cache`, bound before a config named only `.cache/uv`); its preparation failed, the error
    /// was dropped, and bubblewrap followed the path, binding the link's target writable. A bind source is
    /// opened from relay's caches' root following no link, and passed to bubblewrap as that open
    /// directory; one that cannot be refuses the sandbox.
    #[test]
    fn a_cache_whose_source_is_a_planted_symlink_is_never_bound() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cache/uv")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".cache/uv\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let outside = root.join("host-dir");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(ctx.caches.join(".cache")).unwrap();
        std::os::unix::fs::symlink(&outside, ctx.caches.join(".cache/uv")).unwrap();
        let (scratch, tmp) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: None,
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &repo,
            account: false,
            caches: None,
        };
        let built = args(&ctx, &user, &m, Vec::new());
        assert!(built.is_err(), "a sandbox was built over the planted link: {built:?}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing made through the link");
        // Its own directory, there and whole, is bound by an open handle, not by a path.
        std::fs::remove_file(ctx.caches.join(".cache/uv")).unwrap();
        let (built, keep) = args(&ctx, &user, &m, Vec::new()).unwrap();
        let dest = OsString::from(user.join(".cache/uv"));
        assert!(built.windows(3).any(|w| w[0] == "--bind-fd" && w[2] == dest) && keep.len() == 1, "{built:?}");
        let source = ctx.caches.join(".cache/uv");
        assert!(!built.iter().any(|a| Path::new(a) == source), "bound by path");
    }

    /// Copilot at fdbf183: with the home itself a symlink (`/home/me -> /srv/me`) and a hide rule written by
    /// the real path (`/srv/me/.cargo/git/secret`), only the hide side was compared resolved, so the overlap
    /// was missed. Both sides are compared in one canonical form.
    #[test]
    fn a_hide_rule_written_by_the_real_path_of_a_symlinked_home_keeps_the_cache_a_bind() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let real_home = root.join("srv-me");
        std::fs::create_dir_all(real_home.join(".cargo/git/secret")).unwrap();
        let user = root.join("me");
        std::os::unix::fs::symlink(&real_home, &user).unwrap();
        let conf = format!(
            "name = \"t\"\n[sandbox]\nhide = [{:?}]\nhide_home = []\ncaches = [\".cargo/git\"]\nlinked_caches = [\".cargo/git\"]\n",
            real_home.join(".cargo/git/secret")
        );
        let cfg = Config::parse(&conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp, clone) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"), root.join("clone"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: Some(&clone),
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &clone,
            account: false,
            caches: None,
        };
        let rels = linkable(&ctx, &user, &m);
        assert!(rels.is_empty(), "{rels:?}");
    }

    #[test]
    fn a_hide_rule_reaching_a_cache_through_a_symlink_keeps_it_a_bind() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cargo/git/secret")).unwrap();
        std::os::unix::fs::symlink(".cargo/git", user.join(".private")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = [\".private/secret\"]\n\
                    caches = [\".cargo/git\"]\nlinked_caches = [\".cargo/git\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp, clone) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"), root.join("clone"));
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: Some(&clone),
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &clone,
            account: false,
            caches: None,
        };
        let rels = linkable(&ctx, &user, &m);
        assert!(rels.is_empty(), "{rels:?}");
        // A copy in the pool from before the rule, holding what it now hides, stays there.
        let pool = root.join("pool");
        std::fs::create_dir_all(pool.join(format!("idle-{}/.cargo/git/secret", uuid::Uuid::now_v7()))).unwrap();
        let copy = own_caches(&ctx, &clone.join(".git"), farm::IN_CLONE, &pool, &rels, None).unwrap();
        assert!(copy.is_none() && !clone.join(".git/relay-caches").exists(), "the pooled copy was taken");
    }

    #[test]
    fn a_cache_whose_host_path_is_a_symlink_is_not_linked() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".private/secret")).unwrap();
        std::fs::create_dir_all(user.join(".cargo")).unwrap();
        std::os::unix::fs::symlink("../.private", user.join(".cargo/git")).unwrap();
        std::fs::create_dir_all(user.join(".npm-real/_cacache")).unwrap();
        std::os::unix::fs::symlink(".npm-real", user.join(".npm")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = [\".private/secret\"]\n\
                    caches = [\".cargo/git\", \".npm/_cacache\"]\nlinked_caches = [\".cargo/git\", \".npm/_cacache\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp, farm) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"), root.join("tree/f"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: Some(&farm),
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &farm,
            account: false,
            caches: None,
        };
        // The cache itself a symlink, and a directory on its way one.
        assert!(linkable(&ctx, &user, &m).is_empty(), "{:?}", linkable(&ctx, &user, &m));
    }

    /// PR #67 review 4: a copy an interrupted run left in the clone was handled only by taking a copy, so a
    /// run that links nothing (a hide rule added since for its only linked cache) left it there, hidden
    /// path included, readable in the job's tree. With no copy to take, one left in place is removed.
    #[test]
    fn a_copy_left_in_place_goes_when_this_run_links_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new();
        let ctx = ctx(d.path());
        let git = d.path().join("clone/.git");
        std::fs::create_dir_all(git.join("relay-caches/.cargo/git/secret")).unwrap();
        std::fs::write(git.join("relay-caches/.cargo/git/secret/key"), "k").unwrap();
        let _ = own_caches(&ctx, &git, farm::IN_CLONE, &d.path().join("farms"), &[], None);
        assert!(!git.join("relay-caches").exists(), "the abandoned copy is still in the clone");
        // One that cannot be removed (its directory is not writable here) stops the job from starting.
        std::fs::create_dir_all(git.join("relay-caches/.cargo")).unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o555)).unwrap();
        let refused = own_caches(&ctx, &git, farm::IN_CLONE, &d.path().join("farms"), &[], None);
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(refused.is_err(), "a copy left in place that cannot go must refuse the launch");
    }

    /// The linked caches the plan links for a job with a tree, under this `[sandbox]` table, in the home
    /// `user` the caller laid out below `root`.
    fn linkable_in(root: &Path, user: &Path, sandbox: &str) -> Vec<CachePath> {
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let cfg = Config::parse(&format!("name = \"t\"\n[sandbox]\nhide = []\n{sandbox}"), &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp, clone) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"), root.join("clone"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: Some(&clone),
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &clone,
            account: false,
            caches: None,
        };
        linkable(&ctx, user, &m)
    }

    /// Copilot at a6b6099: a hidden path that is itself a symlink leading out of a cache
    /// (`~/.cargo/git/secret -> /elsewhere`) was compared only by where it leads, which is not under the
    /// cache, so the cache was linked; a warmed copy holding a real `secret/key` was then readable in the
    /// job's tree, the hide rule mounted elsewhere. A path is under a cache by any directory on its way.
    #[test]
    fn a_hidden_symlink_leading_out_of_a_cache_keeps_it_a_bind() {
        let d = Dir::new();
        let user = d.path().join("user");
        std::fs::create_dir_all(user.join(".cargo/git")).unwrap();
        std::fs::create_dir_all(d.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(d.path().join("elsewhere"), user.join(".cargo/git/secret")).unwrap();
        let rels = linkable_in(
            d.path(),
            &user,
            "hide_home = [\".cargo/git/secret\"]\ncaches = [\".cargo/git\"]\nlinked_caches = [\".cargo/git\"]\n",
        );
        assert!(rels.is_empty(), "{rels:?}");
    }

    /// Copilot at a6b6099: with `~/.alias -> .store` and both declared caches, the other entry was left out of
    /// the nesting check because it leads to the same place, so `.store` was linked: its symlink then stood
    /// in the rebuilt home beside `.alias`, whose bind landed on it, and no sandbox started. Only the entry
    /// itself is left out; another that leads into, around or to it keeps it a bind.
    #[test]
    fn a_cache_another_cache_leads_to_is_not_linked() {
        let d = Dir::new();
        let user = d.path().join("user");
        std::fs::create_dir_all(user.join(".store")).unwrap();
        std::os::unix::fs::symlink(".store", user.join(".alias")).unwrap();
        let rels = linkable_in(
            d.path(),
            &user,
            "hide_home = []\ncaches = [\".alias\", \".store\"]\nlinked_caches = [\".store\"]\n",
        );
        assert!(rels.is_empty(), "{rels:?}");
    }

    /// Copilot at a6b6099: with the home itself a symlink (`/home/me -> /srv/me`) a linked cache's
    /// directory was rebuilt at the path as written, and bubblewrap refuses a tmpfs on a symlink: no
    /// sandbox started, where the cache as a bind had. A cache is linked only where no directory on its
    /// whole path, the home and what is above it included, is a symlink.
    #[test]
    fn a_cache_in_a_symlinked_home_is_not_linked() {
        let d = Dir::new();
        std::fs::create_dir_all(d.path().join("srv-me/.cache")).unwrap();
        let user = d.path().join("me");
        std::os::unix::fs::symlink(d.path().join("srv-me"), &user).unwrap();
        let rels =
            linkable_in(d.path(), &user, "hide_home = []\ncaches = [\".cache\"]\nlinked_caches = [\".cache\"]\n");
        assert!(rels.is_empty(), "{rels:?}");
    }

    /// Copilot at a6b6099 (two threads): the handles a sandbox binds its caches by were left open across
    /// `exec` in the runner, which holds them through its disk-decision agents and its acceptance checks.
    /// Bubblewrap closes the handles its own arguments name and passes every other on, so such a child
    /// read what its plan hides through `/proc/self/fd/<n>/secret`. A handle is close-on-exec in the
    /// runner for its whole life: nothing the runner starts inherits it.
    #[test]
    fn a_bound_caches_handle_reaches_no_other_child_of_the_runner() {
        let d = Dir::new();
        let root = d.path();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(user.join(".cache")).unwrap();
        let conf = "name = \"t\"\n[sandbox]\nhide = []\nhide_home = []\ncaches = [\".cache\"]\n";
        let cfg = Config::parse(conf, &repo).unwrap();
        let ctx = Ctx::unclaimed(cfg, repo.join("relay.toml"), root.join("state"), root.join("data"));
        let (scratch, tmp) = (root.join("state/t/jobs/j.d"), root.join("data/t/scratch/j"));
        let home = Home { harness: "claude", label: "default".into(), dir: user.join(".claude") };
        let m = Mounts {
            home: &home,
            workdir: None,
            scratch: &scratch,
            inbox: None,
            tmp: &tmp,
            cwd: &repo,
            account: false,
            caches: None,
        };
        let (built, keep) = args(&ctx, &user, &m, Vec::new()).unwrap();
        assert_eq!(keep.len(), 1);
        for Passed { arg, dir: fd } in &keep {
            // Its argument is no handle's number until the sandbox's supervisor puts its own there.
            assert_eq!((built[arg - 1].to_str(), built[*arg].to_str()), (Some("--bind-fd"), Some("handle")));
            let flags = rustix::io::fcntl_getfd(fd).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC), "the runner's handle is left open across exec");
            let n = std::os::fd::AsRawFd::as_raw_fd(fd).to_string();
            let inherited =
                std::process::Command::new("sh").args(["-c", "test -e /proc/self/fd/$0", &n]).status().unwrap();
            assert!(!inherited.success(), "another child of the runner was given the cache's handle");
        }
    }

    #[test]
    fn a_placeholder_is_seeded_empty_and_never_through_a_symlink() {
        let d = Dir::new();
        let home = d.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        seed(&home.join("settings.json"), false).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("settings.json")).unwrap(), "{}\n");
        seed(&home.join("CLAUDE.md"), false).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("CLAUDE.md")).unwrap(), "");
        seed(&home.join("hooks"), true).unwrap();
        assert!(home.join("hooks").is_dir());
        // Something already there (a planted symlink included) is left alone, and nothing is followed.
        let elsewhere = d.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, home.join("settings.local.json")).unwrap();
        assert!(seed(&home.join("settings.local.json"), false).is_err());
        assert!(!elsewhere.exists());
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join("agent")).unwrap();
        assert!(seed(&home.join("agent/config.yml"), false).is_err(), "a symlinked parent is not followed");
        assert!(!elsewhere.join("config.yml").exists());
    }
}
