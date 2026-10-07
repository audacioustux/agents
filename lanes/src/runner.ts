// Starting, queueing, resuming and stopping lanes, and the supervisor. Every process starts through `Seams.launch`.

import { resolve } from "node:path";
import { stripVTControlCharacters } from "node:util";
import { type Cluster, maintain, RealCluster } from "./db.ts";
import { Refusal } from "./refusal.ts";

export { Refusal }; // callers and tests import it from here
import {
  alive,
  laneAlive,
  limitLifts,
  loadRegistry,
  mtime,
  type Paths,
  procStat,
  readText,
  withLock,
  withRegistry,
} from "./registry.ts";
import {
  banned,
  baseEnv,
  type ClaudeUsage,
  config,
  floorWhy,
  harnessCommand,
  hhmm,
  isoNow,
  kindOf,
  type Lane,
  parseClaude,
  parseClaudeUsage,
  parseOmp,
  type QueueItem,
  quotaLeft,
  quotaRefusal,
  type Registry,
  TOOLSETS,
  waitReason,
} from "./rules.ts";

type Launch = (
  argv: string[],
  o: { cwd: string; env: Record<string, string>; out: string; append?: boolean },
) => number;
export interface Seams {
  launch: Launch; // detached, stdout and stderr to `out`; returns the pid
  machineBusy(): string | null;
  ompUsage(): Promise<string>; // "" when unreadable
  claudeUsage(harness: string): Promise<string>; // `claude -p /usage` on that account; "" when unreadable
  cluster: Cluster; // the lanes' shared PostgreSQL and Redis (the optional `db` module)
}
export interface Ctx {
  paths: Paths;
  seams: Seams;
  say(line: string): void;
}

export type StartArgs = Omit<QueueItem, "after" | "light" | "waiting">;

const exists = (path?: string) => mtime(path) !== null;
const laneEnv = (): Record<string, string> => ({ ...baseEnv(), ...config.laneEnv });
const stamp = (ctx: Ctx, line: string) => ctx.say(`[${isoNow()}] ${line}`);
export const describe = (e: unknown) => e instanceof Error ? `${e.name}: ${e.message}` : String(e);
const notBanned = (model: string) => {
  const why = banned(model);
  if (why) throw new Refusal(`REFUSED: ${model} may not be used: ${why}`);
};

/** Read before the lock: `omp usage` and `claude /usage` are network calls of up to 90 s. */
export async function quotaPrecheck(a: { harness: string; model: string; force: boolean }, ctx: Ctx) {
  if (a.force) return null;
  if (kindOf(a.harness) === "omp") return quotaRefusal(a.model, await ctx.seams.ompUsage());
  return kindOf(a.harness) === "claude" ? floorWhy(a.harness, (await claudeUsages(ctx)).get(a.harness)) : null;
}

/** The same read for a manual resume, which stays on the lane's own account: its floor reason, or null. */
export async function resumePrecheck(a: { name: string; force: boolean }, ctx: Ctx) {
  const l = loadRegistry(ctx.paths).lanes[a.name];
  const h = l?.harness;
  if (a.force || !h || kindOf(h) !== "claude") return null;
  return floorWhy(h, (await claudeUsages(ctx)).get(h));
}

type Usages = Map<string, ClaudeUsage | null>;
type UsageCache = Record<string, { at: number; usage: ClaudeUsage | null }>;
const metered = () =>
  Object.entries(config.harnesses).filter(([, h]) => h.kind === "claude" && h.cap > 0).map(([n]) => n);

/** What the supervisor last read, for the board; empty when there is none. */
export function cachedUsage(p: Paths): UsageCache {
  try {
    return JSON.parse(readText(p.claudeUsage) ?? "{}");
  } catch {
    return {};
  }
}

/** Each account that can run lanes: from the cache file while it is under `cacheMinutes` old (a failed read too), else
 * read once, all together. `fresh` reads every account. */
export async function claudeUsages(ctx: Ctx, fresh = false): Promise<Usages> {
  const cache = cachedUsage(ctx.paths);
  const now = Date.now();
  const stale = metered().filter((h) => fresh || !(cache[h]?.at > now - config.claudeQuota.cacheMinutes * 60_000));
  await Promise.all(stale.map(async (h) => {
    cache[h] = { at: now, usage: parseClaudeUsage(await ctx.seams.claudeUsage(h)) };
  }));
  if (stale.length) Deno.writeTextFileSync(ctx.paths.claudeUsage, JSON.stringify(cache));
  return new Map(metered().map((h) => [h, cache[h].usage]));
}

