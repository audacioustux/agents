//! The cost index: token totals folded from the transcripts every Claude home already writes
//! (`<home>/projects/**/*.jsonl`), machine-wide, under `$XDG_STATE_HOME/relay/cost/`. It never runs
//! `claude`.
//!
//! A refresh reads only the bytes a transcript gained since the last one, up to its last complete
//! line; a file that shrank or was replaced (another inode) is read again from 0, which the dedup
//! makes safe. The totals are kept in the index and never recomputed from what is on disk: Claude
//! deletes old transcripts, and their spend stays counted. Tokens, not dollars, are kept, so a price
//! change needs no re-index.
//!
//! One API response is written as several lines (one per content block), forks and resumes copy
//! earlier lines into new files, and twin homes may both hold a copy: each (message id, request id)
//! is counted once, by a 64-bit hash of the pair. A later copy whose output is larger adds the
//! difference to the first copy's output, iteration by iteration, each to its own model's row; any
//! copy whose usage differs is a disagreement, counted.
//!
//! One writer at a time (`lock`); readers read the last snapshot (`index.bin`), written whole to a
//! temporary file and renamed over it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// The token classes a row counts, in [`Tokens`] order.
pub const CLASSES: [&str; 7] =
    ["input", "output", "cache_read", "cache_write_5m", "cache_write_1h", "web_search", "web_fetch"];
pub const INPUT: usize = 0;
pub const OUTPUT: usize = 1;
pub const CACHE_READ: usize = 2;
pub const WRITE_5M: usize = 3;
pub const WRITE_1H: usize = 4;
pub const WEB_SEARCH: usize = 5;
pub const WEB_FETCH: usize = 6;

/// Counts per class of [`CLASSES`].
pub type Tokens = [u64; 7];

/// What a row is the total of: one UTC hour (seconds since the epoch / 3600), one home (its label),
/// one project (with the relay job, for a clone), one model, at one speed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Dims {
    pub hour: i64,
    pub home: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
}

/// How far a transcript has been read, and which file that was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    dev: u64,
    ino: u64,
    /// Read up to here: the end of its last complete line.
    offset: u64,
    /// Its size when last looked at: unchanged, it is not opened again, even with a partial line past `offset`.
    #[serde(default)]
    size: u64,
    /// Its modification time when last looked at (nanoseconds since the epoch; 0: not known). A file
    /// written in the last [`HOT`] is looked at on every refresh; an older one only on a full sweep.
    #[serde(default)]
    mtime: i64,
}

/// What the dedup and the reads have counted, over the index's life.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// API responses counted, each once.
    pub messages: u64,
    /// Lines that repeated a response already counted, and were dropped.
    pub copies_dropped: u64,
    /// Of those, the ones whose usage differed from the counted copy.
    pub disagreements: u64,
    /// `<synthetic>` lines, which carry no real usage.
    pub synthetic: u64,
    /// Lines with a usage that could not be read.
    pub unreadable: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Meta {
    version: u32,
    /// When a refresh last changed the index (seconds since the epoch).
    changed_at: Option<i64>,
    files: BTreeMap<PathBuf, Mark>,
    /// Each directory listed below a home's `projects/` (the root itself included), with its modification
    /// time as of its last listing: one whose time is the same holds the same entries, so it is not
    /// listed again. [`UNSETTLED`] when it changed too lately to tell.
    #[serde(default)]
    dirs: BTreeMap<PathBuf, i64>,
    rows: Vec<(Dims, Tokens)>,
    counters: Counters,
    /// The responses written with more than one iteration (an advisor's beside the model's own): the
    /// row and the largest output counted for each of their iterations, in the order of the line's parts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    parts: BTreeMap<u64, Vec<(u32, u32)>>,
}

/// A counted response: its output total and a fingerprint of its whole usage, and the row its
/// output went to (its own model's). A response of several iterations has each one's in `Meta::parts`.
#[derive(Debug, Clone, Copy)]
struct Seen {
    output: u32,
    print: u32,
    row: u32,
}

