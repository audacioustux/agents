// Lane rules and record shapes. Everything specific to one machine or project is in the config file ($LANES_CONFIG).

import { dirname, resolve } from "node:path";

const STATES = [
  "RUNNING",
  "DONE",
  "EARLY_END",
  "ERROR",
  "RATE_LIMIT",
  "BUDGET",
  "NO_OUTPUT",
  "STOPPED",
] as const;
export type State = (typeof STATES)[number];
export const LIVE: readonly string[] = ["RUNNING", "RATE_LIMIT"];
export const FROM_OUTPUT: readonly string[] = ["DONE", "EARLY_END", "ERROR", "RATE_LIMIT", "BUDGET"]; // fixed while the output is
const KINDS = ["claude", "omp"] as const;

// A shape names a leaf type; `key?` is optional, {"*": V} is a map and [V] a list. The record types derive from them.
const LEAF = {
  s: (v: unknown): v is string => typeof v === "string",
  "s|null": (v: unknown): v is string | null => v === null || typeof v === "string",
  n: (v: unknown): v is number => typeof v === "number",
  "n|null": (v: unknown): v is number | null => v === null || typeof v === "number",
  b: (v: unknown): v is boolean => typeof v === "boolean",
  "s[]": (v: unknown): v is string[] => Array.isArray(v) && v.every((x) => typeof x === "string"),
  h: (v: unknown, harnesses: object): v is string => typeof v === "string" && v in harnesses, // a configured harness
  state: (v: unknown): v is State => STATES.includes(v as State),
  kind: (v: unknown): v is (typeof KINDS)[number] => KINDS.includes(v as (typeof KINDS)[number]),
};
type Leaf = { [K in keyof typeof LEAF]: (typeof LEAF)[K] extends (v: unknown, h: object) => v is infer T ? T : never };
type Shape = keyof Leaf | readonly [Shape] | { readonly [k: string]: Shape };
type Flat<T> = { [K in keyof T]: T[K] };
type Of<S> = S extends keyof Leaf ? Leaf[S]
  : S extends readonly [infer E] ? Of<E>[]
  : S extends { "*": infer V } ? Record<string, Of<V>>
  : Flat<
    & { -readonly [K in keyof S as K extends `${string}?` ? never : K]: Of<S[K]> }
    & { -readonly [K in keyof S as K extends `${infer N}?` ? N : never]?: Of<S[K]> }
  >;

const LANE = {
  name: "s",
  model: "s",
  readonly: "b",
  created: "s",
  attempts: "n",
  resumes: "n",
  cost: "n",
  status: "state",
  harness: "h",
  "tools?": "s",
  "task?": "s",
  "cwd?": "s",
  "prompt?": "s",
  "effort?": "s",
  "budget?": "n",
  "globs?": "s[]",
  "marker?": "s|null",
  "chrome?": "b",
  "session?": "s|null",
  "pid?": "n",
  "pid_started?": "s|null",
  "out?": "s",
  "started?": "s",
  "turns?": "n|null",
  "last?": "s",
  "cost_prev?": "n",
  "resume_message?": "s",
  "answered_by?": "s[]",
  "announced?": "s",
  "stopped?": "s",
  "parsed_mtime?": "n|null",
} as const;
const QUEUE_ITEM = {
  name: "s",
  cwd: "s",
  prompt: "s",
  model: "s",
  effort: "s",
  budget: "n",
  globs: "s|null", // the raw comma-separated --globs
  readonly: "b",
  force: "b",
  harness: "h",
  tools: "s|null",
  task: "s|null",
  chrome: "b",
  "after?": "s",
  "light?": "b",
  "waiting?": "s",
} as const;
export const REGISTRY = { lanes: { "*": LANE }, queue: [QUEUE_ITEM] } as const satisfies Shape;
const CONFIG = {
  root: "s",
  path: "s[]",
  unsetEnv: "s[]",
  marker: "s",
  resumePrompt: "s",
  harnesses: { "*": { kind: "kind", "home?": "s", cap: "n", "env?": { "*": "s" } } },
  defaults: { harness: "h", model: "s", effort: "s", budget: "n" },
  modelCaps: { "*": "n" },
  banned: { "*": "s" },
  quotaFloors: [{ model: "s", section: "s", floor: "n" }],
  claudeQuota: { weeklyMax: "n", sessionMax: "n", cacheMinutes: "n" },
  machine: { maxLoad: "n", minFreeGb: "n", startsPerTick: "n" },
  supervisor: { interval: "n", maxResumes: "n" },
  launch: { nice: "n", timeoutSeconds: "n", ompMaxTime: "s" },
  laneEnv: { "*": "s" },
  "db?": {
    dir: "s",
    pgPort: "n",
    "redis?": { port: "n", databases: "n" },
    maxConnections: "n",
    sharedBuffers: "s",
    initdbArgs: "s[]",
    "migrateCommand?": "s[]",
    templateInputs: "s[]",
    "env?": { "*": "s" },
    templateKeepMinutes: "n",
    orphanHours: "n",
  },
  "worktrees?": {
    repo: "s",
    dir: "s",
    remote: "s",
    branch: "s",
    "installCommand?": "s[]",
    "installedMarker?": "s",
    keep: "s[]",
  },
} as const satisfies Shape;
export type Lane = Of<typeof LANE>;
export type QueueItem = Of<typeof QUEUE_ITEM>;
export type Registry = Of<typeof REGISTRY>;
export type Config = Of<typeof CONFIG>;

