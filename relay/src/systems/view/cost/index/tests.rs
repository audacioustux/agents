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
        self.sweep(Sweep::Full)
    }

    pub fn sweep(&self, sweep: Sweep) -> (Index, Refreshed) {
        self.sweep_from(sweep, None)
    }

    pub fn sweep_from(&self, sweep: Sweep, prev: Option<Index>) -> (Index, Refreshed) {
        let (index, done) = refresh(&self.index_dir(), &homes(&self.user()), &self.data_root(), sweep, prev).unwrap();
        assert!(!done.busy, "the lock was held by someone else: {done:?}");
        (index, done)
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
    let (read, done) = refresh(&m.index_dir(), &homes(&m.user()), &m.data_root(), Sweep::Full, None).unwrap();
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

/// Sets the modification time of everything under the machine's user dir an hour back: files, then
/// directories below to above (writing a file changes its directory's time, not the one above).
fn age(m: &Machine) {
    fn all(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                all(&p, out);
            }
            out.push(p);
        }
    }
    let mut paths = Vec::new();
    all(&m.user(), &mut paths);
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    // Children come before their directory, so a directory is set after what is in it.
    for p in paths.iter().chain([&m.user()]) {
        File::open(p).unwrap().set_modified(hour_ago).unwrap();
    }
}

#[test]
fn a_changed_only_refresh_lists_only_moved_directories_and_looks_only_at_new_and_lately_written_transcripts() {
    let m = Machine::new();
    fixture(&m);
    let (first, full) = m.refresh();
    assert!(full.full && full.statted == 4 && full.listed == full.dirs, "{full:?}");
    // Directories changed this second cannot be told from ones changed again in it: listed again.
    let (_, fresh) = m.sweep(Sweep::Changed);
    assert!(fresh.listed > 0 && fresh.statted > 0, "{fresh:?}");
    // Everything old: a changed-only refresh lists and looks at nothing, and writes nothing.
    age(&m);
    m.refresh();
    let (same, none) = m.sweep(Sweep::Changed);
    assert_eq!((none.listed, none.statted, none.read, none.written), (0, 0, 0, false), "{none:?}");
    assert!(none.dirs >= 4, "its directories are still looked at: {none:?}");
    assert_eq!((same.counters(), same.sizes()), (first.counters(), first.sizes()));
    let opus = |i: &Index| by_model(i)[&("claude-opus-5-5".into(), false)][0];
    // An old transcript written in place changes no directory's time: not seen until a full sweep.
    let more = |id: &str| line(id, id, "claude-opus-5-5", TS, "/work/demo", &usage(1000, 1, 0, 0, 0));
    let before = opus(&first);
    m.append(".claude", "-work-demo/s2.jsonl", &more("late1"));
    let (missed, done) = m.sweep(Sweep::Changed);
    assert_eq!(done.read, 0, "{done:?}");
    assert_eq!(opus(&missed), before);
    let (caught, done) = m.refresh();
    assert_eq!(done.read, 1, "{done:?}");
    assert_eq!(opus(&caught), before + 1000);
    // It is now lately written: every refresh looks at it, and nothing else.
    m.append(".claude", "-work-demo/s2.jsonl", &more("late2"));
    let (live, done) = m.sweep(Sweep::Changed);
    assert_eq!((done.statted, done.read), (1, 1), "{done:?}");
    assert_eq!(opus(&live), before + 2000);
    // A new transcript changes its directory's time: that directory is listed and the file read.
    age(&m);
    m.refresh();
    m.write(".claude", "-work-demo/s5.jsonl", &more("new1"));
    let (grown, done) = m.sweep(Sweep::Changed);
    assert_eq!((done.listed, done.read, done.files), (1, 1, 5), "{done:?}");
    assert_eq!(opus(&grown), before + 3000);
    // So does one in a directory made after: a subagent's, below a session directory.
    age(&m);
    m.refresh();
    m.write(".claude.work", "-clone/s4/subagents/agent-2.jsonl", &more("new2"));
    let (deeper, done) = m.sweep(Sweep::Changed);
    assert_eq!((done.read, done.files), (1, 6), "{done:?}");
    assert_eq!(opus(&deeper), before + 4000);
    // One deleted: gone from the index, its totals stay.
    age(&m);
    m.refresh();
    std::fs::remove_file(m.file(".claude", "-work-demo/s5.jsonl")).unwrap();
    let (less, done) = m.sweep(Sweep::Changed);
    assert_eq!((done.gone, done.files, done.written), (1, 5, true), "{done:?}");
    assert_eq!(opus(&less), opus(&deeper));
    // The index on disk has the directories: the next process lists nothing either.
    age(&m);
    m.refresh();
    let (_, none) = m.sweep(Sweep::Changed);
    assert_eq!((none.listed, none.statted), (0, 0), "{none:?}");
}

