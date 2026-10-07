// registry.json: parsed strictly, locked with flock on .registry.lock, saved atomically, refreshed.

import { dirname } from "node:path";
import {
  classifyOutput,
  config,
  fit,
  FROM_OUTPUT,
  kindOf,
  type Lane,
  oneLine,
  parseClaude,
  parseOmp,
  REGISTRY,
  type Registry,
  resetEpoch,
} from "./rules.ts";

export const pathsAt = (root: string) => ({
  root,
  registry: `${root}/registry.json`,
  lock: `${root}/.registry.lock`,
  pidfile: `${root}/supervisor.pid`,
  log: `${root}/supervisor.log`,
  claudeUsage: `${root}/claude-usage.json`,
});
export type Paths = ReturnType<typeof pathsAt>;
export const defaultPaths = () => pathsAt(Deno.env.get("LANES_ROOT") ?? config.root);

export function parseRegistry(text: string): Registry {
  const raw = JSON.parse(text);
  const r = fit(REGISTRY, { queue: [], ...raw }, "registry", config.harnesses);
  for (const [n, l] of Object.entries(r.lanes)) {
    if (l.name !== n) throw new Error(`registry.lanes.${n} is named ${l.name}`);
  }
  return r;
}

export function loadRegistry(p: Paths): Registry {
  try {
    return parseRegistry(Deno.readTextFileSync(p.registry));
  } catch (e) {
    if (e instanceof Deno.errors.NotFound) return { lanes: {}, queue: [] }; // only absence reads as empty
    throw e;
  }
}

export function saveRegistry(p: Paths, r: Registry) {
  const tmp = Deno.makeTempFileSync({ dir: p.root, prefix: ".registry." });
  Deno.writeTextFileSync(tmp, JSON.stringify(r, null, 1));
  Deno.renameSync(tmp, p.registry); // atomic: a reader never sees half a registry
}

/** Load, refresh, run `fn`, save, under the lock; saved on error too, so no launched lane goes unrecorded. */
export const withRegistry = <T>(p: Paths, fn: (r: Registry) => T | Promise<T>) =>
  withLock(p, async () => {
    const r = loadRegistry(p);
    refresh(r);
    try {
      return await fn(r);
    } finally {
      saveRegistry(p, r);
    }
  });

/** `fn` while holding an exclusive `flock` on `file` (created if absent), shared with any other process that locks it. */
export async function lockFile<T>(file: string, fn: () => T | Promise<T>): Promise<T> {
  Deno.mkdirSync(dirname(file), { recursive: true });
  using lock = await Deno.open(file, { write: true, create: true });
  await lock.lock(true);
  return await fn();
}

export const withLock = <T>(p: Paths, fn: () => T | Promise<T>): Promise<T> => lockFile(p.lock, fn);

/** State letter and start time from /proc, or null when the process is gone. */
export function procStat(pid: number) {
  const f = readText(`/proc/${pid}/stat`)?.split(") ").at(-1)?.split(" ");
  return f ? { state: f[0], started: f[19] } : null;
}

/** A zombie has finished; the same pid with another start time was recycled. */
export function alive(pid: number, started?: string) {
  const st = procStat(pid);
  return st !== null && st.state !== "Z" && (started === undefined || st.started === started);
}

/** No recorded start time means a pid that may have been recycled: treated as finished, so `stop` hits no stranger. */
export const laneAlive = (l: Lane) => Boolean(l.pid && l.pid_started && alive(l.pid, l.pid_started));

export function mtime(path?: string | null): number | null {
  try {
    return path ? Deno.statSync(path).mtime!.getTime() / 1000 : null;
  } catch {
    return null;
  }
}

/** Text with \r\n and \r read as \n. */
export function readText(path?: string | null): string | null {
  try {
    return path ? new TextDecoder().decode(Deno.readFileSync(path)).replace(/\r\n?/g, "\n") : null;
  } catch {
    return null;
  }
}

/** When a rate-limited lane may retry: 2 minutes after its stated reset, else 15 minutes after it died. */
export function limitLifts(l: Lane): number {
  const died = mtime(l.out)!;
  const at = resetEpoch((readText(l.out) ?? "").slice(-6000), died);
  return at ? at + 120 : died + 900;
}

/** Each lane's state and, once it ends, what its output says. An ended lane with unchanged output is not re-read. */
export function refresh(r: Registry) {
  for (const l of Object.values(r.lanes)) {
    const h = l.harness;
    const mt = mtime(l.out);
    // equal to the millisecond: that is all `Deno.stat` reads, so the stored time can differ below it
    const unchanged = l.parsed_mtime != null && mt !== null && Math.abs(l.parsed_mtime - mt) < 1e-3;
    const [live, was] = [laneAlive(l), l.status];
    if (!live && unchanged && (l.stopped || FROM_OUTPUT.includes(was))) {
      if (l.stopped) l.status = "STOPPED";
      continue;
    }
    const raw = live ? null : readText(l.out);
    l.status = live ? "RUNNING" : l.stopped ? "STOPPED" : classifyOutput(h, raw);
    if (!live && unchanged && was !== "RUNNING") continue; // a lane last seen running is read once more after it ends
    l.parsed_mtime = mt;
    if (kindOf(h) === "omp") {
      if (mt === null) continue;
      const o = parseOmp(raw ?? readText(l.out) ?? "");
      Object.assign(l, { session: o.session || l.session, cost: Number(o.cost.toFixed(3)), answered_by: o.models });
      l.turns = o.calls;
      if (!live) l.last = oneLine(o.text, 160);
    } else if (!live) {
      const d = raw === null ? null : parseClaude(raw);
      if (!d) continue;
      if ("session_id" in d) l.session = d.session_id;
      l.cost = Number(((l.cost_prev ?? 0) + (d.total_cost_usd || 0)).toFixed(2));
      l.turns = d.num_turns ?? null;
      l.last = oneLine(d.result ?? "", 160);
    }
  }
}