/** `v` typed as `shape`, or an error naming the first key that does not fit. */
export function fit<S extends Shape>(shape: S, v: unknown, where: string, harnesses: object): Of<S> {
  const bad = (why: string) => new Error(`${where} ${why}`);
  if (typeof shape === "string") {
    if (!LEAF[shape as keyof Leaf](v, harnesses as never)) throw bad(`= ${JSON.stringify(v)} is not ${shape}`);
  } else if (Array.isArray(shape)) {
    if (!Array.isArray(v)) throw bad("is not a list");
    v.forEach((x, i) => fit(shape[0], x, `${where}[${i}]`, harnesses));
  } else {
    if (typeof v !== "object" || v === null || Array.isArray(v)) throw bad("is not an object");
    const spec = shape as Record<string, Shape>;
    for (const k in spec) if (k !== "*" && !k.endsWith("?") && !(k in v)) throw bad(`has no "${k}"`);
    for (const [k, x] of Object.entries(v)) {
      const s = spec["*"] ?? spec[k] ?? spec[`${k}?`];
      if (!s) throw bad(`has an unknown key "${k}"`);
      fit(s, x, `${where}.${k}`, harnesses);
    }
  }
  return v as Of<S>;
}

/** The value of every key a config file may leave out. `db` and `worktrees` apply only to a file that names them. */
const DEFAULTS = {
  root: "state",
  path: [],
  unsetEnv: [],
  marker: "^[^\\n]{0,60}",
  resumePrompt: "Continue and finish the task from where you stopped. Your previous turn ended without a final " +
    "report, and in non-interactive mode ending the turn ends the job. Run every long command in the foreground, " +
    "never end your turn while work is pending, and write your final report only when everything is done. First " +
    "check the current state, then complete the remaining steps of the original brief.",
  defaults: { model: "sonnet", effort: "high", budget: 50 },
  modelCaps: {},
  banned: {},
  quotaFloors: [],
  claudeQuota: { weeklyMax: 90, sessionMax: 90, cacheMinutes: 10 },
  machine: { maxLoad: 1_000, minFreeGb: 0, startsPerTick: 2 },
  supervisor: { interval: 120, maxResumes: 3 },
  launch: { nice: 10, timeoutSeconds: 14_400, ompMaxTime: "45m" },
  laneEnv: {},
};
const MODULE_DEFAULTS = {
  db: {
    dir: "db",
    pgPort: 54330,
    maxConnections: 200,
    sharedBuffers: "256MB",
    initdbArgs: ["-E", "UTF8"],
    templateInputs: [],
    templateKeepMinutes: 120,
    orphanHours: 24,
  },
  redis: { port: 63790, databases: 16 },
  worktrees: { repo: ".", dir: ".worktrees", remote: "origin", branch: "main", keep: [] },
};

