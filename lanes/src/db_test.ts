import "./test_config.ts";
import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import {
  create,
  drop,
  environment,
  type LaneNote,
  locksIn,
  maintain,
  postgresSettings,
  settings,
  sweep,
  templateHead,
} from "./db.ts";
import type { Registry } from "./rules.ts";
import { fakeCluster, lane } from "./testing.ts";

const MINUTE = 60_000;
const db = settings();

/** A worktree with the `inputs` directory the template is keyed on, and a file outside it. */
function worktree(first = "one") {
  const root = Deno.makeTempDirSync({ prefix: "lanedb-wt-" });
  Deno.mkdirSync(`${root}/inputs/nested`, { recursive: true });
  Deno.writeTextFileSync(`${root}/inputs/a.sql`, first);
  Deno.writeTextFileSync(`${root}/inputs/nested/b.sql`, "second");
  Deno.writeTextFileSync(`${root}/README.md`, "not a template input");
  return { root };
}
const locks = () => locksIn(Deno.makeTempDirSync({ prefix: "lanedb-locks-" }));
const noteOf = (c: ReturnType<typeof fakeCluster>, name: string) => c.databases.get(name) as LaneNote;

Deno.test("templateHead follows the template inputs and nothing else", () => {
  const a = worktree();
  const head = templateHead(a.root);
  assertEquals(head, templateHead(worktree().root), "the same files in another worktree are the same head");
  Deno.writeTextFileSync(`${a.root}/README.md`, "changed");
  assertEquals(templateHead(a.root), head);
  for (const file of ["inputs/a.sql", "inputs/nested/b.sql"]) {
    const b = worktree();
    Deno.writeTextFileSync(`${b.root}/${file}`, "edited");
    assert(templateHead(b.root) !== head, `${file} changes the head`);
  }
  assert(templateHead(worktree("other").root) !== head, "a different file content is a different head");
});

Deno.test("a template input that is missing is refused, naming it", () => {
  const root = Deno.makeTempDirSync({ prefix: "lanedb-wt-" });
  assertThrows(() => templateHead(root), Error, "template input inputs is not in");
});

Deno.test("lanes on one head share one template, migrated once, and each gets its own database and Redis number", async () => {
  const c = fakeCluster();
  const wt = worktree().root;
  const l = locks();
  c.state.migrating = new Promise((done) => setTimeout(done, 20)); // the second lane arrives while the first migrates
  const [one, two] = await Promise.all([
    create(c.cluster, { lane: "alpha", worktree: wt }, l),
    create(c.cluster, { lane: "beta", worktree: wt }, l),
  ]);
  assertEquals(c.state.calls.filter((x) => x.startsWith("migrate")).length, 1);
  assertEquals(c.state.calls.filter((x) => x.startsWith("create")).sort(), [
    `create lane_alpha from tpl_${templateHead(wt)}`,
    `create lane_beta from tpl_${templateHead(wt)}`,
  ]);
  assertEquals(one.DATABASE_URL, `postgresql://postgres@127.0.0.1:${db.pgPort}/lane_alpha`);
  assertEquals(one.DATABASE_ADMIN_URL, `postgresql://postgres@127.0.0.1:${db.pgPort}/postgres`);
  assert(one.REDIS_URL !== two.REDIS_URL, "each lane has its own Redis database");
  assertEquals(
    [...c.databases.keys()].sort(),
    ["lane_alpha", "lane_beta", `tpl_${templateHead(wt)}`],
    "no half-built template is left",
  );
});

Deno.test("the same lane on the same head keeps its database; on a new head it gets a fresh one and keeps its Redis number", async () => {
  const c = fakeCluster();
  const wt = worktree();
  const l = locks();
  const first = await create(c.cluster, { lane: "a", worktree: wt.root }, l);
  assertEquals(await create(c.cluster, { lane: "a", worktree: wt.root }, l), first);
  assertEquals(c.state.calls.filter((x) => x.startsWith("create")).length, 1);
  Deno.writeTextFileSync(`${wt.root}/inputs/a.sql`, "edited");
  const second = await create(c.cluster, { lane: "a", worktree: wt.root }, l);
  assertEquals(second.REDIS_URL, first.REDIS_URL);
  assertEquals(noteOf(c, "lane_a").head, templateHead(wt.root));
  assertEquals(c.state.calls.filter((x) => x.startsWith("migrate")).length, 2);
});

Deno.test("a failed migration drops what it built, leaves no template, and the next lane tries again", async () => {
  const c = fakeCluster();
  const wt = worktree().root;
  const l = locks();
  c.state.failMigrate = true;
  await assertRejects(() => create(c.cluster, { lane: "a", worktree: wt }, l), Error, "migration failed");
  assertEquals([...c.databases.keys()], []);
  c.state.failMigrate = false;
  await create(c.cluster, { lane: "b", worktree: wt }, l);
  assertEquals([...c.databases.keys()].sort(), ["lane_b", `tpl_${templateHead(wt)}`]);
});

Deno.test("two lane names that map to one database name are refused, and so is a bad name", async () => {
  const c = fakeCluster();
  const wt = worktree().root;
  const l = locks();
  await create(c.cluster, { lane: "a-b", worktree: wt }, l);
  await assertRejects(() => create(c.cluster, { lane: "a_b", worktree: wt }, l), Error, "belongs to lane a-b");
  await assertRejects(() => create(c.cluster, { lane: "a b; DROP", worktree: wt }, l), Error, "must be letters");
});