/// A response whose usage has `iterations` as (model, output) pairs, the first the response's own.
fn advised(id: &str, own: &str, iterations: &[(&str, u64)]) -> String {
    let its: Vec<_> = iterations
        .iter()
        .map(|(model, out)| {
            serde_json::json!({
                "type": "message", "model": model, "input_tokens": 100, "output_tokens": out,
                "cache_read_input_tokens": 0, "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0 },
            })
        })
        .collect();
    line(id, id, own, TS, "/work/demo", &serde_json::json!({ "iterations": its }))
}

#[test]
fn a_later_larger_copy_adds_each_iterations_growth_to_that_iterations_model() {
    let m = Machine::new();
    let (opus, fable) = ("claude-opus-5-5", "claude-fable-5-2");
    // The first copy, while streaming: the response's own iteration at 10, the advisor's at 5.
    m.write(".claude", "-work-demo/a.jsonl", &advised("m1", opus, &[(opus, 10), (fable, 5)]));
    let (first, _) = m.refresh();
    let t = by_model(&first);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (10, 5));
    // A copy in a fork, in a later refresh (the index is read back): the advisor's grew to 30, the own stayed.
    m.write(".claude", "-work-demo/b.jsonl", &advised("m1", opus, &[(opus, 10), (fable, 30)]));
    let (second, _) = m.refresh();
    let t = by_model(&second);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (10, 30), "{t:?}");
    // Inputs are counted once: 100 per iteration.
    assert_eq!((t[&(opus.into(), false)][INPUT], t[&(fable.into(), false)][INPUT]), (100, 100));
    // One that grew in both, and one that shrank in one: the largest of each, each at its own model.
    m.write(".claude", "-work-demo/c.jsonl", &advised("m1", opus, &[(opus, 12), (fable, 20)]));
    let (third, _) = m.refresh();
    let t = by_model(&third);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (12, 30), "{t:?}");
    assert_eq!(third.counters().messages, 1);
    assert_eq!(third.counters().copies_dropped, 2);
    // A response seen with one iteration, then with the advisor's too: which iteration grew cannot be told
    // (an index of an earlier version keeps no iterations), so the total's growth goes to the first row.
    m.write(".claude", "-work-demo/d.jsonl", &advised("m2", opus, &[(opus, 7)]));
    m.write(".claude", "-work-demo/e.jsonl", &advised("m2", opus, &[(opus, 9), (fable, 4)]));
    let (fourth, _) = m.refresh();
    let t = by_model(&fourth);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (12 + 13, 30), "{t:?}");
}

#[test]
fn an_index_of_an_earlier_version_keeps_no_iterations_and_a_copy_never_counts_one_twice() {
    let m = Machine::new();
    let (opus, fable) = ("claude-opus-5-5", "claude-fable-5-2");
    m.write(".claude", "-work-demo/a.jsonl", &advised("m1", opus, &[(opus, 10), (fable, 5)]));
    m.refresh();
    // As the earlier version saved it: the response's total output, no iterations.
    let mut old = Index::load(&m.index_dir()).unwrap();
    old.need_keys(&m.index_dir()).unwrap();
    old.meta.parts.clear();
    old.save(&m.index_dir()).unwrap();
    // An identical copy adds nothing: not the advisor's 5 again.
    m.write(".claude", "-work-demo/b.jsonl", &advised("m1", opus, &[(opus, 10), (fable, 5)]));
    let (same, _) = m.refresh();
    let t = by_model(&same);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (10, 5), "{t:?}");
    assert_eq!((t[&(opus.into(), false)][INPUT], t[&(fable.into(), false)][INPUT]), (100, 100));
    // A larger one adds the difference of the totals, as that version did.
    m.write(".claude", "-work-demo/c.jsonl", &advised("m1", opus, &[(opus, 10), (fable, 8)]));
    let (more, _) = m.refresh();
    let t = by_model(&more);
    assert_eq!((t[&(opus.into(), false)][OUTPUT], t[&(fable.into(), false)][OUTPUT]), (13, 5), "{t:?}");
    assert_eq!(more.counters().messages, 1);
}

