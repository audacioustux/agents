import "./test_config.ts";
import { assert, assertEquals, assertThrows } from "@std/assert";
import { alive, loadRegistry, parseRegistry, pathsAt, procStat, refresh, saveRegistry, withLock } from "./registry.ts";
import { claudeResult, lane, longReport } from "./testing.ts";

Deno.test("the registry round-trips: what is saved is what is loaded", () => {
  const paths = pathsAt(Deno.makeTempDirSync());
  const r = parseRegistry(JSON.stringify({ lanes: { a: lane({ name: "a", status: "DONE" }) }, queue: [] }));
  saveRegistry(paths, r);
  assertEquals(loadRegistry(paths), r);
  assertEquals(loadRegistry(pathsAt(Deno.makeTempDirSync())), { lanes: {}, queue: [] }, "no file is an empty registry");
});

Deno.test("the parser refuses unknown keys, wrong kinds, missing keys and unconfigured harnesses", () => {
  const ok = {
    name: "a",
    harness: "claude1",
    model: "m",
    readonly: false,
    created: "t",
    attempts: 0,
    resumes: 0,
    cost: 0,
    status: "DONE",
  };
  const reg = (l: unknown, queue: unknown[] = []) => JSON.stringify({ lanes: { a: l }, queue });
  parseRegistry(reg(ok));
  assertThrows(() => parseRegistry(reg({ ...ok, colour: "red" })), Error, 'unknown key "colour"');
  assertThrows(() => parseRegistry(reg({ ...ok, attempts: "1" })), Error, 'registry.lanes.a.attempts = "1" is not n');
  assertThrows(() => parseRegistry(reg({ ...ok, status: "WAITING" })), Error, "is not state");
  assertThrows(() => parseRegistry(reg({ ...ok, harness: "claude9" })), Error, "is not h");
  const { cost: _, ...noCost } = ok;
  assertThrows(() => parseRegistry(reg(noCost)), Error, 'has no "cost"');
  assertThrows(() => parseRegistry(JSON.stringify({ lanes: {}, queue: [], extra: 1 })), Error, 'unknown key "extra"');
  const item = {
    name: "q",
    cwd: "/",
    prompt: "/p",
    model: "m",
    effort: "high",
    budget: 200,
    globs: null,
    readonly: false,
    force: false,
    harness: "claude1",
    tools: null,
    task: null,
    chrome: false,
  };
  parseRegistry(reg(ok, [item]));
  assertThrows(() => parseRegistry(reg(ok, [{ ...item, harness: "claude9" }])), Error, "registry.queue[0].harness");
});

/** A process that holds an exclusive `flock` on `lock` for `seconds`, started once it holds it. */
async function holdFlock(lock: string, seconds: number) {
  const p = new Deno.Command("flock", {
    args: ["-x", lock, "sh", "-c", `echo held; sleep ${seconds}`],
    stdout: "piped",
  }).spawn();
  const reader = p.stdout.getReader();
  await reader.read(); // "held": the lock is the child's from here on
  reader.releaseLock();
  return p;
}

Deno.test("the registry lock is a flock: it waits for another holder, and keeps another process out", async () => {
  const paths = pathsAt(Deno.makeTempDirSync());
  const holder = await holdFlock(paths.lock, 1.5);
  const t = performance.now();
  await withLock(paths, () => {});
  const waited = performance.now() - t;
  await holder.status;
  assert(waited > 1000, `took the lock after ${waited.toFixed(0)} ms while another process held it`);

  let release!: () => void;
  const held = withLock(paths, () => new Promise<void>((done) => release = done));
  await new Promise((done) => setTimeout(done, 100));
  const other = new Deno.Command("flock", { args: ["-x", paths.lock, "date", "+%s.%N"], stdout: "piped" });
  const started = Date.now() / 1000;
  const waiting = other.output();
  setTimeout(() => release(), 1200);
  await held;
  const tookAt = Number(new TextDecoder().decode((await waiting).stdout));
  assert(tookAt - started > 1, `another process took the lock ${(tookAt - started).toFixed(2)} s after it asked`);
});

Deno.test("refresh classifies an ended lane once, then trusts its state while the output is unchanged", () => {
  const root = Deno.makeTempDirSync();
  const out = `${root}/a.1.json`;
  Deno.writeTextFileSync(out, claudeResult(longReport, { total_cost_usd: 1.234, num_turns: 7 }));
  const r = {
    lanes: {
      a: lane({ name: "a", harness: "claude2", out, status: "RUNNING", pid: 999999, pid_started: "1", cost_prev: 1 }),
    },
    queue: [],
  };
  refresh(r);
  const a = r.lanes.a;
  assertEquals([a.status, a.session, a.cost, a.turns, a.last?.slice(0, 5)], ["DONE", "s-1", 2.23, 7, "Done."]);
  a.status = "EARLY_END"; // what an earlier pass recorded for this same output
  refresh(r);
  assertEquals(a.status, "EARLY_END", "an unchanged output is not read again");
  Deno.utimeSync(out, new Date(), new Date(Date.now() + 5000));
  refresh(r);
  assertEquals(a.status, "DONE", "a changed output is read again");
});

