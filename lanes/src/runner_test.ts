import "./test_config.ts";
import { assert, assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { loadRegistry, procStat, withRegistry } from "./registry.ts";
import { config } from "./rules.ts";
import * as run from "./runner.ts";
import { claudeResult, fakeSeams, scratch, usageText } from "./testing.ts";

const args = (name: string, prompt: string, cwd: string, over: Partial<run.StartArgs> = {}): run.StartArgs => ({
  name,
  cwd,
  prompt,
  model: "sonnet",
  effort: "high",
  budget: 200,
  globs: null,
  readonly: false,
  force: false,
  harness: "claude4",
  tools: null,
  task: "demo",
  chrome: false,
  ...over,
});

Deno.test("start launches through the seam and records the lane", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  f.state.running = true;
  await withRegistry(ctx.paths, (r) => run.start(args("a", prompt, root, { chrome: true }), r, ctx, null));
  const [l] = f.launched;
  const lane = loadRegistry(ctx.paths).lanes.a;
  assertEquals(l.argv.slice(0, 6), ["nice", "-n", "10", "timeout", "14400", "claude"]);
  assertEquals(l.argv.slice(l.argv.indexOf("--allowedTools") + 1).slice(0, 8).at(-1), "mcp__claude-in-chrome");
  assertEquals(l.argv.at(-1), lane.session);
  assertEquals(l.env.CLAUDE_CONFIG_DIR?.endsWith("/.claude.account4"), true);
  assertEquals([lane.status, lane.attempts, lane.marker, lane.out], [
    "RUNNING",
    1,
    "Fix the failing parser test, then report.",
    `${root}/a.1.json`,
  ]);
  assertEquals(lane.pid_started, procStat(lane.pid!)!.started);
  await f.cleanup();
});

Deno.test("a lane starts with the configured lane environment", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  await withRegistry(ctx.paths, (r) => run.start(args("a", prompt, root), r, ctx, null));
  assertEquals(f.launched[0].env.LANES_TEST, "1");
});

Deno.test("start refuses: missing prompt, a banned model, an existing name, a full account", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  const refusal = (a: run.StartArgs) =>
    withRegistry(ctx.paths, (r) => run.start(a, r, ctx, null)).then(
      () => null,
      (e: run.Refusal) => [e.retryable, e.message],
    );
  assertEquals((await refusal(args("x", `${root}/nope`, root)))?.[1], `REFUSED: prompt ${root}/nope does not exist`);
  assertStringIncludes(
    String(await refusal(args("x", prompt, root, { model: "provider-a/model-retired", force: true }))),
    "may not be used",
  );
  f.state.running = true;
  for (const n of ["r1", "r2", "r3", "r4", "r5"]) assertEquals(await refusal(args(n, prompt, root)), null);
  assertEquals(await refusal(args("r6", prompt, root)), [
    true,
    "REFUSED: claude4 already runs 5 lanes (cap 5); wait or --force",
  ]);
  assertEquals(await refusal(args("r6", prompt, root, { force: true })), null);
  assertEquals((await refusal(args("r1", prompt, root, { force: true })))?.[1], "r1 already running");
  await f.cleanup();
  f.state.running = false;
  await withRegistry(ctx.paths, () => {}); // r1..r6 have ended now
  assertStringIncludes(String(await refusal(args("r1", prompt, root))), "already exists");
});