type Plain = Record<string, unknown>;
const isPlain = (v: unknown): v is Plain => typeof v === "object" && v !== null && !Array.isArray(v);
/** `over` on top of `base`: objects merge key by key, every other value (a list included) replaces. */
function merge(base: Plain, over: Plain): Plain {
  const out: Plain = { ...base };
  for (const [k, v] of Object.entries(over)) out[k] = isPlain(v) && isPlain(base[k]) ? merge(base[k], v) : v;
  return out;
}

function withDefaults(raw: unknown): unknown {
  if (!isPlain(raw)) return raw; // `fit` names what is wrong with it
  const c = merge(DEFAULTS, raw);
  const db = raw.db;
  if (isPlain(db)) {
    const redis = isPlain(db.redis) ? { redis: merge(MODULE_DEFAULTS.redis, db.redis) } : {};
    c.db = { ...merge(MODULE_DEFAULTS.db, db), ...redis };
  }
  if (isPlain(raw.worktrees)) c.worktrees = merge(MODULE_DEFAULTS.worktrees, raw.worktrees);
  const first = isPlain(raw.harnesses) ? Object.keys(raw.harnesses)[0] : undefined;
  c.defaults = { harness: first, ...(c.defaults as Plain) };
  return c;
}

function loadConfig(file: string): Config {
  let text;
  try {
    text = Deno.readTextFileSync(file);
  } catch {
    throw new Error(`no config file at ${file}: copy lanes.config.example.json there, or set LANES_CONFIG`);
  }
  const raw = withDefaults(JSON.parse(text));
  const c = fit(CONFIG, raw, file, (raw as Plain).harnesses ?? {});
  const home = (p: string) => p.replace(/^~(?=\/)/, Deno.env.get("HOME")!);
  const base = dirname(file);
  for (const [name, h] of Object.entries(c.harnesses)) {
    if (h.kind === "claude" && !h.home) throw new Error(`${file} harnesses.${name}: a claude harness needs "home"`);
    if (h.home) h.home = home(h.home);
  }
  if (c.worktrees) c.worktrees.repo = resolve(base, home(c.worktrees.repo));
  return { ...c, root: resolve(base, home(c.root)), path: c.path.map(home) };
}

export const config = loadConfig(
  Deno.env.get("LANES_CONFIG") ?? new URL("../lanes.config.json", import.meta.url).pathname,
);
export const kindOf = (h: string) => config.harnesses[h]?.kind;
export const banned = (model: string) =>
  Object.entries(config.banned).find(([re]) => new RegExp(re).test(model))?.[1] ?? null;

/** The process environment minus `unsetEnv`, with `path` appended to PATH: what every tool and lane starts from. */
export function baseEnv(): Record<string, string> {
  const env = Deno.env.toObject();
  for (const k of config.unsetEnv) delete env[k];
  return { ...env, PATH: [env.PATH, ...config.path].filter(Boolean).join(":") };
}

const CLAUDE_TOOLS = {
  ro: ["Read", "Glob", "Grep", "WebFetch"],
  edit: ["Read", "Edit", "Write", "Glob", "Grep", "WebFetch"],
  full: ["Bash", "Read", "Edit", "Write", "Glob", "Grep", "WebFetch"],
};
/** `--tools` per harness kind: omp never gets bash unattended. */
export const TOOLSETS: Record<string, Record<string, string[]>> = {
  claude: CLAUDE_TOOLS,
  omp: { none: [], ro: ["read", "grep", "glob"], edit: ["read", "grep", "glob", "edit", "write"] },
};

// ── A finished job's output ──

const EARLY_END =
  /\bi(['’]ll| will) (continue|pick (this|it) up|check back|wait)\b|\bi(['’]m| am) (now )?waiting (for|on)\b|\b(is|are) still running\b|\bonce the (background|run|gate|suite|build)\b[^.]{0,60}\b(finish|complete)/i;