Deno.test("liveness compares the start time, so a recycled pid is not alive", () => {
  const me = procStat(Deno.pid)!;
  assert(alive(Deno.pid, me.started));
  assert(!alive(Deno.pid, String(Number(me.started) + 1)));
  assert(!alive(2 ** 22 + 12345));
});

Deno.test("refresh: a live pid without a start time is not alive; the mtime match tolerates 1 ms, no more", () => {
  const root = Deno.makeTempDirSync();
  const out = `${root}/a.1.json`;
  Deno.writeTextFileSync(out, claudeResult(longReport));
  const mt = Deno.statSync(out).mtime!.getTime() / 1000;
  const a = lane({ name: "a", harness: "claude2", out, pid: Deno.pid, status: "RUNNING" });
  const r = { lanes: { a }, queue: [] };
  refresh(r);
  assertEquals(a.status, "DONE", "a lane with no recorded start time (the oldest ones) could name a recycled pid");
  a.status = "EARLY_END";
  a.parsed_mtime = mt + 0.0005;
  refresh(r);
  assertEquals(a.status, "EARLY_END", "0.5 ms apart is the same file");
  a.parsed_mtime = mt + 0.002;
  refresh(r);
  assertEquals(a.status, "DONE", "2 ms apart is not");
});

Deno.test("output with CRLF line ends reads like any other", () => {
  const root = Deno.makeTempDirSync();
  const out = `${root}/crlf.json`;
  Deno.writeTextFileSync(out, claudeResult(longReport).replace("\n", "\r\n"));
  const r = { lanes: { a: lane({ name: "a", harness: "claude2", out, status: "RUNNING" }) }, queue: [] };
  refresh(r);
  assertEquals(r.lanes.a.status, "DONE");
});

const MAIN = new URL("main.ts", import.meta.url).pathname;
/** The CLI against the config file `c`: whether it succeeded, and what it wrote to stderr. */
async function cli(c: unknown, ...args: string[]) {
  const file = Deno.makeTempFileSync({ suffix: ".json" });
  Deno.writeTextFileSync(file, JSON.stringify(c));
  const out = await new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", MAIN, ...args],
    env: { LANES_CONFIG: file, LANES_ROOT: Deno.makeTempDirSync() },
    stderr: "piped",
    stdout: "null",
  }).output();
  return [out.success, new TextDecoder().decode(out.stderr)] as const;
}
const FIXTURE = JSON.parse(Deno.readTextFileSync(new URL("test.config.json", import.meta.url)));

Deno.test("a config that does not fit is refused at start, naming the key", async () => {
  const status = (c: unknown) => cli(c, "status", "--json");
  assertEquals((await status(FIXTURE))[0], true);
  const capless = structuredClone(FIXTURE);
  delete capless.harnesses.omp.cap;
  assert((await status(capless))[1].includes('.harnesses.omp has no "cap"'));
  assert(
    (await status({ ...FIXTURE, defaults: { ...FIXTURE.defaults, harness: "nobody" } }))[1].includes(
      ".defaults.harness",
    ),
  );
  assert((await status({ ...FIXTURE, colour: "red" }))[1].includes('unknown key "colour"'));
  const homeless = structuredClone(FIXTURE);
  delete homeless.harnesses.claude1.home;
  assert((await status(homeless))[1].includes('a claude harness needs "home"'));
  assert((await status({}))[1].includes('has no "harnesses"'));
});

Deno.test("a config of one harness is complete: every other key has its documented default", async () => {
  const [ok] = await cli(
    { harnesses: { only: { kind: "claude", home: "~/.claude.only", cap: 1 } } },
    "status",
    "--json",
  );
  assertEquals(ok, true);
});

Deno.test("the optional modules refuse, instead of doing nothing, when the config does not name them", async () => {
  const bare = { harnesses: FIXTURE.harnesses };
  assert((await cli(bare, "db", "sweep"))[1].includes("the db module is off"));
  assert((await cli(bare, "db", "new", "a", "--worktree", "."))[1].includes("the db module is off"));
  assert((await cli(bare, "worktree", "status"))[1].includes("the worktree pool is off"));
});