Deno.test("drop removes the database and flushes its Redis number; an unknown lane is not an error", async () => {
  const c = fakeCluster();
  const wt = worktree().root;
  const l = locks();
  await create(c.cluster, { lane: "a", worktree: wt }, l);
  const redis = noteOf(c, "lane_a").redis;
  c.state.flushed.length = 0;
  assertEquals(await drop(c.cluster, "a", l), "lane_a: dropped");
  assertEquals(c.state.flushed, [redis]);
  assertEquals(await drop(c.cluster, "a", l), "lane_a: none");
});

const registry = (...status: [string, Registry["lanes"][string]["status"]][]): Registry => ({
  lanes: Object.fromEntries(status.map(([name, s]) => [name, lane({ name, status: s })])),
  queue: [],
});

Deno.test("the sweep drops the databases of lanes that ended for good and keeps those that may be resumed", async () => {
  const c = fakeCluster();
  const wt = worktree().root;
  const l = locks();
  for (const name of ["done", "stopped", "running", "errored"]) {
    await create(c.cluster, { lane: name, worktree: wt }, l);
  }
  const dropped = await sweep(
    c.cluster,
    registry(["done", "DONE"], ["stopped", "STOPPED"], ["running", "RUNNING"], ["errored", "ERROR"]),
    new Date(),
    l,
  );
  assertEquals(dropped.sort(), ["lane_done (DONE)", "lane_stopped (STOPPED)"]);
  assertEquals([...c.databases.keys()].filter((n) => n.startsWith("lane_")).sort(), ["lane_errored", "lane_running"]);
});

Deno.test("a lane the registry does not know is dropped only once it is orphanHours old", async () => {
  const c = fakeCluster();
  const l = locks();
  const now = new Date("2030-01-02T12:00:00Z");
  const note = (created: string): LaneNote => ({
    lane: "manual",
    head: "0".repeat(16),
    redis: 1,
    worktree: "/w",
    created,
  });
  c.databases.set("lane_manual", note("2030-01-01T13:00:00Z")); // 23 h
  assertEquals(await sweep(c.cluster, registry(), now, l), []);
  c.databases.set("lane_manual", note("2030-01-01T11:00:00Z")); // 25 h
  assertEquals(await sweep(c.cluster, registry(), now, l), ["lane_manual (lane not in the registry)"]);
});

Deno.test("a template is kept while a database uses it and for templateKeepMinutes after, then dropped", async () => {
  const c = fakeCluster();
  const l = locks();
  const head = "a".repeat(16);
  const t0 = new Date("2030-01-02T12:00:00Z");
  c.databases.set(`tpl_${head}`, { head, used: t0.toISOString() });
  const lateBy = (min: number) => new Date(t0.getTime() + min * MINUTE);
  assertEquals(await sweep(c.cluster, registry(), lateBy(db.templateKeepMinutes - 1), l), []);
  c.databases.set("lane_x", { lane: "x", head, redis: 1, worktree: "/w", created: t0.toISOString() });
  assertEquals(await sweep(c.cluster, registry(["x", "RUNNING"]), lateBy(db.templateKeepMinutes + 1), l), []);
  assertEquals(await sweep(c.cluster, registry(["x", "DONE"]), lateBy(db.templateKeepMinutes + 1), l), [
    "lane_x (DONE)",
    `tpl_${head} (unused)`,
  ]);
  assertEquals([...c.databases.keys()], []);
});

Deno.test("the sweep drops a template left half-built, but not while its build holds the lock", async () => {
  const c = fakeCluster();
  const l = locks();
  const head = "b".repeat(16);
  c.databases.set(`tplb_${head}_1234abcd`, null);
  const building = Promise.withResolvers<void>();
  const held = import("./registry.ts").then(({ lockFile }) => lockFile(l.template(head), () => building.promise));
  await new Promise((done) => setTimeout(done, 10));
  const swept = sweep(c.cluster, registry(), new Date(), l);
  await new Promise((done) => setTimeout(done, 20));
  assertEquals(c.state.calls.filter((x) => x.startsWith("drop")), [], "waits for the build");
  building.resolve();
  await held;
  assertEquals(await swept, [`tplb_${head}_1234abcd (half-built)`]);
});

Deno.test("maintain does nothing for a server nobody started, and restarts one that died", async () => {
  const c = fakeCluster();
  const l = locks();
  assertEquals(await maintain(c.cluster, registry(), l), []);
  assertEquals(c.state.calls, []);
  c.state.pidfile = true; // started once, killed without a clean stop
  await maintain(c.cluster, registry(), l);
  assertEquals(c.state.calls, ["ensureUp"]);
});

Deno.test("the server settings are for disposable data and sized from the config", () => {
  const settings = postgresSettings();
  for (const s of ["fsync=off", "synchronous_commit=off", "full_page_writes=off", `port=${db.pgPort}`]) {
    assert(settings.includes(s), s);
  }
  assert(settings.includes(`max_connections=${db.maxConnections}`));
});

Deno.test("environment: the default variables, a configured set, and Redis left out when it is not configured", () => {
  assertEquals(environment("lane_a", 7), {
    DATABASE_URL: `postgresql://postgres@127.0.0.1:${db.pgPort}/lane_a`,
    DATABASE_ADMIN_URL: `postgresql://postgres@127.0.0.1:${db.pgPort}/postgres`,
    REDIS_URL: `redis://127.0.0.1:${db.redis!.port}/7`,
  });
  const custom = { ...db, env: { DB: "{pg}/{database}", CACHE: "{redis}" } };
  assertEquals(Object.keys(environment("lane_a", 7, custom)), ["DB", "CACHE"]);
  const { redis: _, ...noRedis } = db;
  assertEquals(Object.keys(environment("lane_a", 0, noRedis)), ["DATABASE_URL", "DATABASE_ADMIN_URL"]);
});
