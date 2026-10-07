// Test helpers: a scratch lanes root, a fake launcher and a fake database cluster.

import type { Cluster, Entry, LaneNote } from "./db.ts";
import { pathsAt } from "./registry.ts";
import type { Lane } from "./rules.ts";
import type { Ctx, Seams } from "./runner.ts";

export interface Launched {
  argv: string[];
  cwd: string;
  env: Record<string, string>;
  out: string;
}

/** The shared servers in memory: databases with their notes, the Redis numbers flushed, and every call made. A migration
 * leaves a `tplb_` database behind (as a real migration does), or fails when `failMigrate` is set; `migrating` can hold it. */
export function fakeCluster() {
  const databases = new Map<string, Entry["note"]>();
  const state = {
    up: false,
    pidfile: false,
    failMigrate: false,
    migrating: Promise.resolve(),
    calls: [] as string[],
    flushed: [] as number[],
  };
  const cluster: Cluster = {
    ensureUp: () => {
      state.calls.push("ensureUp");
      state.up = state.pidfile = true;
      return Promise.resolve();
    },
    shouldBeUp: () => state.pidfile,
    list: () => Promise.resolve([...databases].map(([name, note]) => ({ name, note }))),
    create(name, note: LaneNote, template) {
      if (!databases.has(template)) return Promise.reject(new Error(`no template ${template}`));
      state.calls.push(`create ${name} from ${template}`);
      databases.set(name, note);
      return Promise.resolve();
    },
    async migrate(worktree, name) {
      state.calls.push(`migrate ${name} in ${worktree}`);
      await state.migrating;
      databases.set(name, null);
      if (state.failMigrate) throw new Error("migration failed");
    },
    rename: (from, to) => {
      databases.set(to, databases.get(from) ?? null);
      databases.delete(from);
      return Promise.resolve();
    },
    annotate: (name, note) => {
      databases.set(name, note);
      return Promise.resolve();
    },
    drop: (name) => {
      state.calls.push(`drop ${name}`);
      databases.delete(name);
      return Promise.resolve();
    },
    flushRedis: (db) => {
      state.flushed.push(db);
      return Promise.resolve();
    },
  };
  return { cluster, databases, state };
}

/** A launcher that starts no harness. Each launch writes `output` (the job's whole stdout) to the lane's file and
 * returns a pid: one that is gone by default, or a live `sleep` while `running` is set. */
export function fakeSeams(
  o: { busy?: string | null; usage?: string; claude?: Record<string, string> } = {},
) {
  const launched: Launched[] = [];
  const sleepers: Deno.ChildProcess[] = [];
  const state = { output: "", running: false };
  const { cluster } = fakeCluster();
  const claudeReads: string[] = []; // each `claude /usage` read, by harness
  const seams: Seams = {
    launch(argv, l) {
      launched.push({ argv, cwd: l.cwd, env: l.env, out: l.out });
      Deno.writeTextFileSync(l.out, state.output);
      if (!state.running) return 4194304 + launched.length; // above any pid Linux hands out: a job already gone
      const child = new Deno.Command("sleep", { args: ["60"] }).spawn();
      sleepers.push(child);
      return child.pid;
    },
    machineBusy: () => o.busy ?? null,
    ompUsage: () => Promise.resolve(o.usage ?? ""),
    claudeUsage: (h) => {
      claudeReads.push(h);
      return Promise.resolve(o.claude?.[h] ?? "");
    },
    cluster,
  };
  const cleanup = async () => {
    for (const s of sleepers) {
      try {
        s.kill();
      } catch { /* already gone */ }
      await s.status;
    }
  };
  return { seams, launched, state, claudeReads, cleanup };
}

/** A scratch lanes root with a prompt file and a cwd, and a context that records what it says. */
export function scratch(seams: Seams) {
  const root = Deno.makeTempDirSync({ prefix: "lanes-test-" });
  const prompt = `${root}/prompt.txt`;
  Deno.writeTextFileSync(prompt, "Fix the failing parser test, then report.\nSecond line of the brief.");
  const said: string[] = [];
  const ctx: Ctx = { paths: pathsAt(root), seams, say: (line) => said.push(line) };
  return { root, prompt, said, ctx };
}

/** A claude result line as `--output-format json` writes it. */
export const claudeResult = (result: string, extra: Record<string, unknown> = {}) =>
  JSON.stringify({ type: "result", subtype: "success", is_error: false, session_id: "s-1", result, ...extra }) + "\n";

export const longReport = "Done. " + "Every step of the brief is complete and verified. ".repeat(6);

export function lane(over: Partial<Lane> & { name: string }): Lane {
  return {
    harness: "claude2",
    model: "sonnet",
    readonly: false,
    created: "2030-01-01T00:00:00Z",
    attempts: 1,
    resumes: 0,
    cost: 0,
    status: "DONE",
    ...over,
  };
}

/** What `claude -p /usage` prints for an account (the 5-hour session, then the all-models week), as the CLI words it. */
export const usageText = (session: number, week: number) =>
  `You are currently using your subscription to power your Claude Code usage
Current session: ${session}% used · resets Jan 2, 11:29am (UTC)
Current week (all models): ${week}% used · resets Jan 3, 9:59am (UTC)
Current week (one model): 0% used · resets Jan 3, 10am (UTC)
What's contributing to your limits usage?
`;
