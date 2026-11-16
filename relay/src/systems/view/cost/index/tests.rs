use super::*;

/// An assistant line: one response (`id`, `req`) at `ts` in `cwd`, with `usage` as written.
pub fn line(id: &str, req: &str, model: &str, ts: &str, cwd: &str, usage: &serde_json::Value) -> String {
    let v = serde_json::json!({
        "type": "assistant", "requestId": req, "timestamp": ts, "sessionId": "s", "cwd": cwd,
        "message": { "id": id, "model": model, "role": "assistant", "content": [{"type": "text", "text": "hi"}], "usage": usage },
    });
    format!("{v}\n")
}

/// A usage whose real counts are in one iteration, the top level all 0.
pub fn usage(input: u64, output: u64, read: u64, w5: u64, w1h: u64) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
        "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0 },
        "speed": "standard",
        "server_tool_use": { "web_search_requests": 0, "web_fetch_requests": 0 },
        "iterations": [{
            "type": "message", "input_tokens": input, "output_tokens": output, "cache_read_input_tokens": read,
            "cache_creation_input_tokens": w5 + w1h,
            "cache_creation": { "ephemeral_5m_input_tokens": w5, "ephemeral_1h_input_tokens": w1h },
        }],
    })
}

pub struct Machine {
    pub dir: crate::config::tempdir::Dir,
}

impl Machine {
    pub fn new() -> Self {
        Self { dir: crate::config::tempdir::Dir::new() }
    }

    pub fn user(&self) -> PathBuf {
        self.dir.path().join("user")
    }

    pub fn data_root(&self) -> PathBuf {
        self.user().join(".local/share/relay")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.dir.path().join("state/cost")
    }

    /// A transcript at `rel` under `home`'s projects dir, its parents made.
    pub fn file(&self, home: &str, rel: &str) -> PathBuf {
        let p = self.user().join(home).join("projects").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        p
    }

    pub fn write(&self, home: &str, rel: &str, text: &str) -> PathBuf {
        let p = self.file(home, rel);
        std::fs::write(&p, text).unwrap();
        p
    }

    pub fn append(&self, home: &str, rel: &str, text: &str) {
        let p = self.file(home, rel);
        std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
    }

    pub fn refresh(&self) -> (Index, Refreshed) {
        refresh(&self.index_dir(), &homes(&self.user()), &self.data_root()).unwrap()
    }
}

/// Every row's tokens summed per (model, fast), over the whole index.
fn by_model(index: &Index) -> BTreeMap<(String, bool), Tokens> {
    let mut out: BTreeMap<(String, bool), Tokens> = BTreeMap::new();
    for (d, t) in index.rows() {
        let e = out.entry((d.model.clone(), d.fast)).or_default();
        e.iter_mut().zip(t).for_each(|(a, b)| *a += b);
    }
    out
}

const TS: &str = "2026-11-16T10:15:00.000Z";