const MAGIC: &[u8; 8] = b"RLYCOST1";
const VERSION: u32 = 1;
/// A dedup key on disk: hash, output, fingerprint, row.
const KEY_BYTES: usize = 20;

/// The folded totals and what the refresh needs to go on.
#[derive(Debug)]
pub struct Index {
    meta: Meta,
    by_dims: HashMap<Dims, u32>,
    /// The dedup keys. A snapshot is read without them (`keys_loaded` false) and they are read when a
    /// transcript line needs them: a refresh that finds nothing new, and every reader of the totals,
    /// never does.
    keys: HashMap<u64, Seen>,
    keys_loaded: bool,
    /// How many there are in the snapshot, while they are not loaded.
    key_count: usize,
    /// The snapshot file this is of, as last read or written: one still the same on disk is not read again.
    stamp: Option<Stamp>,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            meta: Meta::default(),
            by_dims: HashMap::new(),
            keys: HashMap::new(),
            keys_loaded: true,
            key_count: 0,
            stamp: None,
        }
    }
}

/// Which file a snapshot is: a rename puts a new one (inode) in place each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
}

impl Stamp {
    fn of(md: &std::fs::Metadata) -> Self {
        Self { dev: md.dev(), ino: md.ino(), size: md.size(), mtime: mtime_ns(md) }
    }

    fn at(dir: &Path) -> Option<Self> {
        std::fs::metadata(dir.join("index.bin")).ok().map(|md| Self::of(&md))
    }
}

/// What one refresh did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Refreshed {
    /// Another process held the lock: the snapshot it last wrote was read instead.
    pub busy: bool,
    pub files: usize,
    /// Files read (from where they were left, or from 0), and bytes read.
    pub read: usize,
    pub bytes: u64,
    /// Files read again from 0: shrunk, or replaced.
    pub reread: usize,
    /// Files gone since the last refresh (their totals stay).
    pub gone: usize,
    pub written: bool,
    /// Whether every transcript was looked at ([`Sweep::Full`]), or only the new and the lately written.
    pub full: bool,
    /// Directories looked at, and listed (their entries read).
    pub dirs: usize,
    pub listed: usize,
    /// Transcripts looked at.
    pub statted: usize,
}

/// What a refresh looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// Every directory is listed and every transcript looked at.
    Full,
    /// Only the directories whose modification time moved are listed (a new transcript changes its
    /// directory's), and of the transcripts known, only those written in the last [`HOT`] are looked at
    /// (an append in place does not change its directory's time). A transcript older than that, written
    /// again in place, is not seen until the next full sweep.
    Changed,
}

/// A transcript written this lately is a live session: a changed-only refresh looks at it every time.
pub const HOT: std::time::Duration = std::time::Duration::from_secs(600);
/// A directory changed this lately cannot be told from one changed again within the same clock tick,
/// so it is listed again next time.
const SETTLE_NS: i64 = 2_000_000_000;
/// The time kept for a directory that is listed again next time whatever its time then.
const UNSETTLED: i64 = -1;

/// Where a home's spend is: a Claude config dir (`~/.claude`, `~/.claude.<label>`) with a
/// `projects/` dir, signed in or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub label: String,
    pub dir: PathBuf,
}

/// Every Claude home under `user_home` that holds a `projects/` dir, by label.
pub fn homes(user_home: &Path) -> Vec<Home> {
    let mut homes: Vec<Home> = std::fs::read_dir(user_home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| {
            crate::harness::claude::label_for(&n, ".claude").map(|label| Home { label, dir: user_home.join(n) })
        })
        .filter(|h| h.dir.join("projects").is_dir())
        .collect();
    homes.sort_by(|a, b| a.label.cmp(&b.label));
    homes
}

