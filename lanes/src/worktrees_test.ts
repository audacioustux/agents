import "./test_config.ts";
import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import type { Registry } from "./rules.ts";
import { fill, members, type PoolSettings, recyclable, take } from "./worktrees.ts";
import { lane } from "./testing.ts";

const sh = async (cwd: string, ...argv: string[]) => {
  const [cmd, ...args] = argv;
  const out = await new Deno.Command(cmd, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (!out.success) throw new Error(`${argv.join(" ")}: ${new TextDecoder().decode(out.stderr)}`);
  return new TextDecoder().decode(out.stdout).trim();
};
const git = (cwd: string, ...args: string[]) => sh(cwd, "git", "-c", "user.name=t", "-c", "user.email=t@t.t", ...args);

/** A repository whose `origin/main` has one commit, with `deps/` ignored: pool settings that point at it. */
async function repo(over: Partial<PoolSettings> = {}) {
  const root = Deno.realPathSync(Deno.makeTempDirSync({ prefix: "pool-" }));
  const [origin, work] = [`${root}/origin.git`, `${root}/repo`];
  await sh(root, "git", "init", "-q", "--bare", "-b", "main", origin);
  await sh(root, "git", "clone", "-q", origin, work);
  Deno.writeTextFileSync(`${work}/.gitignore`, "deps/\n");
  Deno.writeTextFileSync(`${work}/file.txt`, "base");
  await git(work, "add", ".");
  await git(work, "commit", "-q", "-m", "base");
  await git(work, "push", "-q", "origin", "HEAD:main");
  const s: PoolSettings = {
    repo: work,
    dir: "wt",
    remote: "origin",
    branch: "main",
    installCommand: ["sh", "-c", "touch .installed"],
    installedMarker: "deps",
    keep: ["^keep-"],
    ...over,
  };
  const add = async (name: string) => {
    await git(work, "worktree", "add", "-q", "-b", name, `${work}/wt/${name}`, "origin/main");
    Deno.mkdirSync(`${work}/wt/${name}/deps`);
    Deno.writeTextFileSync(`${work}/wt/${name}/deps/pkg`, name);
    return `${work}/wt/${name}`;
  };
  return { s, work, add };
}
const noLanes: Registry = { lanes: {}, queue: [] };

Deno.test("take with an empty pool adds a fresh worktree on the branch and runs the install command", async () => {
  const { s, work } = await repo();
  const path = `${work}/wt/new`;
  assertEquals(await take(s, path, { branch: "lane/new" }), `took a fresh worktree: ${path}`);
  assertEquals(await git(path, "branch", "--show-current"), "lane/new");
  assert(Deno.statSync(`${path}/.installed`).isFile);
  await assertRejects(() => take(s, path, { branch: "again" }), Error, "exists");
});

Deno.test("fill pools what is clean, installed and merged, and take hands it out with its ignored files", async () => {
  const { s, work, add } = await repo();
  const done = await add("done");
  assertEquals(await recyclable(s, noLanes), [done]);
  assertEquals(await fill(s, noLanes), ["done"]);
  assertEquals(members(s).length, 1);
  assertEquals(await git(members(s)[0], "branch", "--show-current"), "", "pooled worktrees are detached");

  Deno.writeTextFileSync(`${members(s)[0]}/stray.txt`, "untracked");
  const path = `${work}/wt/next`;
  assertEquals(await take(s, path, { branch: "lane/next" }), `took pooled worktree -> ${path}`);
  assertEquals(members(s), []);
  assertEquals(await git(path, "branch", "--show-current"), "lane/next");
  assertEquals(Deno.readTextFileSync(`${path}/deps/pkg`), "done", "ignored files survive");
  assertThrows(() => Deno.statSync(`${path}/stray.txt`), Deno.errors.NotFound, "", "untracked files do not");
  assert(Deno.statSync(`${path}/.installed`).isFile, "the install command ran again");
});

Deno.test("fill leaves a worktree that is dirty, unmerged, not installed, kept by name or named by a brief", async () => {
  const { s, work, add } = await repo();
  const dirty = await add("dirty");
  Deno.writeTextFileSync(`${dirty}/file.txt`, "edited");
  const ahead = await add("ahead");
  Deno.writeTextFileSync(`${ahead}/new.txt`, "x");
  await git(ahead, "add", "new.txt");
  await git(ahead, "commit", "-q", "-m", "not on the base branch");
  Deno.removeSync(`${await add("bare")}/deps`, { recursive: true });
  for (const name of ["keep-me", "running-brief", "queued-brief"]) await add(name);
  const free = await add("free");
  const brief = (name: string) => {
    const path = `${work}/${name}.txt`;
    Deno.writeTextFileSync(path, `Work in ${work}/wt/${name} and report.`);
    return path;
  };
  const registry: Registry = {
    lanes: { b: lane({ name: "b", status: "RUNNING", prompt: brief("running-brief") }) },
    queue: [queueItem(brief("queued-brief"))],
  };
  assertEquals(await recyclable(s, registry), [free]);
  assertEquals(
    (await recyclable(s, noLanes)).length,
    3,
    "free, running-brief and queued-brief, once no lane names them",
  );
});

Deno.test("a take that cannot check out its branch puts the worktree back in the pool", async () => {
  const { s, work, add } = await repo();
  await add("done");
  await fill(s, noLanes);
  const path = `${work}/wt/next`;
  await assertRejects(() => take(s, path, { branch: "main" }), Error, "main");
  assertEquals(members(s).length, 1);
  assertThrows(() => Deno.statSync(path), Deno.errors.NotFound);
});

const queueItem = (prompt: string) => ({
  name: "q",
  cwd: "/",
  prompt,
  model: "m",
  effort: "high",
  budget: 1,
  globs: null,
  readonly: false,
  force: false,
  harness: "claude1",
  tools: null,
  task: null,
  chrome: false,
});