function launch(l: Lane, resume: boolean, ctx: Ctx) {
  const attempt = l.attempts + 1;
  const out = `${ctx.paths.root}/${l.name}.${attempt}.json`;
  // a fresh claude attempt gets its session id here: a killed run writes none, and without one it cannot be resumed
  if (!resume && kindOf(l.harness) === "claude") l.session = crypto.randomUUID();
  const prompt = resume ? l.resume_message || config.resumePrompt : Deno.readTextFileSync(l.prompt!);
  const cmd = harnessCommand(l, prompt, resume, ctx.paths.root);
  const env = laneEnv();
  for (const [k, v] of Object.entries(cmd.env)) v === undefined ? delete env[k] : env[k] = v;
  const pid = ctx.seams.launch(cmd.argv, { cwd: l.cwd!, env, out });
  Object.assign(l, { pid, pid_started: procStat(pid)?.started ?? null, out, attempts: attempt, started: isoNow() });
  l.status = "RUNNING";
  delete l.stopped;
  delete l.announced;
  if (resume) l.resumes += 1;
}

const mustExist = (a: StartArgs) =>
  Object.entries({ cwd: a.cwd, prompt: a.prompt }).forEach(([what, path]) => {
    if (!exists(path)) throw new Refusal(`REFUSED: ${what} ${path} does not exist`);
  });

export function start(a: StartArgs, r: Registry, ctx: Ctx, quotaWhy: string | null) {
  mustExist(a);
  const old = r.lanes[a.name];
  if (old?.status === "RUNNING") throw new Refusal(`${a.name} already running`);
  if (old && !a.force) {
    throw new Refusal(
      `REFUSED: lane ${a.name} already exists (its output would be overwritten); pick a new name or --force`,
    );
  }
  notBanned(a.model);
  const kind = kindOf(a.harness);
  const tools = a.tools || (a.readonly ? "ro" : kind === "claude" ? "full" : "ro");
  if (a.readonly && tools !== "ro" && tools !== "none") {
    throw new Refusal(`REFUSED: --readonly contradicts --tools ${tools}`);
  }
  const toolsets = Object.keys(TOOLSETS[kind] ?? {}).sort();
  if (!toolsets.includes(tools)) throw new Refusal(`${a.harness} has no toolset '${tools}': ${toolsets.join(", ")}`);
  if (quotaWhy) throw new Refusal(`REFUSED (quota floor): ${quotaWhy}`, true);
  const lanes = Object.values(r.lanes);
  const globs = (a.globs ?? "").split(",").filter(Boolean);
  const limited = lanes.filter((o) => o.harness === a.harness && o.status === "RATE_LIMIT" && exists(o.out));
  const wait = waitReason({ ...a, globs }, lanes, limited.map(limitLifts), Date.now() / 1000);
  if (wait) throw new Refusal(wait, true);
  const l: Lane = {
    name: a.name,
    harness: a.harness,
    tools,
    task: a.task ?? "",
    cwd: resolve(a.cwd),
    prompt: resolve(a.prompt),
    model: a.model,
    effort: a.effort,
    budget: a.budget,
    globs,
    readonly: tools === "ro" || tools === "none",
    created: old?.created ?? isoNow(),
    attempts: old?.attempts ?? 0,
    resumes: old?.resumes ?? 0,
    cost: 0,
    marker: new RegExp(config.marker).exec(Deno.readTextFileSync(a.prompt))?.[0] ?? null,
    chrome: a.chrome || (old?.chrome ?? false),
    status: "RUNNING",
  };
  launch(l, false, ctx);
  r.lanes[a.name] = l;
  ctx.say(`started ${a.name} [${a.harness}/${a.model}, tools=${tools}] pid ${l.pid} -> ${l.out}`);
}