/// Where a session ran (`cwd`) as (project, job): a relay clone (`<data_root>/<project>/clones/<job>`,
/// at any depth below it) is its project and job; anything else, its directory's name.
pub fn project_of(cwd: Option<&str>, data_root: &Path) -> (String, Option<String>) {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else { return ("unknown".into(), None) };
    let path = Path::new(cwd);
    if let Ok(rest) = path.strip_prefix(data_root) {
        let parts: Vec<&str> = rest.iter().filter_map(|p| p.to_str()).collect();
        if let [project, "clones", job, ..] = parts.as_slice() {
            return ((*project).to_owned(), Some((*job).to_owned()));
        }
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(cwd);
    (name.to_owned(), None)
}

/// FNV-1a, 64-bit: stable across builds, unlike the standard library's hasher.
fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Line {
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    #[serde(flatten)]
    counts: Counts,
    iterations: Option<Vec<Iteration>>,
    speed: Option<String>,
    server_tool_use: Option<ServerTools>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Iteration {
    model: Option<String>,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Counts {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_creation: Option<Creation>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Creation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServerTools {
    web_search_requests: u64,
    web_fetch_requests: u64,
}

impl Counts {
    fn tokens(&self) -> Tokens {
        let mut t = [0; 7];
        t[INPUT] = self.input_tokens;
        t[OUTPUT] = self.output_tokens;
        t[CACHE_READ] = self.cache_read_input_tokens;
        match &self.cache_creation {
            Some(c) => (t[WRITE_5M], t[WRITE_1H]) = (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens),
            // Without the split (older lines), a cache write is the 5-minute kind, the default.
            None => t[WRITE_5M] = self.cache_creation_input_tokens,
        }
        t
    }
}

/// A transcript line's usage, as (model, tokens) per iteration, the response's own model first.
struct Parsed {
    key: u64,
    hour: i64,
    cwd: Option<String>,
    fast: bool,
    parts: Vec<(String, Tokens)>,
}

enum Read {
    Usage(Parsed),
    Synthetic,
    Unreadable,
    Nothing,
}

fn parse(line: &str) -> Read {
    // Most lines are prompts and tool output: only one with a usage is parsed.
    if !line.contains("\"usage\"") {
        return Read::Nothing;
    }
    let Ok(l) = serde_json::from_str::<Line>(line) else { return Read::Unreadable };
    let Some(m) = l.message else { return Read::Nothing };
    let Some(u) = m.usage else { return Read::Nothing };
    let model = m.model.unwrap_or_default();
    if model == "<synthetic>" {
        return Read::Synthetic;
    }
    let Some(t) = l.timestamp.as_deref().and_then(|t| t.parse::<jiff::Timestamp>().ok()) else {
        return Read::Unreadable;
    };
    let model = if model.is_empty() { "unknown".to_owned() } else { model };
    let mut parts: Vec<(String, Tokens)> = match u.iterations.filter(|i| !i.is_empty()) {
        // The real counts are here; the top-level ones can all be 0 beside them.
        Some(its) => its.into_iter().map(|i| (i.model.unwrap_or_else(|| model.clone()), i.counts.tokens())).collect(),
        None => vec![(model.clone(), u.counts.tokens())],
    };
    // Ordered so the response's own model's part comes first: the dedup's output goes there.
    let own = parts.iter().position(|(m, _)| *m == model).unwrap_or(0);
    parts.swap(0, own);
    if let Some(s) = &u.server_tool_use {
        parts[0].1[WEB_SEARCH] += s.web_search_requests;
        parts[0].1[WEB_FETCH] += s.web_fetch_requests;
    }
    let (id, req) = (m.id.unwrap_or_default(), l.request_id.unwrap_or_default());
    let key =
        if id.is_empty() && req.is_empty() { fnv(&[line.as_bytes()]) } else { fnv(&[id.as_bytes(), req.as_bytes()]) };
    Read::Usage(Parsed {
        key,
        hour: t.as_second().div_euclid(3600),
        cwd: l.cwd,
        fast: u.speed.as_deref() == Some("fast"),
        parts,
    })
}

fn print(parts: &[(String, Tokens)]) -> u32 {
    let mut bytes = Vec::with_capacity(parts.len() * 64);
    for (m, t) in parts {
        bytes.extend_from_slice(m.as_bytes());
        for n in t {
            bytes.extend_from_slice(&n.to_le_bytes());
        }
    }
    let h = fnv(&[&bytes]);
    #[allow(clippy::cast_possible_truncation, reason = "a fingerprint: the low bits are enough")]
    let p = (h ^ (h >> 32)) as u32;
    p
}

impl Index {
    /// The index in `dir`: the last snapshot, or an empty one when there is none yet. Its dedup keys
    /// are read when a refresh needs them.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("index.bin");
        let read = || -> std::io::Result<(Vec<u8>, Stamp)> {
            let mut f = File::open(&path)?;
            let stamp = Stamp::of(&f.metadata()?);
            let mut bytes = Vec::with_capacity(usize::try_from(stamp.size).unwrap_or(0));
            f.read_to_end(&mut bytes)?;
            Ok((bytes, stamp))
        };
        let (bytes, stamp) = match read() {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut index = Self::decode(&bytes).with_context(|| format!("reading {}", path.display()))?;
        index.stamp = Some(stamp);
        Ok(index)
    }

    /// Where the dedup keys start in a snapshot, and how many there are.
    fn key_section(bytes: &[u8]) -> Result<(usize, usize)> {
        let take = |at: usize, n: usize| bytes.get(at..at + n).context("cut short");
        let u64_at = |at: usize| -> Result<u64> { Ok(u64::from_le_bytes(take(at, 8)?.try_into()?)) };
        if take(0, 8)? != MAGIC {
            bail!("not a cost index");
        }
        let at = 16 + usize::try_from(u64_at(8)?)?;
        Ok((at + 8, usize::try_from(u64_at(at)?)?))
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let (keys_at, n) = Self::key_section(bytes)?;
        let meta: Meta = serde_json::from_slice(&bytes[16..keys_at - 8])?;
        if meta.version != VERSION {
            bail!("a cost index of version {}, not {VERSION}", meta.version);
        }
        bytes.get(keys_at..keys_at + n * KEY_BYTES).context("cut short")?;
        let by_dims =
            meta.rows.iter().enumerate().map(|(i, (d, _))| (d.clone(), u32::try_from(i).unwrap_or(0))).collect();
        Ok(Self { meta, by_dims, keys: HashMap::new(), keys_loaded: false, key_count: n, stamp: None })
    }

    /// Reads the dedup keys of the snapshot in `dir`, if they are not read yet. Only valid for an index
    /// whose snapshot is still the file there (a refresh holds the lock, and checks).
    fn need_keys(&mut self, dir: &Path) -> Result<()> {
        if self.keys_loaded {
            return Ok(());
        }
        let path = dir.join("index.bin");
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let (at, n) = Self::key_section(&bytes).with_context(|| format!("reading {}", path.display()))?;
        let section = bytes.get(at..at + n * KEY_BYTES).with_context(|| format!("{} is cut short", path.display()))?;
        let u32_of = |b: &[u8]| -> Result<u32> { Ok(u32::from_le_bytes(b.try_into()?)) };
        let mut keys = HashMap::with_capacity(n);
        for k in section.as_chunks::<KEY_BYTES>().0 {
            let hash = u64::from_le_bytes(k[..8].try_into()?);
            keys.insert(hash, Seen { output: u32_of(&k[8..12])?, print: u32_of(&k[12..16])?, row: u32_of(&k[16..])? });
        }
        (self.keys, self.keys_loaded) = (keys, true);
        Ok(())
    }

    /// Lets go of the dedup keys, which are the bulk of it: they are read again when needed. For an
    /// index that is as its snapshot is (what a refresh returns).
    pub fn release_keys(&mut self) {
        if self.keys_loaded {
            (self.key_count, self.keys, self.keys_loaded) = (self.keys.len(), HashMap::new(), false);
        }
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let meta = serde_json::to_vec(&self.meta)?;
        let mut out = Vec::with_capacity(32 + meta.len() + self.keys.len() * KEY_BYTES);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(meta.len() as u64).to_le_bytes());
        out.extend_from_slice(&meta);
        out.extend_from_slice(&(self.keys.len() as u64).to_le_bytes());
        // Sorted, so the same index is the same bytes.
        let mut keys: Vec<_> = self.keys.iter().collect();
        keys.sort_unstable_by_key(|(k, _)| **k);
        for (k, s) in keys {
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&s.output.to_le_bytes());
            out.extend_from_slice(&s.print.to_le_bytes());
            out.extend_from_slice(&s.row.to_le_bytes());
        }
        Ok(out)
    }

    fn save(&mut self, dir: &Path) -> Result<()> {
        self.need_keys(dir)?;
        self.meta.version = VERSION;
        let bytes = self.encode()?;
        let tmp = dir.join(format!("index.bin.{}.tmp", std::process::id()));
        let mut f = File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join("index.bin")).with_context(|| format!("writing {}", dir.display()))?;
        self.stamp = Stamp::at(dir);
        Ok(())
    }

    /// Every row: what it is the total of, and its tokens.
    pub fn rows(&self) -> impl Iterator<Item = &(Dims, Tokens)> {
        self.meta.rows.iter()
    }

    pub fn counters(&self) -> Counters {
        self.meta.counters
    }

    /// When a refresh last changed it.
    pub fn changed_at(&self) -> Option<jiff::Timestamp> {
        self.meta.changed_at.and_then(|s| jiff::Timestamp::from_second(s).ok())
    }

    /// Transcripts it has a mark for, and dedup keys it holds.
    pub fn sizes(&self) -> (usize, usize) {
        (self.meta.files.len(), if self.keys_loaded { self.keys.len() } else { self.key_count })
    }

    fn row(&mut self, d: Dims) -> u32 {
        if let Some(&i) = self.by_dims.get(&d) {
            return i;
        }
        let i = u32::try_from(self.meta.rows.len()).unwrap_or(u32::MAX);
        self.meta.rows.push((d.clone(), [0; 7]));
        self.by_dims.insert(d, i);
        i
    }

    fn add(&mut self, row: u32, t: &Tokens) {
        let r = &mut self.meta.rows[row as usize].1;
        r.iter_mut().zip(t).for_each(|(a, b)| *a += b);
    }

    /// Takes in one transcript line, read in `home`.
    fn take(&mut self, line: &str, home: &str, data_root: &Path) {
        let p = match parse(line) {
            Read::Usage(p) => p,
            Read::Synthetic => return self.meta.counters.synthetic += 1,
            Read::Unreadable => return self.meta.counters.unreadable += 1,
            Read::Nothing => return,
        };
        let out_of = |t: &Tokens| u32::try_from(t[OUTPUT]).unwrap_or(u32::MAX);
        let output = p.parts.iter().fold(0, |a: u32, (_, t)| a.saturating_add(out_of(t)));
        let fp = print(&p.parts);
        let c = &mut self.meta.counters;
        if let Some(seen) = self.keys.get(&p.key).copied() {
            c.copies_dropped += 1;
            if seen.print != fp {
                c.disagreements += 1;
            }
            // The largest output of each iteration counts: what a later copy has more of goes where that
            // iteration's first copy went, to the iteration's own model (an advisor's is not the response's).
            let mut had = self.meta.parts.get(&p.key).cloned().unwrap_or_else(|| vec![(seen.row, seen.output)]);
            let mut grew = false;
            for (i, (model, t)) in p.parts.iter().enumerate() {
                if i >= had.len() {
                    // An iteration the first copy did not have: a row of its model, in the first copy's hour and place.
                    let mut d = self.meta.rows[seen.row as usize].0.clone();
                    d.model.clone_from(model);
                    had.push((self.row(d), 0));
                }
                let (row, was) = had[i];
                if out_of(t) > was {
                    self.meta.rows[row as usize].1[OUTPUT] += u64::from(out_of(t) - was);
                    had[i].1 = out_of(t);
                    grew = true;
                }
            }
            if grew && let Some(s) = self.keys.get_mut(&p.key) {
                s.output = had.iter().fold(0, |a: u32, (_, o)| a.saturating_add(*o));
                s.print = fp;
            }
            if had.len() > 1 {
                self.meta.parts.insert(p.key, had);
            }
            return;
        }
        c.messages += 1;
        let (project, job) = project_of(p.cwd.as_deref(), data_root);
        let mut rows = Vec::with_capacity(p.parts.len());
        for (model, t) in &p.parts {
            let d = Dims {
                hour: p.hour,
                home: home.to_owned(),
                project: project.clone(),
                job: job.clone(),
                model: model.clone(),
                fast: p.fast,
            };
            let row = self.row(d);
            self.add(row, t);
            rows.push((row, out_of(t)));
        }
        self.keys.insert(p.key, Seen { output, print: fp, row: rows.first().map_or(0, |r| r.0) });
        if rows.len() > 1 {
            self.meta.parts.insert(p.key, rows);
        }
    }

    /// Reads `path` from `from` to its last complete line; returns the new offset and the bytes read.
    fn read_file(&mut self, path: &Path, from: u64, home: &str, data_root: &Path) -> std::io::Result<u64> {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(from))?;
        let mut r = BufReader::with_capacity(1 << 20, f);
        let (mut at, mut buf) = (from, Vec::new());
        loop {
            buf.clear();
            let n = r.by_ref().read_until(b'\n', &mut buf)?;
            // A last line without its newline is still being written: it is read next time.
            if n == 0 || buf.last() != Some(&b'\n') {
                return Ok(at);
            }
            at += n as u64;
            if let Ok(line) = std::str::from_utf8(&buf) {
                self.take(line, home, data_root);
            }
        }
    }
}

