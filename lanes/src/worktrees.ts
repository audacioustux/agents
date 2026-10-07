// The optional worktree pool (the `worktrees` config key): a worktree that is installed and no longer needed is moved
// into a pool instead of being deleted, and `take` hands it out again on a new branch. Ignored files such as installed
// dependencies survive, so a new lane starts with a relink instead of a full install.

import { basename, join, resolve } from "node:path";
import { Refusal } from "./refusal.ts";
import { lockFile, mtime, readText } from "./registry.ts";
import { type Config, config, LIVE, type Registry } from "./rules.ts";

export type PoolSettings = NonNullable<Config["worktrees"]>;

/** The `worktrees` settings, or a refusal when this installation does not use the module. */
export const poolSettings = (): PoolSettings => {
  if (!config.worktrees) {
    throw new Refusal('REFUSED: the worktree pool is off: add a "worktrees" key to the config file');
  }
  return config.worktrees;
};

const POOL = "pool-";
const dirOf = (s: PoolSettings) => resolve(s.repo, s.dir);
const baseRef = (s: PoolSettings) => `${s.remote}/${s.branch}`;
const exists = (path: string) => mtime(path) !== null;

async function run(argv: string[], cwd: string): Promise<string> {
  const [cmd, ...args] = argv;
  const out = await new Deno.Command(cmd, { args, cwd, stdin: "null", stdout: "piped", stderr: "piped" }).output();
  const text = (b: Uint8Array) => new TextDecoder().decode(b);
  if (!out.success) throw new Error(`${argv.join(" ")}: ${(text(out.stderr) || text(out.stdout)).trim().slice(-2000)}`);
  return text(out.stdout);
}
const git = (cwd: string, ...args: string[]) => run(["git", ...args], cwd);
const succeeds = (cwd: string, ...args: string[]) => git(cwd, ...args).then(() => true, () => false);

/** The pooled worktrees, oldest name first. */
export function members(s: PoolSettings): string[] {
  const dir = dirOf(s);
  if (!exists(dir)) return [];
  return [...Deno.readDirSync(dir)].filter((e) => e.isDirectory && e.name.startsWith(POOL)).map((e) =>
    join(dir, e.name)
  )
    .sort();
}

const install = (s: PoolSettings, path: string) => s.installCommand ? run(s.installCommand, path) : Promise.resolve("");

/** A worktree at `path`: a pooled one on a new `branch` (detached when there is none) at `from`, or a fresh one when the
 * pool is empty. Returns what it did. */
export async function take(
  s: PoolSettings,
  path: string,
  o: { branch?: string; from?: string },
): Promise<string> {
  path = resolve(path);
  const from = o.from ?? baseRef(s);
  const checkout = o.branch ? ["-b", o.branch] : ["--detach"];
  if (exists(path)) throw new Refusal(`REFUSED: ${path} exists`);
  Deno.mkdirSync(dirOf(s), { recursive: true });
  const member = await lockFile(`${dirOf(s)}/.pool.lock`, async () => {
    const [first] = members(s);
    if (first) await git(s.repo, "worktree", "move", first, path);
    return first;
  });
  if (!member) {
    await git(s.repo, "worktree", "add", ...(o.branch ? ["-b", o.branch, path] : ["--detach", path]), from);
    await install(s, path);
    return `took a fresh worktree: ${path}`;
  }
  try {
    await git(path, "clean", "-fdq"); // untracked files only: ignored ones stay
    await git(path, "checkout", "-q", ...checkout, from);
  } catch (e) {
    await git(s.repo, "worktree", "move", path, member); // the pool keeps what it could not hand out
    throw e;
  }
  await install(s, path);
  return `took pooled worktree -> ${path}`;
}

/** The names under the pool directory that a live or queued lane's brief mentions: those worktrees are in use. */
function namedByBriefs(s: PoolSettings, r: Registry): Set<string> {
  const prompts = [
    ...Object.values(r.lanes).filter((l) => LIVE.includes(l.status)).map((l) => l.prompt),
    ...r.queue.map((q) => q.prompt),
  ];
  const escaped = dirOf(s).replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  const named = new RegExp(`${escaped}/([^\\s/\`"'),;]+)`, "g");
  return new Set(prompts.flatMap((p) => [...(readText(p) ?? "").matchAll(named)].map((m) => m[1])));
}

/** Worktrees under the pool directory that can be recycled: clean, installed (when `installedMarker` is set), already
 * contained in the base branch, not named by a brief, and not matching `keep`. */
export async function recyclable(s: PoolSettings, r: Registry): Promise<string[]> {
  await git(s.repo, "fetch", "-q", s.remote, s.branch);
  const [named, keep, dir] = [namedByBriefs(s, r), s.keep.map((k) => new RegExp(k)), dirOf(s)];
  const listed = (await git(s.repo, "worktree", "list", "--porcelain")).split("\n")
    .filter((l) => l.startsWith("worktree ")).map((l) => l.slice("worktree ".length));
  const out: string[] = [];
  for (const path of listed.filter((p) => resolve(p, "..") === dir)) {
    const name = basename(path);
    if (name.startsWith(POOL) || named.has(name) || keep.some((k) => k.test(name))) continue;
    if (s.installedMarker && !exists(join(path, s.installedMarker))) continue;
    if ((await git(path, "status", "--porcelain")).trim()) continue;
    if (
      await succeeds(s.repo, "merge-base", "--is-ancestor", (await git(path, "rev-parse", "HEAD")).trim(), baseRef(s))
    ) {
      out.push(path);
    }
  }
  return out;
}

/** Moves each recyclable worktree into the pool, detached (its branch stays). Returns the names it pooled. */
export async function fill(s: PoolSettings, r: Registry): Promise<string[]> {
  const pooled: string[] = [];
  let n = members(s).length;
  for (const path of await recyclable(s, r)) {
    await git(path, "checkout", "-q", "--detach");
    await git(s.repo, "worktree", "move", path, join(dirOf(s), `${POOL}${Date.now()}-${++n}`));
    pooled.push(basename(path));
  }
  return pooled;
}