const RATE = /rate.?limit|usage limit|spend limit|session limit|hit your [a-z -]*limit|429|overloaded/i;
const RESETS = /resets\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)\s*\(UTC\)/i;
// deno-lint-ignore no-control-regex -- every line separator a harness's output can contain
const LINE_BREAKS = /\r\n|[\n\r\v\f\x1c\x1d\x1e\x85\u2028\u2029]/;
// deno-lint-ignore no-explicit-any
type Json = any;
const json = (s: string): Json => {
  try {
    return JSON.parse(s);
  } catch {
    return null;
  }
};

/** The last two non-empty lines: where a job that stopped early says so (a good report's body may say it too). */
export const closing = (text: string) =>
  text.split(LINE_BREAKS).filter((x) => x.trim()).slice(-2).join("\n").slice(-400);
const endsEarly = (text: string) => EARLY_END.test(closing(text)) || [...text].length < 200; // a report, not a one-liner

/** The epoch second of the first "resets 9:20am (UTC)" after `after`, or null. */
export function resetEpoch(text: string, after: number): number | null {
  const m = RESETS.exec(text);
  if (!m) return null;
  const day = new Date(after * 1000);
  day.setUTCHours((Number(m[1]) % 12) + (m[3].toLowerCase() === "pm" ? 12 : 0), Number(m[2] ?? 0), 0, 0);
  const at = day.getTime() / 1000;
  return at <= after ? at + 86400 : at;
}

/** omp --mode json: one event per line; the result is the last assistant text. */
export function parseOmp(raw: string) {
  const o = {
    text: "",
    cost: 0,
    models: [] as string[],
    ended: false,
    stop: null,
    session: null,
    calls: 0,
    lastAction: "",
  };
  for (const line of raw.split(LINE_BREAKS)) {
    const e = line.startsWith("{") ? json(line) : null;
    if (e?.type === "session") o.session = e.id ?? null;
    else if (e?.type === "agent_end") o.ended = true;
    else if (e?.type === "message_end" && e.message?.role === "assistant") {
      const m = e.message;
      o.stop = m.stopReason ?? null;
      if (m.model && !o.models.includes(m.model)) o.models.push(m.model);
      o.cost += m.usage?.cost?.total || 0;
      for (const c of m.content ?? []) {
        if (c.type === "text" && c.text?.trim()) o.text = c.text.trim();
        else if (["toolCall", "tool_use", "tool_call"].includes(c.type)) {
          const a = c.arguments ?? c.input ?? {};
          o.calls += 1;
          o.lastAction = `${c.name} ${String(a.path || a.pattern || a.file_path || a.command || "").slice(0, 70)}`;
        }
      }
    }
  }
  return o as typeof o & { stop: string | null; session: string | null };
}

/** claude --output-format json: the last whole-line JSON object that is a result. */
export const parseClaude = (raw: string): Json =>
  raw.split("\n").filter((x) => x.startsWith("{") && x.endsWith("}")).reverse().map(json)
    .find((d) => d && typeof d === "object" && !Array.isArray(d) && ("session_id" in d || "subtype" in d)) ?? null;

/** The state of a lane that is neither alive nor stopped, from its output (null: there is none). */
export function classifyOutput(harness: string, raw: string | null): State {
  if (raw === null) return "NO_OUTPUT";
  const limited = (s: string) => RATE.test(s) ? "RATE_LIMIT" : "ERROR";
  if (kindOf(harness) === "omp") {
    const o = parseOmp(raw);
    const ok = o.ended && o.text && o.stop !== "error" && o.stop !== "aborted";
    return ok ? endsEarly(o.text) ? "EARLY_END" : "DONE" : limited(raw.slice(-4000));
  }
  const d = parseClaude(raw);
  if (!d) return limited(raw.slice(-4000));
  const result = d.result ?? "";
  if (d.subtype === "error_max_budget_usd") return "BUDGET";
  if (RATE.test(result.slice(0, 400)) && RESETS.test(result)) return "RATE_LIMIT";
  if (d.is_error || (d.subtype != null && d.subtype !== "success")) return limited(JSON.stringify(d).slice(0, 2000));
  return endsEarly(result) ? "EARLY_END" : "DONE";
}