#[test]
fn a_kept_index_is_not_read_again_until_another_process_writes_it_and_its_keys_are_read_only_when_needed() {
    let m = Machine::new();
    fixture(&m);
    age(&m);
    let (mut a, _) = m.refresh();
    let keys = a.sizes().1;
    assert!(keys > 0);
    a.release_keys();
    assert_eq!(a.sizes(), (4, keys), "released, still counted");
    let stamp = a.stamp;
    // Nothing new: the kept index is the one returned, and no key is read.
    let (same, done) = m.sweep_from(Sweep::Changed, Some(a));
    assert_eq!((done.read, done.written, same.stamp), (0, false, stamp), "{done:?}");
    assert!(!same.keys_loaded, "a refresh that found nothing never reads the keys");
    // Another process writes the index: the kept one is dropped for it.
    let mut kept = same;
    kept.release_keys();
    m.append(
        ".claude",
        "-work-demo/s2.jsonl",
        &line("x", "x", "claude-opus-5-5", TS, "/work/demo", &usage(1, 1, 0, 0, 0)),
    );
    let (other, _) = m.refresh();
    let (seen, done) = m.sweep_from(Sweep::Changed, Some(kept));
    assert_eq!(done.read, 0, "read by the other one already: {done:?}");
    assert_eq!(seen.counters(), other.counters());
    assert_eq!(seen.counters().messages, 7);
    // A line that repeats one counted before the keys were released is found a copy: they are read for it.
    let mut kept = seen;
    kept.release_keys();
    m.append(
        ".claude",
        "-work-demo/s2.jsonl",
        &line("x", "x", "claude-opus-5-5", TS, "/work/demo", &usage(1, 1, 0, 0, 0)),
    );
    let (after, done) = m.sweep_from(Sweep::Full, Some(kept));
    assert_eq!(done.read, 1, "{done:?}");
    assert_eq!((after.counters().messages, after.counters().copies_dropped), (7, other.counters().copies_dropped + 1));
    assert_eq!(after.sizes().1, keys + 1);
    // The snapshot on disk is whole: a fresh load has the same totals.
    assert_eq!(Index::load(&m.index_dir()).unwrap().counters(), after.counters());
}

#[test]
fn the_refresh_lock_is_given_up_even_while_a_forked_child_holds_a_copy_of_it() {
    let m = Machine::new();
    std::fs::create_dir_all(m.index_dir()).unwrap();
    let path = m.index_dir().join("lock");
    // A child forked in the middle of a refresh holds a copy of the descriptor until it execs; a copy is that.
    let role = crate::os::Role::try_take(&path).unwrap().unwrap();
    let child_copy = role.share().unwrap();
    drop(role);
    assert!(crate::os::Role::try_take(&path).unwrap().is_none(), "a plain drop leaves it held by the copy");
    drop(child_copy);
    let held = Unlock(crate::os::Role::try_take(&path).unwrap().unwrap());
    let _child_copy = held.0.share().unwrap();
    drop(held);
    assert!(crate::os::Role::try_take(&path).unwrap().is_some(), "released for everyone");
}

/// The modification time of `path`, and a way to set it back exactly.
fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn set_mtime(path: &Path, t: std::time::SystemTime) {
    File::open(path).unwrap().set_modified(t).unwrap();
}

#[test]
fn a_directory_changed_in_the_instant_it_was_recorded_is_listed_again_whatever_its_time_says() {
    let m = Machine::new();
    fixture(&m);
    let dir = m.user().join(".claude/projects/-work-demo");
    // The racily clean case: a directory last changed just now is recorded, then gains a file in the same
    // clock tick, which leaves its time as it was. The time is forced here, not waited for.
    let now = std::time::SystemTime::now();
    set_mtime(&dir, now);
    m.refresh();
    let new = line("racy", "racy", "claude-opus-5-5", TS, "/work/demo", &usage(1000, 1, 0, 0, 0));
    std::fs::write(dir.join("s9.jsonl"), new).unwrap();
    set_mtime(&dir, now);
    let (seen, done) = m.sweep(Sweep::Changed);
    assert!(done.listed >= 1 && done.read == 1, "{done:?}");
    assert_eq!(seen.sizes().0, 5);
    // An old directory is not in doubt, and one whose time is put back to what it was is a change nobody
    // can see: the full sweep, which lists everything, does.
    age(&m);
    m.refresh();
    let then = mtime(&dir);
    std::fs::write(
        dir.join("s10.jsonl"),
        line("old", "old", "claude-opus-5-5", TS, "/work/demo", &usage(1, 1, 0, 0, 0)),
    )
    .unwrap();
    set_mtime(&dir, then);
    let (missed, done) = m.sweep(Sweep::Changed);
    assert_eq!((done.listed, done.read, missed.sizes().0), (0, 0, 5), "{done:?}");
    let (caught, done) = m.refresh();
    assert_eq!((done.read, caught.sizes().0), (1, 6), "{done:?}");
}

#[test]
fn a_live_transcript_appended_in_the_instant_it_was_looked_at_is_read_whatever_its_time_says() {
    let m = Machine::new();
    fixture(&m);
    let s2 = m.file(".claude", "-work-demo/s2.jsonl");
    m.refresh();
    let then = mtime(&s2);
    // The append leaves the modification time as it was (the same clock tick), the size moves.
    m.append(
        ".claude",
        "-work-demo/s2.jsonl",
        &line("live", "live", "claude-opus-5-5", TS, "/work/demo", &usage(1000, 1, 0, 0, 0)),
    );
    set_mtime(&s2, then);
    let (index, done) = m.sweep(Sweep::Changed);
    assert_eq!(done.read, 1, "{done:?}");
    assert_eq!(by_model(&index)[&("claude-opus-5-5".into(), false)][0], 15 + 1000);
}