Deno.test("a tick drains the queue: machine gate, two starts a tick, --after, --light, release", async () => {
  const f = fakeSeams({ busy: "the machine is busy (load 30, starts under 24)" });
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.running = true;
  await withRegistry(ctx.paths, (r) => {
    run.queue({ ...args("gated", prompt, root) }, r, ctx);
    run.queue({ ...args("light", prompt, root), light: true }, r, ctx);
    run.queue(
      {
        ...args("after", prompt, root, { harness: "omp", model: "provider-a/model-large", force: true }),
        after: "light",
      },
      r,
      ctx,
    );
  });
  await run.tick(3, ctx);
  let r = loadRegistry(ctx.paths);
  assertEquals(r.queue.map((q) => [q.name, q.waiting]), [
    ["gated", "the machine is busy (load 30, starts under 24)"],
    ["after", "waits for light (RUNNING)"],
  ]);
  assertEquals(Object.keys(r.lanes), ["light"]);
  await withRegistry(ctx.paths, (r) => run.release("after", r, ctx));
  await run.tick(3, ctx);
  r = loadRegistry(ctx.paths);
  assertEquals(Object.keys(r.lanes).sort(), ["after", "light"]);
  assertEquals(f.launched[1].argv.slice(5, 6), ["omp"]);
  assert(said.some((s) => s.endsWith("after: dequeued and started")));

  const g = fakeSeams();
  const s = scratch(g.seams);
  g.state.running = true;
  await withRegistry(
    s.ctx.paths,
    (r) => ["b1", "b2", "b3"].forEach((n) => run.queue(args(n, s.prompt, s.root), r, s.ctx)),
  );
  await run.tick(3, s.ctx);
  assertEquals(loadRegistry(s.ctx.paths).queue.map((q) => [q.name, q.waiting]), [["b3", "2 starts per tick"]]);
  await f.cleanup();
  await g.cleanup();
});

Deno.test("queue stores absolute paths and refuses a duplicate or a banned model", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  const cwd = Deno.cwd();
  Deno.chdir(root);
  try {
    await withRegistry(ctx.paths, (r) => run.queue(args("q", "prompt.txt", "."), r, ctx));
  } finally {
    Deno.chdir(cwd);
  }
  assertEquals(loadRegistry(ctx.paths).queue[0].prompt, `${root}/prompt.txt`);
  await assertRejects(
    () => withRegistry(ctx.paths, (r) => run.queue(args("q", prompt, root), r, ctx)),
    run.Refusal,
    "already exists or is queued",
  );
  await assertRejects(
    () =>
      withRegistry(ctx.paths, (r) => run.queue(args("m", prompt, root, { model: "provider-a/model-retired" }), r, ctx)),
    run.Refusal,
    "may not be used",
  );
});

Deno.test("a tick resumes an early end with the resume prompt, retries a lifted limit, and gives up after max resumes", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.output = claudeResult("I'll wait for the gate.");
  await withRegistry(ctx.paths, (r) => run.start(args("early", prompt, root), r, ctx, null));
  f.state.output = claudeResult("You've hit your session limit · resets 9:20am (UTC)", { is_error: true });
  await withRegistry(ctx.paths, (r) => run.start(args("limited", prompt, root), r, ctx, null));
  f.state.output = claudeResult("I'll wait for the gate.");
  await run.tick(1, ctx);
  const early = f.launched[2];
  assertEquals(early.argv.slice(early.argv.indexOf("-p") + 1)[0], config.resumePrompt);
  assertEquals(early.argv.slice(-2), ["--resume", loadRegistry(ctx.paths).lanes.early.session!]);
  assert(said.some((s) => /limited: waits for the account limit to lift at \d\d:\d\dZ, then resumes/.test(s)));
  await run.tick(1, ctx); // early ended early again, with its one resume spent
  await run.tick(1, ctx);
  assertEquals(
    said.filter((s) => s.endsWith("early: EARLY_END needs a human (resumes exhausted or no session)")).length,
    1,
  );

  const out = loadRegistry(ctx.paths).lanes.limited.out!;
  Deno.writeTextFileSync(out, claudeResult("You've hit your session limit · resets soon", { is_error: true }));
  Deno.utimeSync(out, new Date(), new Date(Date.now() - 16 * 60_000)); // died 16 minutes ago, no reset time stated
  await run.tick(1, ctx);
  assert(said.some((s) => s.endsWith("limited: rate-limited earlier, retrying")));
  assertEquals(loadRegistry(ctx.paths).lanes.limited.resumes, 1);
});