/// A directory's or file's modification time, in nanoseconds since the epoch.
fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.mtime().saturating_mul(1_000_000_000).saturating_add(md.mtime_nsec())
}

/// What a refresh found below one home's `projects/`: the directories to take as they were, and the
/// transcripts of those it listed.
#[derive(Default)]
struct Walk {
    dirs: BTreeMap<PathBuf, i64>,
    kept: HashSet<PathBuf>,
    listed: HashSet<PathBuf>,
    /// Every directory looked at, and listed.
    looked: usize,
    read: usize,
}

/// Walks the directories below `root`: the listing of one whose time is as the index has it is taken
/// from the index (its subdirectories are the ones it knew), any other is listed.
fn walk(root: &Path, old: &BTreeMap<PathBuf, i64>, full: bool, now_ns: i64) -> Walk {
    let mut kids: HashMap<&Path, Vec<&Path>> = HashMap::new();
    for d in old.range(root.to_path_buf()..).map(|(d, _)| d.as_path()).take_while(|d| d.starts_with(root)) {
        if let Some(parent) = d.parent() {
            kids.entry(parent).or_default().push(d);
        }
    }
    let mut w = Walk::default();
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        // Its time before its listing: an entry added while it is read moves the time past this one.
        let Ok(md) = std::fs::metadata(&d) else { continue };
        let m = mtime_ns(&md);
        w.looked += 1;
        if !full && old.get(&d) == Some(&m) {
            todo.extend(kids.get(d.as_path()).into_iter().flatten().map(|k| k.to_path_buf()));
            w.kept.insert(d.clone());
            w.dirs.insert(d, m);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        w.read += 1;
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let p = e.path();
            if t.is_dir() {
                todo.push(p);
            } else if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                w.listed.insert(p);
            }
        }
        w.dirs.insert(d, if m < now_ns - SETTLE_NS { m } else { UNSETTLED });
    }
    w
}