// ── File scopes ──

/** Shell-style `*`, `?` and `[...]` matching of one path segment, case-sensitive. */
function fnmatch(name: string, pattern: string): boolean {
  let re = "";
  for (let i = 0; i < pattern.length; i++) {
    let j = i + 1 + (pattern[i + 1] === "!" ? 1 : 0);
    j = pattern[i] === "[" ? pattern.indexOf("]", j + (pattern[j] === "]" ? 1 : 0)) : -1;
    if (pattern[i] === "*") re += ".*";
    else if (pattern[i] === "?") re += ".";
    else if (j > 0) {
      const body = pattern.slice(i + 1, j).replace(/\\/g, "\\\\");
      re += `[${body[0] === "!" ? "^" + body.slice(1) : body}]`;
      i = j;
    } else re += pattern[i].replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  }
  return new RegExp(`^${re}$`, "s").test(name);
}

/** Leading segments up to the first `**`, and whether there was one (it matches anything below). */
function segments(glob: string): [string[], boolean] {
  const all = glob.replace(/^\/+|\/+$/g, "").split("/");
  const at = all.indexOf("**");
  return at < 0 ? [all, false] : [all.slice(0, at), true];
}

const segMatch = (a: string, b: string) => {
  const [pa, pb] = [a.split(/[*?[]/)[0], b.split(/[*?[]/)[0]];
  return fnmatch(a, b) || fnmatch(b, a) || (pa !== a && pb !== b && (pa.startsWith(pb) || pb.startsWith(pa)));
};

/** The first pair of globs that can name the same file, or null. */
export function overlap(a: string[], b: string[]): [string, string] | null {
  for (const x of a) {
    for (const y of b) {
      const [[xs, xOpen], [ys, yOpen]] = [segments(x), segments(y)];
      if (!xs.slice(0, ys.length).every((s, i) => segMatch(s, ys[i]))) continue;
      if (xs.length === ys.length || (xs.length < ys.length && xOpen) || (ys.length < xs.length && yOpen)) {
        return [x, y];
      }
    }
  }
  return null;
}

// ── Starting ──

/** Why a start must wait, or null. `lifts`: when each rate-limited lane of this harness may retry. */
export function waitReason(
  req: { name: string; harness: string; model: string; globs: string[]; force: boolean },
  lanes: Lane[],
  lifts: number[],
  now: number,
): string | null {
  if (req.force) return null;
  const running = lanes.filter((o) => o.status === "RUNNING");
  const lift = Math.max(...lifts); // a lane started into a limited account dies at once, with no session to resume
  if (lift > now) {
    return `REFUSED: the ${req.harness} account is at its limit until ${hhmm(lift)}; queue it, or --force`;
  }
  const [sameModel, modelCap] = [running.filter((o) => o.model === req.model).length, config.modelCaps[req.model]];
  if (sameModel >= modelCap) {
    return `REFUSED: ${sameModel} lanes already run on ${req.model} (cap ${modelCap}); wait or --force`;
  }
  const [same, cap] = [running.filter((o) => o.harness === req.harness).length, config.harnesses[req.harness].cap];
  if (same >= cap) return `REFUSED: ${req.harness} already runs ${same} lanes (cap ${cap}); wait or --force`;
  for (const o of running) {
    const hit = !o.readonly && o.globs?.length && req.globs.length ? overlap(req.globs, o.globs) : null;
    if (hit) {
      return `REFUSED: ${req.name} globs ${hit[0]} overlap running lane ${o.name} (${
        hit[1]
      }); sequence them or --force`;
    }
  }
  return null;
}

/** One attempt's argv, and the environment its harness adds to the lane's own. */
export function harnessCommand(l: Lane, prompt: string, resume: boolean, root: string) {
  const { kind, home } = config.harnesses[l.harness];
  const env = { ...config.harnesses[l.harness].env };
  const tools = l.tools ?? "full";
  const toolset = TOOLSETS[kind][tools];
  const cmd: string[] = [];
  if (kind === "omp") {
    cmd.push("omp", "-p", prompt, "--model", l.model, "--thinking", l.effort!, "--mode", "json", "--no-title");
    cmd.push("--max-time", config.launch.ompMaxTime, "--session-dir", `${root}/omp-sessions/${l.name}`);
    cmd.push(...(tools === "none" ? ["--no-tools"] : ["--tools", toolset.join(",")]));
    if (tools === "edit") cmd.push("--auto-approve");
    if (resume) cmd.push("--resume", l.session!);
  } else {
    cmd.push("claude", "-p", prompt, "--max-budget-usd", String(l.budget));
    cmd.push("--output-format", "json", "--permission-mode", "acceptEdits", "--allowedTools", ...toolset);
    if (l.chrome) cmd.push("mcp__claude-in-chrome", "--chrome");
    if (resume) cmd.push("--resume", l.session!);
    else cmd.push("--model", l.model, "--effort", l.effort!, "--session-id", l.session!);
    env.CLAUDE_CONFIG_DIR = home!;
  }
  const { nice, timeoutSeconds } = config.launch;
  return { argv: ["nice", "-n", String(nice), "timeout", String(timeoutSeconds), ...cmd], env };
}

// ── Quota (omp usage) ──

/** {window: percent left} for one `omp usage` account section; empty when unreadable. */
export function quotaLeft(usage: string, section: string): Record<string, number> {
  const left: Record<string, number> = {};
  let inside = false;
  for (const line of usage.split("\n")) {
    if (/^\S/.test(line)) inside = line.startsWith(section);
    const m = /●\s+(.+?)\s+[█░]+\s+([\d.]+)% used/.exec(line);
    if (inside && m) left[m[1].trim()] = Number((100 - Number(m[2])).toFixed(1));
  }
  return left;
}

/** Why this model must not start now, or null. An unreadable quota refuses too. */
export function quotaRefusal(model: string, usage: string): string | null {
  for (const { model: re, section, floor } of config.quotaFloors) {
    if (!new RegExp(re).test(model)) continue;
    const left = Object.entries(quotaLeft(usage, section));
    if (!left.length) return `cannot read ${section} quota (\`lanes quota\`); not starting ${model} blind`;
    const low = left.filter(([, v]) => v <= floor).map(([w, v]) => `${w} ${v}%`);
    if (low.length) return `${section} quota left (${low.join(", ")}) is at or below the floor of ${floor}%`;
  }
  return null;
}

// ── Quota (claude /usage) ──

export interface ClaudeUsage {
  session: { used: number; resets: string };
  week: { used: number; resets: string };
}
const WINDOW = /^Current (session|week \(all models\)):\s+(\d+)% used(?:\s+·\s+resets\s+(.+?))?\s*$/gm;

/** `claude -p /usage`: the 5-hour session and the all-models week; null when either is missing. */
export function parseClaudeUsage(text: string): ClaudeUsage | null {
  const w: Record<string, ClaudeUsage["week"]> = {};
  for (const [, which, used, resets] of text.matchAll(WINDOW)) {
    w[which[0]] = { used: Number(used), resets: resets ?? "" };
  }
  return w.s && w.w ? { session: w.s, week: w.w } : null;
}

/** Why a claude account may not start a lane now, or null. An unreadable account is not gated: its limit still lands as RATE_LIMIT. */
export function floorWhy(harness: string, u: ClaudeUsage | null | undefined): string | null {
  const { weeklyMax, sessionMax } = config.claudeQuota;
  const over = !u ? [] : [
    ...u.week.used >= weeklyMax ? [`week ${u.week.used}% used (stops at ${weeklyMax}%), resets ${u.week.resets}`] : [],
    ...u.session.used >= sessionMax
      ? [`5-hour session ${u.session.used}% used (stops at ${sessionMax}%), resets ${u.session.resets}`]
      : [],
  ];
  return over.length ? `${harness} is at its floor: ${over.join("; ")}` : null;
}

export const isoNow = (ms = Date.now()) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
export const hhmm = (epoch: number) => new Date(epoch * 1000).toISOString().slice(11, 16) + "Z";
export const oneLine = (s: string, n: number) => s.slice(0, n).replaceAll("\n", " ");