Deno.test("stop, forget; a running lane cannot be forgotten", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  f.state.running = true;
  await withRegistry(ctx.paths, (r) => run.start(args("a", prompt, root), r, ctx, null));
  await assertRejects(() => withRegistry(ctx.paths, (r) => run.forget("a", r, ctx)), run.Refusal, "stop it first");
  await f.cleanup();
  await withRegistry(ctx.paths, (r) => run.stop({ name: "a", reason: "test" }, r, ctx));
  assertEquals(loadRegistry(ctx.paths).lanes.a.status, "STOPPED");
  await run.tick(3, ctx);
  assertEquals(f.launched.length, 1, "a stopped lane is never resumed");
  await withRegistry(ctx.paths, (r) => run.forget("a", r, ctx));
  assertEquals(Object.keys(loadRegistry(ctx.paths).lanes), []);
});

Deno.test("the real launcher: own session, output to the file, reaped when done, stopped by process group", async () => {
  const { root, prompt, ctx } = scratch(run.realSeams);
  const env = Deno.env.toObject();
  const pid = run.realSeams.launch(["sh", "-c", "echo hello; echo oops >&2"], {
    cwd: root,
    env,
    out: `${root}/o.json`,
  });
  await new Promise((done) => setTimeout(done, 300));
  assertEquals(Deno.readTextFileSync(`${root}/o.json`), "hello\noops\n");
  assertEquals(procStat(pid), null, "a finished child is reaped, not left a zombie");

  const seams = {
    ...run.realSeams,
    launch: (_argv: string[], o: Parameters<run.Seams["launch"]>[1]) => run.realSeams.launch(["sleep", "60"], o),
  };
  const c = { ...ctx, seams };
  await withRegistry(ctx.paths, (r) => run.start(args("s", prompt, root), r, c, null));
  const l = loadRegistry(ctx.paths).lanes.s;
  const stat = Deno.readTextFileSync(`/proc/${l.pid}/stat`).split(") ")[1].split(" ");
  assertEquals([stat[2], stat[3]], [String(l.pid), String(l.pid)], "it leads its own process group and session");
  await withRegistry(ctx.paths, (r) => run.stop({ name: "s" }, r, c));
  await new Promise((done) => setTimeout(done, 300));
  assertEquals(procStat(l.pid!), null);
});

/** A process whose command line names `supervise`, as a running supervisor's does. */
const fakeSupervisor = (seconds: number) =>
  new Deno.Command(Deno.execPath(), {
    args: ["eval", `await new Promise((done) => setTimeout(done, ${seconds * 1000}))`, "supervise"],
  }).spawn();

Deno.test("supervisor restart signals the old one under the registry lock, waits for it, and starts the new one", async () => {
  const f = fakeSeams();
  const { ctx, said } = scratch(f.seams);
  const stranger = new Deno.Command("sleep", { args: ["60"] }).spawn();
  Deno.writeTextFileSync(ctx.paths.pidfile, String(stranger.pid)); // a recycled pid: alive, but not a supervisor
  await run.supervisor("stop", [], ctx);
  assertEquals([run.supervisorPid(ctx.paths), said.length, procStat(stranger.pid) !== null], [null, 0, true]);
  stranger.kill();
  await stranger.status;
  const old = fakeSupervisor(60);
  Deno.writeTextFileSync(ctx.paths.pidfile, String(old.pid));
  const self = ["deno", "run", "-A", "main.ts", "supervise", "--interval", "120", "--max-resumes", "3"];
  await run.supervisor("restart", self, ctx);
  assertEquals((await old.status).signal, "SIGTERM");
  assertEquals(f.launched.map((l) => [l.argv, l.out, l.cwd]), [[self, ctx.paths.log, ctx.paths.root]]);
  assertEquals(said.slice(0, 1), [`stopped supervisor pid ${old.pid}`]);
});

Deno.test("supervise refuses to start beside a live supervisor", async () => {
  const { ctx } = scratch(fakeSeams().seams);
  const other = fakeSupervisor(30);
  Deno.writeTextFileSync(ctx.paths.pidfile, String(other.pid));
  await assertRejects(
    () => run.supervise(1, 3, ctx),
    run.Refusal,
    `a supervisor already runs (pid ${other.pid})`,
  );
  other.kill();
  await other.status;
});