/// The fixture transcripts: repeated lines of one response, a fork copy in another file and in
/// another home, an advisor iteration, a fast line, an unpriced model, a `<synthetic>` line, and a
/// truncated last line.
pub fn fixture(m: &Machine) {
    let cwd = "/work/demo";
    let a = usage(10, 100, 1000, 50, 20);
    // One response written as three lines (one per content block), the first with a partial output.
    let partial = usage(10, 40, 1000, 50, 20);
    let mut s = line("msg_a", "req_a", "claude-opus-5-5", TS, cwd, &partial);
    s += &line("msg_a", "req_a", "claude-opus-5-5", TS, cwd, &a);
    s += &line("msg_a", "req_a", "claude-opus-5-5", TS, cwd, &a);
    // An advisor iteration, priced at its own model.
    let mut adv = usage(5, 7, 0, 0, 0);
    adv["iterations"].as_array_mut().unwrap().push(serde_json::json!({
        "type": "advisor_message", "model": "claude-fable-5-2", "input_tokens": 300, "output_tokens": 30,
        "cache_read_input_tokens": 0, "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0 },
    }));
    s += &line("msg_b", "req_b", "claude-opus-5-5", TS, cwd, &adv);
    // Fast mode, with a web search.
    let mut fast = usage(1, 2, 0, 0, 0);
    fast["speed"] = "fast".into();
    fast["server_tool_use"]["web_search_requests"] = 3.into();
    s += &line("msg_c", "req_c", "claude-opus-5-5", TS, cwd, &fast);
    // A model without a price.
    s += &line("msg_d", "req_d", "claude-fable-5-1", TS, cwd, &usage(4, 4, 0, 0, 0));
    // A synthetic line: no real usage.
    s += &line("msg_e", "req_e", "<synthetic>", TS, cwd, &usage(9, 9, 9, 9, 9));
    // An older line without iterations: the top-level counts.
    let old = serde_json::json!({ "input_tokens": 6, "output_tokens": 8, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 2 });
    s += &line("msg_f", "req_f", "claude-sonnet-5-5", TS, cwd, &old);
    // The last line cut short: still being written.
    let cut = line("msg_g", "req_g", "claude-opus-5-5", TS, cwd, &usage(1000, 1000, 0, 0, 0));
    s += &cut[..cut.len() / 2];
    m.write(".claude", "-work-demo/s1.jsonl", &s);
    // A fork: the first response copied into another session file, and into another home (a twin);
    // a subagent's transcript below its session.
    m.write(".claude", "-work-demo/s2.jsonl", &line("msg_a", "req_a", "claude-opus-5-5", TS, cwd, &a));
    m.write(".claude.work", "-work-demo/s3.jsonl", &line("msg_a", "req_a", "claude-opus-5-5", TS, cwd, &a));
    let clone = m.data_root().join("agents/clones/fix-it/relay");
    m.write(
        ".claude.work",
        "-clone/s4/subagents/agent-1.jsonl",
        &line("msg_h", "req_h", "claude-haiku-5-5", TS, clone.to_str().unwrap(), &usage(1, 1, 0, 0, 0)),
    );
}

#[test]
fn each_response_counts_once_its_largest_copy_and_each_iteration_at_its_own_model() {
    let m = Machine::new();
    fixture(&m);
    let (index, done) = m.refresh();
    assert_eq!(done.files, 4, "{done:?}");
    let t = by_model(&index);
    assert_eq!(t[&("claude-opus-5-5".into(), false)], [10 + 5, 100 + 7, 1000, 50, 20, 0, 0], "{t:?}");
    assert_eq!(t[&("claude-fable-5-2".into(), false)], [300, 30, 0, 0, 0, 0, 0], "the advisor at its own model");
    assert_eq!(t[&("claude-opus-5-5".into(), true)], [1, 2, 0, 0, 0, 3, 0], "fast, with its searches");
    assert_eq!(t[&("claude-fable-5-1".into(), false)][..2], [4, 4]);
    assert_eq!(t[&("claude-sonnet-5-5".into(), false)], [6, 8, 0, 2, 0, 0, 0], "the top-level counts");
    assert!(!t.keys().any(|(m, _)| m == "<synthetic>"));
    let c = index.counters();
    assert_eq!(c.messages, 6, "a, b, c, d, f, h; the cut line not yet: {c:?}");
    assert_eq!(c.synthetic, 1);
    assert_eq!(c.copies_dropped, 4, "two in s1, one in s2, one in the twin home");
    assert_eq!(c.disagreements, 1, "the partial copy");
    let homes: Vec<&str> = index.rows().map(|(d, _)| d.home.as_str()).collect();
    assert!(homes.contains(&"default") && homes.contains(&"work"), "{homes:?}");
    let clone = index.rows().find(|(d, _)| d.model == "claude-haiku-5-5").unwrap();
    assert_eq!((clone.0.project.as_str(), clone.0.job.as_deref()), ("agents", Some("fix-it")), "a relay clone");
    let opus = index.rows().find(|(d, _)| d.model == "claude-opus-5-5").unwrap();
    assert_eq!((opus.0.project.as_str(), opus.0.job.as_deref()), ("demo", None));
    assert_eq!(opus.0.hour, TS.parse::<jiff::Timestamp>().unwrap().as_second() / 3600);
}

