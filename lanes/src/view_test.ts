import "./test_config.ts";
import { assert, assertEquals } from "@std/assert";
import { pathsAt, procStat, saveRegistry } from "./registry.ts";
import type { Registry } from "./rules.ts";
import { fakeSeams, lane, scratch, usageText } from "./testing.ts";
import { claudeUsages } from "./runner.ts";
import { asJson, board, entries, watch } from "./view.ts";

const NOW = Date.parse("2030-01-02T21:30:00Z") / 1000;
const me = { pid: Deno.pid, pid_started: procStat(Deno.pid)!.started }; // alive for the refresh
const r: Registry = {
  lanes: {
    old: lane({
      name: "old",
      harness: "claude2",
      status: "DONE",
      started: "2030-01-02T08:00:00Z",
      stopped: "done with",
    }),
    stuck: lane({ name: "stuck", harness: "claude3", status: "ERROR", started: "2030-01-02T08:00:00Z" }),
    b: lane({
      name: "b",
      harness: "omp",
      model: "provider-a/model-large",
      status: "RUNNING",
      ...me,
      started: "2030-01-02T21:00:00Z",
      task: "Verify the retry fixes",
    }),
    "a-lane-with-a-rather-long-name-indeed": lane({
      name: "a-lane-with-a-rather-long-name-indeed",
      harness: "claude1",
      effort: "high",
      status: "RUNNING",
      ...me,
      started: "2030-01-02T19:25:00Z",
      task: "Refactor the parser, add retries, update the docs (wave 1) ".repeat(3),
    }),
  },
  queue: [{
    name: "q",
    cwd: "/",
    prompt: "/p",
    model: "sonnet",
    effort: "high",
    budget: 200,
    globs: null,
    readonly: false,
    force: false,
    harness: "claude4",
    tools: null,
    task: "Review the importer",
    chrome: false,
    after: "b",
    waiting: "waits for b (RUNNING)",
  }],
};

Deno.test("entries: live lanes by harness, then queued; --all adds ended lanes last", () => {
  assertEquals(entries(r, false).map((e) => e.name), ["a-lane-with-a-rather-long-name-indeed", "b", "q"]);
  assertEquals(entries(r, true).map((e) => e.name).slice(-2), ["old", "stuck"]);
});

Deno.test("status --json has exactly the stable fields", () => {
  const json = JSON.parse(asJson(entries(r, false)));
  for (const row of json) {
    assertEquals(Object.keys(row), [
      "name",
      "harness",
      "model",
      "effort",
      "status",
      "task",
      "resumes",
      "after",
      "waiting",
    ]);
  }
  assertEquals(json.at(-1), {
    name: "q",
    harness: "claude4",
    model: "sonnet",
    effort: "high",
    status: "QUEUED",
    task: "Review the importer",
    resumes: 0,
    after: "b",
    waiting: "waits for b (RUNNING)",
  });
});

Deno.test("the gum board fits 120 columns and names what needs a human", async () => {
  const root = Deno.makeTempDirSync();
  saveRegistry(pathsAt(root), r);
  const out = await board(pathsAt(root), false, true, 120, NOW);
  const widest = Math.max(...out.split("\n").map((l) => [...l].length));
  assert(widest <= 120, `a line is ${widest} columns:\n${out}`);
  for (
    const cell of [
      "claude1",
      "model-large",
      "sonnet@high",
      "QUEUED",
      "2h05m",
      "waits for b (RUNNING) · Review",
      "needs a human: stuck (NO_OUTPUT)",
    ]
  ) {
    assert(out.includes(cell), `missing "${cell}":\n${out}`);
  }
});

Deno.test("the board header shows each claude account's week from the supervisor's last read", async () => {
  const f = fakeSeams({ claude: { claude1: usageText(3, 1), claude2: usageText(11, 96) } });
  const { ctx } = scratch(f.seams);
  await claudeUsages(ctx);
  const out = await board(ctx.paths, false, false, 120, NOW);
  assert(out.includes("claude1 0/4 1%w ·"), out);
  assert(out.includes("claude2 0/1 96%w ·"), out);
  assert(out.includes("claude3 0/0 ·"), "an account that reports nothing shows no percentage");
});

Deno.test("watch shows an unreadable registry and keeps going instead of exiting", async () => {
  const root = Deno.makeTempDirSync({ prefix: "lanes-watch-" });
  Deno.writeTextFileSync(`${root}/registry.json`, JSON.stringify({ lanes: {}, queue: [{ name: "x", priority: 1 }] }));
  const shown: string[] = [];
  const [log, clear] = [console.log, console.clear];
  console.log = (s: string) => shown.push(s);
  console.clear = () => {};
  const stop = new Error("stop after one frame");
  const tick = globalThis.setTimeout;
  globalThis.setTimeout = ((() => {
    throw stop;
  }) as unknown) as typeof setTimeout;
  try {
    await watch(false, 1, pathsAt(root)).catch((e) => {
      if (e !== stop) throw e;
    });
  } finally {
    [console.log, console.clear, globalThis.setTimeout] = [log, clear, tick];
  }
  assert(shown.some((s) => s.startsWith("registry unreadable, retrying:")), shown.join("\n"));
});