Deno.test("the queue keeps a lane under the quota floor or with its quota unread, and drops one that cannot start", async () => {
  const f = fakeSeams({ usage: "" });
  const { root, prompt, ctx } = scratch(f.seams);
  const capped = { harness: "omp" as const, model: "provider-a/model-large", tools: "ro" };
  const gone = `${root}/gone.txt`;
  Deno.writeTextFileSync(gone, "A brief that is deleted before the queue reaches it");
  await withRegistry(ctx.paths, (r) => {
    run.queue(args("floor", prompt, root, capped), r, ctx);
    run.queue({ ...args("vanished", gone, root), light: true }, r, ctx);
  });
  Deno.removeSync(gone);
  await run.tick(3, ctx);
  const r = loadRegistry(ctx.paths);
  assertEquals(
    r.queue.map((q) => [q.name, q.waiting?.startsWith("REFUSED (quota floor): cannot read Provider A quota")]),
    [["floor", true]],
  );
  await withRegistry(ctx.paths, (r) => run.drain(r, new Map(), ctx));
  assertEquals(
    loadRegistry(ctx.paths).queue[0].waiting,
    "REFUSED (quota floor): quota not read yet (queued during this tick)",
  );
  assertEquals(f.launched.length, 0);
});

Deno.test("start waits while the account is at its stated limit", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  const at = new Date(Date.now() + 2 * 3600_000);
  const h = at.getUTCHours();
  const resets = `resets ${h % 12 || 12}:${String(at.getUTCMinutes()).padStart(2, "0")}${h < 12 ? "am" : "pm"} (UTC)`;
  f.state.output = claudeResult(`You've hit your session limit · ${resets}`, { is_error: true });
  await withRegistry(ctx.paths, (r) => run.start(args("limited", prompt, root), r, ctx, null));
  f.state.output = "";
  const refused = await withRegistry(ctx.paths, (r) => run.start(args("next", prompt, root), r, ctx, null))
    .then(() => null, (e: run.Refusal) => [e.retryable, e.message.replace(/\d\d:\d\dZ/, "HH:MMZ")]);
  assertEquals(refused, [true, "REFUSED: the claude4 account is at its limit until HH:MMZ; queue it, or --force"]);
});

Deno.test("a relaunch clears stopped and announced and keeps chrome and created; a resume keeps the cost so far", async () => {
  const f = fakeSeams();
  const { root, prompt, ctx } = scratch(f.seams);
  f.state.output = claudeResult("I'll wait.", { total_cost_usd: 1.5 });
  await withRegistry(ctx.paths, (r) => run.start(args("a", prompt, root, { chrome: true }), r, ctx, null));
  await withRegistry(ctx.paths, (r) => run.stop({ name: "a" }, r, ctx));
  const before = loadRegistry(ctx.paths).lanes.a;
  await withRegistry(ctx.paths, (r) => run.start(args("a", prompt, root, { force: true }), r, ctx, null));
  const a = loadRegistry(ctx.paths).lanes.a;
  assertEquals([a.chrome, a.created, a.attempts, "stopped" in a, "announced" in a], [
    true,
    before.created,
    2,
    false,
    false,
  ]);
  await withRegistry(ctx.paths, (r) => run.resume({ name: "a", budget: 7 }, r, ctx, null));
  const resumed = loadRegistry(ctx.paths).lanes.a;
  assertEquals([resumed.cost_prev, resumed.budget, resumed.resumes, f.launched.at(-1)!.argv.at(-2)], [
    1.5,
    7,
    1,
    "--resume",
  ]);
});

Deno.test("a resume passes the machine gate and the per-tick start limit, like a queued lane", async () => {
  const busy = { why: "the machine is busy (load 119, starts under 24)" as string | null };
  const f = fakeSeams();
  f.seams.machineBusy = () => busy.why;
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.output = claudeResult("I'll wait.");
  for (const n of ["a", "b", "c"]) await withRegistry(ctx.paths, (r) => run.start(args(n, prompt, root), r, ctx, null));
  await run.tick(3, ctx);
  await run.tick(3, ctx);
  assertEquals(f.launched.length, 3, "no resume while the machine is busy");
  assertEquals(said.filter((s) => s.includes("resume waits: the machine is busy")).length, 3, "said once per lane");
  busy.why = null;
  await run.tick(3, ctx);
  assertEquals(f.launched.length, 3 + config.machine.startsPerTick);
  assert(said.some((s) => s.endsWith(`resume waits: ${config.machine.startsPerTick} starts per tick`)));
});