/** Stored absolute: the supervisor that starts it runs elsewhere. */
export function queue({ after, light, ...a }: StartArgs & { after?: string; light?: boolean }, r: Registry, ctx: Ctx) {
  mustExist(a as StartArgs);
  if (a.name in r.lanes || r.queue.some((q) => q.name === a.name)) {
    throw new Refusal(`REFUSED: ${a.name} already exists or is queued`);
  }
  notBanned(a.model);
  r.queue.push({
    ...a,
    cwd: resolve(a.cwd),
    prompt: resolve(a.prompt),
    ...(after && { after }),
    ...(light && { light }),
  });
  ctx.say(
    `queued ${a.name} [${a.harness}/${a.model}]: starts when ${a.harness} has a free slot` +
      (after ? ` and ${after} is DONE` : ""),
  );
}

export function resume(
  a: { name: string; budget: number; message?: string },
  r: Registry,
  ctx: Ctx,
  quotaWhy: string | null,
) {
  const l = r.lanes[a.name];
  if (!l) throw new Refusal(`no lane ${a.name}`);
  if (l.status === "RUNNING") throw new Refusal("running");
  const raw = readText(l.out);
  const read = raw === null ? null : kindOf(l.harness) === "omp" ? parseOmp(raw).session : parseClaude(raw)?.session_id;
  l.session = l.session || read || null;
  if (!l.session) throw new Refusal("no session id (job never produced JSON); restart it with `start`");
  if (quotaWhy) throw new Refusal(`REFUSED (quota floor): ${quotaWhy}`, true);
  Object.assign(l, { cost_prev: l.cost, budget: a.budget });
  if (a.message) l.resume_message = a.message;
  launch(l, true, ctx);
  ctx.say(`resumed ${a.name} pid ${l.pid}`);
}

export function release(name: string, r: Registry, ctx: Ctx) {
  const q = r.queue.find((x) => x.name === name);
  if (!q) throw new Refusal(`REFUSED: ${name} is not queued`);
  const was = q.after;
  delete q.after;
  delete q.waiting;
  ctx.say(`released ${name}` + (was ? ` (was waiting for ${was})` : " (it had no dependency)"));
}

/** STOPPED is never resumed automatically. */
export function stop(a: { name: string; reason?: string }, r: Registry, ctx: Ctx) {
  const l = r.lanes[a.name];
  if (!l) throw new Refusal(`no lane named ${a.name}`);
  Object.assign(l, { announced: "STOPPED", stopped: a.reason || "stopped by the operator" });
  if (laneAlive(l)) {
    Deno.kill(-l.pid!, "SIGTERM"); // its whole process group
    ctx.say(`stopped ${a.name} (process group ${l.pid}): ${l.stopped}`);
  } else {
    l.status = "STOPPED";
    ctx.say(`${a.name} was not running; marked STOPPED so it is not resumed: ${l.stopped}`);
  }
}

export function forget(name: string, r: Registry, ctx: Ctx) {
  const l = r.lanes[name];
  if (!l) throw new Refusal(`no lane ${name}`);
  if (l.status === "RUNNING") throw new Refusal(`${name} is running; stop it first`);
  delete r.lanes[name];
  ctx.say(`forgot ${name} (its output files are kept)`);
}

export async function quota(ctx: Ctx) {
  const usage = await ctx.seams.ompUsage();
  for (const line of usage.split("\n")) if (/—|used|capacity/.test(line)) ctx.say(line.slice(0, 150));
  for (const { model, section, floor } of config.quotaFloors) {
    const left = Object.entries(quotaLeft(usage, section));
    const stop = !left.length || left.some(([, v]) => v <= floor) ? "STOP (at or below the floor)" : "ok";
    const now = left.map(([w, v]) => `${w} ${v}%`).join(", ") || "unreadable";
    ctx.say(`floor for ${model.replace(/\$$/, "")}: ${floor}% left; now ${now} -> ${stop}`);
  }
  const { weeklyMax, sessionMax } = config.claudeQuota;
  ctx.say(`claude floors: no lane starts at ${weeklyMax}% of the week or ${sessionMax}% of the 5-hour session used`);
  for (const [h, u] of await claudeUsages(ctx, true)) {
    const now = u
      ? `session ${u.session.used}% (resets ${u.session.resets}) · week ${u.week.used}% (resets ${u.week.resets})`
      : "unreadable";
    ctx.say(`${h}: ${now} -> ${floorWhy(h, u) ? "STOP (at the floor)" : "ok"}`);
  }
}