#[test]
fn a_refresh_reads_only_new_bytes_and_totals_outlive_their_files() {
    let m = Machine::new();
    fixture(&m);
    let (first, _) = m.refresh();
    let opus = |i: &Index| by_model(i)[&("claude-opus-5-5".into(), false)];
    // Nothing changed: nothing is read or written.
    let (same, done) = m.refresh();
    assert_eq!((done.read, done.bytes, done.written), (0, 0, false), "{done:?}");
    assert_eq!(opus(&same), opus(&first));
    // The cut line is finished, and a new one appended: only those are read.
    let cut = line("msg_g", "req_g", "claude-opus-5-5", TS, "/work/demo", &usage(1000, 1000, 0, 0, 0));
    m.append(".claude", "-work-demo/s1.jsonl", &cut[cut.len() / 2..]);
    m.append(
        ".claude",
        "-work-demo/s1.jsonl",
        &line("msg_i", "req_i", "claude-opus-5-5", TS, "/work/demo", &usage(1, 1, 0, 0, 0)),
    );
    let (grown, done) = m.refresh();
    assert_eq!((done.read, done.reread), (1, 0), "{done:?}");
    assert_eq!(opus(&grown)[0], opus(&first)[0] + 1001);
    assert_eq!(grown.counters().messages, first.counters().messages + 2);
    // A file replaced (a new inode) and one shrunk are read again from 0: the dedup keeps the totals.
    let s2 = m.file(".claude", "-work-demo/s2.jsonl");
    let text = std::fs::read_to_string(&s2).unwrap();
    std::fs::remove_file(&s2).unwrap();
    std::fs::write(&s2, &text).unwrap();
    let s1 = m.file(".claude", "-work-demo/s1.jsonl");
    let whole = std::fs::read_to_string(&s1).unwrap();
    std::fs::write(&s1, &whole[..whole.len() / 3]).unwrap();
    let (again, done) = m.refresh();
    assert_eq!(done.reread, 2, "{done:?}");
    assert_eq!(opus(&again), opus(&grown), "nothing counted twice");
    // Deleted: its totals stay.
    std::fs::remove_dir_all(m.user().join(".claude/projects")).unwrap();
    let (gone, done) = m.refresh();
    assert_eq!(done.gone, 2, "{done:?}");
    assert_eq!(opus(&gone), opus(&grown), "Claude's cleanup takes the files, not the totals");
    assert_eq!(Index::load(&m.index_dir()).unwrap().sizes().0, 2);
}

#[test]
fn a_busy_index_is_read_as_last_written() {
    let m = Machine::new();
    fixture(&m);
    let (first, _) = m.refresh();
    let _held = crate::os::Role::try_take(&m.index_dir().join("lock")).unwrap().unwrap();
    m.append(".claude", "-work-demo/s1.jsonl", "\n");
    let (read, done) = m.refresh();
    assert!(done.busy);
    assert_eq!(read.counters(), first.counters());
}

#[test]
fn a_clone_counts_as_its_project_and_job() {
    let root = Path::new("/u/.local/share/relay");
    assert_eq!(project_of(Some("/u/.local/share/relay/p/clones/j/sub"), root), ("p".into(), Some("j".into())));
    assert_eq!(project_of(Some("/u/.local/share/relay/p/clones/j"), root), ("p".into(), Some("j".into())));
    assert_eq!(project_of(Some("/workspaces/Tickify"), root), ("Tickify".into(), None));
    assert_eq!(project_of(None, root), ("unknown".into(), None));
}

#[test]
fn every_claude_home_with_projects_counts_signed_in_or_not() {
    let m = Machine::new();
    for d in [".claude/projects", ".claude.a/projects", ".claude.b", ".claude-backups/projects", ".omp/projects"] {
        std::fs::create_dir_all(m.user().join(d)).unwrap();
    }
    std::fs::write(m.user().join(".claude.json"), "{}").unwrap();
    let labels: Vec<String> = homes(&m.user()).into_iter().map(|h| h.label).collect();
    assert_eq!(labels, ["a", "default"]);
}