Deno.test("stop signals the lane's whole process group", async () => {
  const { root, prompt, ctx } = scratch(run.realSeams);
  const seams = {
    ...run.realSeams,
    launch: (_: string[], o: Parameters<run.Seams["launch"]>[1]) =>
      run.realSeams.launch(["sh", "-c", "sleep 60 & sleep 60"], o),
  };
  const c = { ...ctx, seams };
  await withRegistry(ctx.paths, (r) => run.start(args("g", prompt, root), r, c, null));
  const pid = loadRegistry(ctx.paths).lanes.g.pid!;
  await new Promise((done) => setTimeout(done, 300));
  const group = () =>
    [...Deno.readDirSync("/proc")].filter((e) => /^\d+$/.test(e.name)).filter((e) => {
      const st = procStat(Number(e.name));
      const f = st && Deno.readTextFileSync(`/proc/${e.name}/stat`).split(") ").at(-1)!.split(" ");
      return f && f[2] === String(pid) && st!.state !== "Z";
    }).length;
  assertEquals(group(), 3, "sh and its two sleeps");
  await withRegistry(ctx.paths, (r) => run.stop({ name: "g" }, r, c));
  await new Promise((done) => setTimeout(done, 300));
  assertEquals(group(), 0);
});

Deno.test("a supervisor waiting for the lock leaves at once on SIGTERM, without a pass", async () => {
  const { ctx } = scratch(fakeSeams().seams);
  const holder = new Deno.Command("flock", {
    args: ["-x", ctx.paths.lock, "sh", "-c", "echo held; sleep 8"],
    stdout: "piped",
  }).spawn();
  const reader = holder.stdout.getReader();
  await reader.read();
  reader.releaseLock();
  const sup = new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", new URL("main.ts", import.meta.url).pathname, "supervise", "--interval", "60"],
    env: { LANES_ROOT: ctx.paths.root },
    stdout: "piped",
  }).spawn();
  await new Promise((done) => setTimeout(done, 1500));
  const t = performance.now();
  sup.kill("SIGTERM");
  const st = await sup.status;
  assert(performance.now() - t < 3000, "it left while the lock was still held");
  assertEquals([st.success, Deno.statSync(ctx.paths.registry.replace("registry.json", "")).isDirectory], [true, true]);
  assertEquals(loadRegistry(ctx.paths), { lanes: {}, queue: [] }, "no pass ran, so nothing was saved");
  holder.kill();
  await holder.status;
  await holder.stdout.cancel().catch(() => {});
});

Deno.test("the CLI exits 1 with the refusal, 2 with the usage", async () => {
  const { root, prompt } = scratch(fakeSeams().seams);
  const cli = async (...a: string[]) => {
    const main = new URL("main.ts", import.meta.url).pathname;
    const out = await new Deno.Command(Deno.execPath(), {
      args: ["run", "-A", main, ...a],
      env: { LANES_ROOT: root },
      stderr: "piped",
      stdout: "null",
    }).output();
    return [out.code, new TextDecoder().decode(out.stderr).split("\n")[0]];
  };
  const q = ["queue", "q", "--cwd", root, "--prompt", prompt];
  assertEquals(await cli(...q), [0, ""]);
  assertEquals(await cli(...q), [1, "REFUSED: q already exists or is queued"]);
  assertEquals(await cli("resume"), [2, "resume takes one argument"]);
  assertEquals((await cli("status", "--bogus"))[0], 2);
  assertEquals(await cli("start", "x"), [2, "--cwd and --prompt are required"]);
});

// claude1 cap 4, claude2 cap 1, claude4 cap 5, claude5 cap 4 (claude3 cap 0); the weekly floor is 90%
const accounts = (week: Record<string, number>, session = 5) =>
  Object.fromEntries(Object.entries(week).map(([h, w]) => [h, usageText(session, w)]));