// ── The supervisor ──

/** One tick's claude starts, counted across resumes and the queue so both pass the same machine gate. */
type Gate = { starts: number };
const machineGate = (gate: Gate, ctx: Ctx) =>
  gate.starts >= config.machine.startsPerTick ? `${gate.starts} starts per tick` : ctx.seams.machineBusy();

function resumePass(r: Registry, maxResumes: number, ctx: Ctx, gate: Gate, usage: Usages) {
  const running = new Map<string, number>();
  const bump = (h: string) => running.set(h, (running.get(h) ?? 0) + 1);
  const lanes = Object.values(r.lanes);
  lanes.filter((l) => l.status === "RUNNING").forEach((l) => bump(l.harness));
  for (const l of lanes) {
    const [st, h] = [l.status, l.harness];
    try {
      const auto = kindOf(h) === "claude";
      if (
        auto && ["EARLY_END", "ERROR", "RATE_LIMIT"].includes(st) && (running.get(h) ?? 0) >= config.harnesses[h].cap
      ) {
        continue; // wait for a slot
      }
      const resumable = auto && l.resumes < maxResumes && Boolean(l.session);
      const limited = resumable && st === "RATE_LIMIT" && exists(l.out);
      const due = (resumable && (st === "EARLY_END" || st === "ERROR")) ||
        (limited && Date.now() / 1000 > limitLifts(l));
      const busy = due && kindOf(h) === "claude" && (floorWhy(h, usage.get(h)) ?? machineGate(gate, ctx)); // a resume stays on its account
      if (busy) {
        if (l.announced !== busy) stamp(ctx, `${l.name}: ${st}, resume waits: ${busy}`);
        l.announced = busy;
      } else if (resumable && (st === "EARLY_END" || st === "ERROR")) {
        stamp(ctx, `${l.name}: ${st} -> resume #${l.resumes + 1}`);
        Object.assign(l, { cost_prev: l.cost, budget: config.defaults.budget });
        launch(l, true, ctx);
      } else if (limited && Date.now() / 1000 > limitLifts(l)) {
        stamp(ctx, `${l.name}: rate-limited earlier, retrying`);
        l.cost_prev = l.cost;
        launch(l, true, ctx);
      } else if (["EARLY_END", "ERROR", "RATE_LIMIT", "NO_OUTPUT", "BUDGET"].includes(st) && l.announced !== st) {
        l.announced = st;
        const then = limited
          ? `waits for the account limit to lift at ${hhmm(limitLifts(l))}, then resumes`
          : `${st} needs a human (resumes exhausted or no session)`;
        stamp(ctx, `${l.name}: ${then}`);
      }
      if (l.status === "RUNNING" && st !== "RUNNING") {
        bump(h);
        if (kindOf(h) === "claude") gate.starts += 1;
      }
    } catch (e) {
      if (l.announced === "SUPERVISOR_ERROR") continue;
      l.announced = "SUPERVISOR_ERROR";
      stamp(ctx, `${l.name}: supervisor could not handle this lane: ${describe(e)}`);
    }
  }
}

/** A queued claude lane whose account is at a floor moves to the account with the most weekly headroom that has a free
 * slot and is under both floors; with none it keeps its place, and this is why it waits (null: it may start). */
function underFloor(q: QueueItem, r: Registry, usage: Usages, ctx: Ctx): string | null {
  const why = floorWhy(q.harness, usage.get(q.harness));
  if (!why) return null;
  const running = (h: string) => Object.values(r.lanes).filter((l) => l.status === "RUNNING" && l.harness === h).length;
  const [to] = [...usage].flatMap(([h, u]) =>
    u && h !== q.harness && !floorWhy(h, u) && running(h) < config.harnesses[h].cap ? [{ h, used: u.week.used }] : []
  ).sort((a, b) => a.used - b.used);
  if (!to) return why;
  stamp(ctx, `${q.name}: moved ${q.harness} -> ${to.h} (week ${to.used}% used): ${why}`);
  q.harness = to.h;
  return null;
}