/// Brings the index in `dir` up to date with every home's transcripts, when no other process is
/// writing it, and returns it with what was done. A busy index is read as last written.
///
/// `prev` is an index this returned before: when its snapshot is still the file on disk it is kept
/// as it is, and the file is not read again.
pub fn refresh(
    dir: &Path,
    homes: &[Home],
    data_root: &Path,
    sweep: Sweep,
    prev: Option<Index>,
) -> Result<(Index, Refreshed)> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let Some(_lock) = crate::os::Role::try_take(&dir.join("lock"))? else {
        return Ok((Index::load(dir)?, Refreshed { busy: true, ..Refreshed::default() }));
    };
    let mut index = match prev {
        Some(p) if p.stamp.is_some() && p.stamp == Stamp::at(dir) => p,
        _ => Index::load(dir)?,
    };
    let full = sweep == Sweep::Full;
    let mut done = Refreshed { full, ..Refreshed::default() };
    let now_ns = i64::try_from(jiff::Timestamp::now().as_nanosecond()).unwrap_or(i64::MAX);
    let hot_from = now_ns.saturating_sub(i64::try_from(HOT.as_nanos()).unwrap_or(i64::MAX));
    let old_dirs = std::mem::take(&mut index.meta.dirs);
    let roots: Vec<(PathBuf, &str)> = homes.iter().map(|h| (h.dir.join("projects"), h.label.as_str())).collect();
    let walks: Vec<Walk> = roots.iter().map(|(r, _)| walk(r, &old_dirs, full, now_ns)).collect();
    done.dirs = walks.iter().map(|w| w.looked).sum();
    done.listed = walks.iter().map(|w| w.read).sum();
    let mut dirs: BTreeMap<PathBuf, i64> = BTreeMap::new();
    for w in &walks {
        dirs.extend(w.dirs.iter().map(|(d, m)| (d.clone(), *m)));
    }
    // What is known: still there (and to be looked at again, or not), or gone.
    let (mut again, mut gone) = (Vec::new(), Vec::new());
    for (p, m) in &index.meta.files {
        let Some(i) = roots.iter().position(|(r, _)| p.starts_with(r)) else {
            gone.push(p.clone());
            continue;
        };
        let w = &walks[i];
        let there = p.parent().is_some_and(|d| w.kept.contains(d)) || w.listed.contains(p);
        if !there {
            gone.push(p.clone());
        } else if full || m.mtime == 0 || m.mtime >= hot_from {
            again.push((p.clone(), i));
        }
    }
    let mut changed = !gone.is_empty();
    for p in &gone {
        index.meta.files.remove(p);
    }
    done.gone = gone.len();
    // And what is new.
    let new: Vec<(PathBuf, usize)> = walks
        .iter()
        .enumerate()
        .flat_map(|(i, w)| w.listed.iter().filter(|p| !index.meta.files.contains_key(*p)).map(move |p| (p.clone(), i)))
        .collect();
    for (path, i) in again.into_iter().chain(new) {
        done.statted += 1;
        let Ok(md) = std::fs::metadata(&path) else {
            // Gone since the listing.
            changed |= index.meta.files.remove(&path).is_some();
            done.gone += 1;
            continue;
        };
        let (dev, ino, size) = (md.dev(), md.ino(), md.size());
        let old = index.meta.files.get(&path).copied();
        let from = match old {
            Some(m) if (m.dev, m.ino) == (dev, ino) && m.offset <= size => m.offset,
            Some(_) => {
                done.reread += 1;
                0
            }
            None => 0,
        };
        let mut mark = Mark { dev, ino, offset: from, size, mtime: mtime_ns(&md) };
        // Unchanged is not opened again, even with a partial last line past its offset.
        if old == Some(mark) {
            continue;
        }
        if size > from {
            // Gone or unreadable since the listing: what was read before stays.
            index.need_keys(dir)?;
            let Ok(at) = index.read_file(&path, from, roots[i].1, data_root) else { continue };
            done.read += 1;
            done.bytes += at - from;
            mark.offset = at;
        }
        if old != Some(mark) {
            changed = true;
            index.meta.files.insert(path, mark);
        }
    }
    done.files = index.meta.files.len();
    if changed || dirs != old_dirs {
        index.meta.dirs = dirs;
        if changed {
            index.meta.changed_at = Some(jiff::Timestamp::now().as_second());
        }
        index.save(dir)?;
        done.written = true;
    } else {
        index.meta.dirs = old_dirs;
    }
    Ok((index, done))
}

#[cfg(test)]
pub(crate) mod tests;