const waiting = (ctx: { paths: ReturnType<typeof scratch>["ctx"]["paths"] }) =>
  loadRegistry(ctx.paths).queue.map((q) => [q.name, q.harness, q.waiting]);
const expireUsage = (ctx: { paths: { claudeUsage: string } }) => Deno.removeSync(ctx.paths.claudeUsage);

Deno.test("a queued lane on an account at the weekly floor moves to the one with the most headroom, and says so", async () => {
  const f = fakeSeams({ claude: accounts({ claude1: 1, claude2: 96, claude4: 36, claude5: 46 }) });
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.running = true;
  await withRegistry(ctx.paths, (r) => run.queue(args("q", prompt, root, { harness: "claude2" }), r, ctx));
  await run.tick(3, ctx);
  assertEquals(loadRegistry(ctx.paths).lanes.q.harness, "claude1");
  assert(said.some((s) => s.includes("q: moved claude2 -> claude1 (week 1% used): claude2 is at its floor: week 96%")));
  assertEquals(f.launched[0].env.CLAUDE_CONFIG_DIR?.endsWith("/.claude.account1"), true);
  await f.cleanup();
});

Deno.test("a queued lane waits with the reason and the reset when no other account is free, under both floors, with a slot", async () => {
  const caps = config.harnesses;
  const f = fakeSeams({ claude: accounts({ claude1: 10, claude2: 96, claude4: 95, claude5: 20 }) });
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.running = true;
  const was = [caps.claude1.cap, caps.claude5.cap];
  [caps.claude1.cap, caps.claude5.cap] = [1, 0]; // claude1 full, claude5 takes no lanes, claude4 over the floor
  try {
    await withRegistry(ctx.paths, (r) => {
      run.start(args("busy", prompt, root, { harness: "claude1" }), r, ctx, null);
      run.queue(args("q", prompt, root, { harness: "claude2" }), r, ctx);
    });
    await run.tick(3, ctx);
  } finally {
    [caps.claude1.cap, caps.claude5.cap] = was;
  }
  const [[, harness, why]] = waiting(ctx);
  assertEquals(harness, "claude2");
  assertEquals(
    why,
    "REFUSED (quota floor): claude2 is at its floor: week 96% used (stops at 90%), resets Jan 3, 9:59am (UTC)",
  );
  assertEquals(f.launched.length, 1, "only the lane that was already running");
  assert(!said.some((s) => s.includes("moved")));
  await f.cleanup();
});

Deno.test("the 5-hour floor holds a lane like the weekly one; --force and an unreadable account do not", async () => {
  const f = fakeSeams({ claude: { ...accounts({ claude1: 1, claude2: 1, claude4: 1, claude5: 1 }, 95), claude5: "" } });
  const { root, prompt, ctx } = scratch(f.seams);
  f.state.running = true;
  await withRegistry(ctx.paths, (r) => {
    run.queue(args("held", prompt, root, { harness: "claude2" }), r, ctx);
    run.queue(args("forced", prompt, root, { harness: "claude2", force: true }), r, ctx);
    run.queue(args("blind", prompt, root, { harness: "claude5" }), r, ctx);
  });
  await run.tick(3, ctx);
  assertEquals(waiting(ctx).map(([n, h]) => [n, h]), [["held", "claude2"]]);
  assertEquals(String(loadRegistry(ctx.paths).queue[0].waiting).includes("5-hour session 95%"), true);
  assertEquals(
    Object.values(loadRegistry(ctx.paths).lanes).map((l) => [l.name, l.harness]).sort(),
    [["blind", "claude5"], ["forced", "claude2"]],
  );
  await f.cleanup();
});