/** Start each queued lane no longer refused. A claude lane not queued with --light also waits for the machine. */
export function drain(
  r: Registry,
  quota: Map<string, string | null>,
  ctx: Ctx,
  gate: Gate = { starts: 0 },
  usage: Usages = new Map(),
) {
  const hold = (q: QueueItem, why: string) => {
    if (q.waiting !== why) stamp(ctx, `${q.name}: still queued: ${why}`);
    q.waiting = why;
  };
  for (const q of [...r.queue]) {
    const gated = kindOf(q.harness) === "claude" && !q.light;
    const busy = gated && machineGate(gate, ctx);
    const dep = q.after && r.lanes[q.after]?.status;
    if (busy) hold(q, busy);
    else if (q.after && dep !== "DONE") hold(q, `waits for ${q.after} (${dep || "not started"})`);
    else {
      // an omp lane queued after this pass read the quota waits one tick
      const unread = kindOf(q.harness) === "omp" && !q.force && !quota.has(q.name);
      const floor = kindOf(q.harness) === "claude" && !q.force ? underFloor(q, r, usage, ctx) : null;
      try {
        start(q, r, ctx, unread ? "quota not read yet (queued during this tick)" : floor ?? quota.get(q.name) ?? null);
        r.queue.splice(r.queue.indexOf(q), 1);
        stamp(ctx, `${q.name}: dequeued and started`);
        if (gated) gate.starts += 1;
      } catch (e) {
        if (!(e instanceof Refusal)) throw e;
        if (e.retryable) hold(q, e.message);
        else {
          r.queue.splice(r.queue.indexOf(q), 1);
          stamp(ctx, `${q.name}: dropped from the queue: ${e.message}`);
        }
      }
    }
  }
}

/** What a pass reads before the lock: the omp quota of each queued omp lane, and every claude account's usage. */
async function readQuotas(ctx: Ctx) {
  const quota = new Map<string, string | null>();
  for (const q of loadRegistry(ctx.paths).queue) {
    if (kindOf(q.harness) === "omp") quota.set(q.name, await quotaPrecheck(q, ctx));
  }
  return { quota, usage: await claudeUsages(ctx) };
}

const lockedPass = (maxResumes: number, reads: Awaited<ReturnType<typeof readQuotas>>, ctx: Ctx, held = () => {}) =>
  withRegistry(ctx.paths, (r) => {
    held();
    const gate = { starts: 0 };
    resumePass(r, maxResumes, ctx, gate, reads.usage);
    try {
      drain(r, reads.quota, ctx, gate, reads.usage);
    } catch (e) {
      stamp(ctx, `queue: ${describe(e)}`);
    }
  });

export const tick = async (maxResumes: number, ctx: Ctx) => lockedPass(maxResumes, await readQuotas(ctx), ctx);

/** The pidfile's pid while it is alive and still a `supervise`: a recycled pid is never signalled. */
export function supervisorPid(p: Paths): number | null {
  const pid = Number(readText(p.pidfile));
  return pid > 0 && alive(pid) && readText(`/proc/${pid}/cmdline`)?.split("\0").includes("supervise") ? pid : null;
}

export async function supervise(interval: number, maxResumes: number, ctx: Ctx) {
  const other = supervisorPid(ctx.paths);
  if (other && other !== Deno.pid) {
    throw new Refusal(`REFUSED: a supervisor already runs (pid ${other}); \`supervisor restart\` replaces it`);
  }
  Deno.writeTextFileSync(ctx.paths.pidfile, String(Deno.pid));
  stamp(ctx, `supervising every ${interval}s (max resumes ${maxResumes}); Ctrl+C to stop`);
  let [holding, stopping] = [false, false]; // a signal while the lock is held waits for the save
  const leave = () => holding ? (stopping = true) : Deno.exit(0);
  Deno.addSignalListener("SIGTERM", leave);
  Deno.addSignalListener("SIGINT", leave);
  const failed = (e: unknown) => stamp(ctx, `supervisor pass failed and will be retried: ${describe(e)}`);
  while (true) {
    const reads = await readQuotas(ctx).catch(failed);
    if (reads) await lockedPass(maxResumes, reads, ctx, () => holding = true).catch(failed);
    holding = false;
    await maintain(ctx.seams.cluster, loadRegistry(ctx.paths)).then(
      (dropped) => dropped.forEach((d) => stamp(ctx, `lane database dropped: ${d}`)),
      (e) => stamp(ctx, `lane database upkeep failed and will be retried: ${describe(e)}`),
    );
    if (stopping) Deno.exit(0);
    await new Promise((done) => setTimeout(done, interval * 1000));
  }
}