Deno.test("a resume stays on its account and waits at the floor instead of starting into the limit", async () => {
  const f = fakeSeams({ claude: accounts({ claude1: 1, claude2: 96, claude4: 1, claude5: 1 }) });
  const { root, prompt, ctx, said } = scratch(f.seams);
  f.state.output = claudeResult("I'll wait.");
  await withRegistry(ctx.paths, (r) => run.start(args("early", prompt, root, { harness: "claude2" }), r, ctx, null));
  await run.tick(3, ctx);
  await run.tick(3, ctx);
  assertEquals(f.launched.length, 1, "no resume, and no move to another account");
  assertEquals(said.filter((s) => s.includes("early: EARLY_END, resume waits: claude2 is at its floor")).length, 1);
  expireUsage(ctx);
  f.seams.claudeUsage = (h) => Promise.resolve(usageText(5, h === "claude2" ? 10 : 1));
  await run.tick(3, ctx);
  assertEquals(f.launched.length, 2);
  assert(said.some((s) => s.endsWith("early: EARLY_END -> resume #1")));
});

Deno.test("a manual resume refuses at its account's floor with the reason and the reset, unless --force", async () => {
  const f = fakeSeams({ claude: accounts({ claude1: 1, claude2: 96, claude4: 1, claude5: 1 }) });
  const { root, prompt, ctx } = scratch(f.seams);
  f.state.output = claudeResult("I'll wait.");
  await withRegistry(ctx.paths, (r) => run.start(args("early", prompt, root, { harness: "claude2" }), r, ctx, null));
  await run.tick(0, ctx); // ends early; no resumes allowed, so the supervisor leaves it
  const resumeBy = async (force: boolean) => {
    const why = await run.resumePrecheck({ name: "early", force }, ctx);
    return withRegistry(ctx.paths, (r) => run.resume({ name: "early", budget: 7 }, r, ctx, why));
  };
  await assertRejects(
    () => resumeBy(false),
    run.Refusal,
    "REFUSED (quota floor): claude2 is at its floor: week 96% used (stops at 90%), resets Jan 3, 9:59am (UTC)",
  );
  assertEquals(f.launched.length, 1, "refused, nothing launched");
  await resumeBy(true);
  assertEquals(f.launched.length, 2);
  assertEquals(await run.resumePrecheck({ name: "nobody", force: false }, ctx), null);
});

Deno.test("each account is read once per cache window, in a tick or across ticks, failures too", async () => {
  const f = fakeSeams({ claude: accounts({ claude1: 1, claude2: 1, claude4: 1 }) });
  const { ctx } = scratch(f.seams);
  await run.tick(3, ctx);
  await run.tick(3, ctx);
  assertEquals(f.claudeReads.sort(), ["claude1", "claude2", "claude4", "claude5"], "claude3 takes no lanes: not read");
  const cache = run.cachedUsage(ctx.paths);
  assertEquals(cache.claude5.usage, null, "the unreadable account is remembered as such");
  cache.claude2.at -= config.claudeQuota.cacheMinutes * 60_000 + 1;
  Deno.writeTextFileSync(ctx.paths.claudeUsage, JSON.stringify(cache));
  await run.tick(3, ctx);
  assertEquals(f.claudeReads.length, 5);
  assertEquals(f.claudeReads.at(-1), "claude2");
});

Deno.test("`lanes quota` lists each claude account's session and week beside its verdict; a direct start is refused at the floor", async () => {
  const f = fakeSeams({ claude: accounts({ claude1: 1, claude2: 96, claude4: 36, claude5: 46 }, 11) });
  const { ctx, said } = scratch(f.seams);
  await run.quota(ctx);
  assert(said.includes(
    "claude2: session 11% (resets Jan 2, 11:29am (UTC)) · week 96% (resets Jan 3, 9:59am (UTC)) -> STOP (at the floor)",
  ));
  assert(said.some((s) => s.startsWith("claude1: session 11%") && s.endsWith("-> ok")));
  assert(said.some((s) => s.startsWith("claude floors: no lane starts at 90% of the week")));
  const a = { harness: "claude2", model: "sonnet", force: false };
  assertEquals((await run.quotaPrecheck(a, ctx))?.startsWith("claude2 is at its floor"), true);
  assertEquals(await run.quotaPrecheck({ ...a, force: true }, ctx), null);
  assertEquals(await run.quotaPrecheck({ ...a, harness: "claude1" }, ctx), null);
});