/** Signals the old supervisor under the lock, so it is never between a launch and its save. */
export async function supervisor(action: "status" | "stop" | "restart", self: string[], ctx: Ctx) {
  const pid = supervisorPid(ctx.paths);
  if (action === "status") return ctx.say(`supervisor: ${pid ? `running, pid ${pid}` : "NOT running"}`);
  // lanes inherit the supervisor's environment, so the new one takes the old one's
  const old = pid && readText(`/proc/${pid}/environ`)?.split("\0").filter(Boolean).map((kv) => kv.split(/=(.*)/s));
  const env = { ...(old ? Object.fromEntries(old) : Deno.env.toObject()), LANES_ROOT: ctx.paths.root };
  if (pid) {
    await withLock(ctx.paths, () => Deno.kill(pid, "SIGTERM"));
    for (let i = 0; i < 150 && alive(pid); i++) await new Promise((done) => setTimeout(done, 200));
    if (alive(pid)) throw new Refusal(`supervisor pid ${pid} is still running 30 s after SIGTERM`);
    ctx.say(`stopped supervisor pid ${pid}`);
  }
  if (action === "restart") {
    const started = ctx.seams.launch(self, { cwd: ctx.paths.root, env, out: ctx.paths.log, append: true });
    ctx.say(`started supervisor pid ${started}`);
  }
}

// ── The real seams ──

/** Deno.Command cannot give a child a file for stdout, so a fixed sh line opens it and execs in place (same pid). */
const launchDetached: Launch = (argv, o) => {
  const child = new Deno.Command("sh", {
    args: ["-c", `exec "$@" >${o.append ? ">" : ""}"$0" 2>&1`, o.out, ...argv],
    cwd: o.cwd,
    env: o.env,
    clearEnv: true,
    detached: true, // its own session and process group, so `stop` can signal the whole tree
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn();
  child.unref(); // `status` still reaps it while this process lives
  return child.pid;
};

function machineBusy(): string | null {
  const { maxLoad, minFreeGb } = config.machine;
  try {
    const load = Deno.loadavg()[0];
    const gb = Number(/^MemAvailable:\s+(\d+)/m.exec(Deno.readTextFileSync("/proc/meminfo"))![1]) / 1048576;
    if (load > maxLoad) return `the machine is busy (load ${load.toFixed(0)}, starts under ${maxLoad})`;
    if (gb < minFreeGb) return `memory is short (${gb.toFixed(1)} GB available, starts above ${minFreeGb})`;
  } catch { /* unreadable: no gate */ }
  return null;
}

const usage = { at: 0, text: "" };
/** At most one call a minute, failed or not; a failure reads as unreadable quota, which refuses the start. */
async function ompUsage(): Promise<string> {
  if (Date.now() - usage.at < 60_000) return usage.text;
  const out = await new Deno.Command("omp", {
    args: ["usage"],
    env: laneEnv(),
    clearEnv: true,
    stdin: "null",
    stderr: "null",
    signal: AbortSignal.timeout(90_000),
  }).output().catch(() => null);
  const text = out ? stripVTControlCharacters(new TextDecoder().decode(out.stdout)) : "";
  Object.assign(usage, { at: Date.now(), text: text.replace(/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+/g, "<account>") });
  return usage.text;
}

async function claudeUsage(harness: string): Promise<string> {
  const env = { ...laneEnv(), CLAUDE_CONFIG_DIR: config.harnesses[harness].home! };
  const out = await new Deno.Command("claude", {
    args: ["-p", "/usage"], // answered locally: no model call, no cost
    env,
    clearEnv: true,
    stdin: "null",
    stderr: "null",
    signal: AbortSignal.timeout(90_000),
  }).output().catch(() => null);
  return out ? stripVTControlCharacters(new TextDecoder().decode(out.stdout)) : "";
}

export const realSeams: Seams = {
  launch: launchDetached,
  machineBusy,
  ompUsage,
  claudeUsage,
  cluster: new RealCluster(),
};
