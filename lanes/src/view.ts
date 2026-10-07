// The board (status, watch) drawn by gum, `status --json` for scripts, and the `events` log.

import { basename } from "node:path";
import { loadRegistry, mtime, type Paths, readText, refresh } from "./registry.ts";
import { config, isoNow, kindOf, type Lane, LIVE, oneLine, parseOmp, type Registry } from "./rules.ts";
import { cachedUsage, describe, supervisorPid } from "./runner.ts";

const RANK: Record<string, number> = { RUNNING: 0, RATE_LIMIT: 1, QUEUED: 2 };
const HUMAN: readonly string[] = ["EARLY_END", "ERROR", "BUDGET", "NO_OUTPUT"];
const JSON_FIELDS = ["name", "harness", "model", "effort", "status", "task", "resumes", "after", "waiting"];

const age = (s: number) => {
  const [m, h, d] = [Math.floor(s / 60), Math.floor(s / 3600), Math.floor(s / 86400)];
  const pad = (n: number) => String(n).padStart(2, "0");
  return s < 60
    ? `${Math.floor(Math.max(0, s))}s`
    : s < 3600
    ? `${m}m`
    : s < 86400
    ? `${h}h${pad(m % 60)}m`
    : `${d}d${pad(h % 24)}h`;
};

/** The rows: live lanes (or all), then the queue, by state, harness and start. */
export function entries(r: Registry, all: boolean) {
  const lanes = Object.values(r.lanes).filter((l) => all || LIVE.includes(l.status)).map((l) => ({
    name: l.name,
    harness: l.harness,
    model: l.model,
    effort: l.effort ?? null,
    status: l.status as string,
    task: l.task || null,
    resumes: l.resumes,
    after: null as string | null,
    waiting: null as string | null,
    since: (l.started ?? l.created) as string | null,
    lane: l as Lane | undefined,
  }));
  const queued = r.queue.map((q) => ({
    ...q,
    status: "QUEUED",
    task: q.task || null,
    resumes: 0,
    after: q.after ?? null,
    waiting: q.waiting ?? null,
    since: null,
    lane: undefined,
  }));
  const order = Object.keys(config.harnesses);
  const key = (e: { status: string; harness: string }) => (RANK[e.status] ?? 3) * 100 + order.indexOf(e.harness);
  return [...lanes, ...queued].sort((a, b) => key(a) - key(b) || (a.since ?? "").localeCompare(b.since ?? ""));
}
export type Entry = ReturnType<typeof entries>[number];
export const asJson = (es: Entry[]) => JSON.stringify(es, JSON_FIELDS, 1);

/** The Claude Code transcript of a lane's current attempt: by session id, else by the marker its prompt carries. */
function transcriptFor(l: Lane): string | null {
  const home = config.harnesses[l.harness]?.home;
  const dir = home && l.cwd && `${home}/projects/${l.cwd.replace(/[/.]/g, "-")}`;
  let names: string[] = [];
  try {
    if (dir && kindOf(l.harness) !== "omp") names = [...Deno.readDirSync(dir)].map((e) => e.name);
  } catch { /* no transcripts */ }
  const cands = names.filter((n) => n.endsWith(".jsonl")).map((n) => `${dir}/${n}`)
    .sort((a, b) => (mtime(b) ?? 0) - (mtime(a) ?? 0));
  const head = (path: string) => {
    using f = Deno.openSync(path);
    const buf = new Uint8Array(60000);
    return new TextDecoder().decode(buf.subarray(0, f.readSync(buf) ?? 0));
  };
  return cands.find((c) => l.session && basename(c).startsWith(l.session)) ??
    (l.marker ? cands.slice(0, 30).find((c) => head(c).includes(l.marker!)) : undefined) ?? null;
}

/** Tool calls so far, the latest call ("now") and sentence ("said"), and seconds since the lane last wrote. */
function activity(l: Lane) {
  const quiet = (path?: string | null) => Date.now() / 1000 - (mtime(path) ?? NaN);
  if (kindOf(l.harness) === "omp") {
    const o = parseOmp(readText(l.out) ?? "");
    return { calls: o.calls, action: o.lastAction, said: oneLine(o.text, 90), quiet: quiet(l.out) };
  }
  const t = transcriptFor(l);
  const a = { calls: 0, action: "", said: "", quiet: quiet(t ?? l.out) };
  for (const line of (readText(t) ?? "").split("\n")) {
    let e;
    try {
      e = JSON.parse(line);
    } catch {
      continue;
    }
    if (e?.type !== "assistant" || !Array.isArray(e.message?.content)) continue;
    for (const c of e.message.content) {
      const i = c.input ?? {};
      if (c.type === "tool_use") a.calls += 1;
      if (c.type === "tool_use") {
        a.action = `${c.name} ${i.file_path || i.command || i.pattern || i.path || i.description || ""}`;
      } else if (c.type === "text" && c.text?.trim()) a.said = oneLine(c.text.trim(), 90);
    }
  }
  return { ...a, action: a.action.replaceAll("\n", " ") };
}

async function gum(args: string[], input?: string): Promise<string> {
  const env = Deno.stdout.isTerminal() ? { CLICOLOR_FORCE: "1" } : undefined;
  let p;
  try {
    p = new Deno.Command("gum", { args, env, stdin: input === undefined ? "null" : "piped", stdout: "piped" }).spawn();
  } catch {
    throw new Error("gum is not installed (`status --json` needs no gum)");
  }
  if (input !== undefined) {
    const w = p.stdin.getWriter();
    await w.write(new TextEncoder().encode(input));
    await w.close();
  }
  return new TextDecoder().decode((await p.output()).stdout).trimEnd();
}

/** A gum table with its last column clipped to the width left (gum ignores --widths when printing). */
function table(rows: string[][], width: number) {
  const w = rows[0].map((_, i) => Math.max(...rows.map((r) => r[i].length)));
  const room = Math.max(10, width - 3 * w.length - 1 - w.slice(0, -1).reduce((a, b) => a + b, 0));
  const clip = (s: string) => s.length > room ? s.slice(0, room - 1) + "…" : s;
  const csv = rows.map((r) =>
    [...r.slice(0, -1), clip(r.at(-1)!)].map((c) => `"${c.replaceAll('"', '""')}"`).join(",")
  );
  return gum(["table", "--print", "--border", "rounded", "--separator", ","], csv.join("\n"));
}

function summary(r: Registry, p: Paths) {
  const lanes = Object.values(r.lanes);
  const week = cachedUsage(p);
  const per = Object.entries(config.harnesses).map(([h, { cap }]) => {
    const queued = r.queue.filter((q) => q.harness === h).length;
    const used = week[h]?.usage?.week.used;
    return `${h} ${lanes.filter((l) => l.harness === h && l.status === "RUNNING").length}/${cap}` +
      (used === undefined ? "" : ` ${used}%w`) + (queued ? ` +${queued}q` : "");
  });
  const sup = supervisorPid(p);
  return `${per.join(" · ")}   supervisor ${sup ? `on (pid ${sup})` : "OFF"}`;
}

const terminalWidth = () =>
  Deno.stdout.isTerminal() ? Deno.consoleSize().columns : Number(Deno.env.get("COLUMNS")) || 120;

/** The board; with `live`, each live lane's latest tool call and sentence under it. */
export async function board(p: Paths, all: boolean, live: boolean, width = terminalWidth(), now = Date.now() / 1000) {
  const r = loadRegistry(p);
  refresh(r);
  const es = entries(r, all);
  const rows = es.map((e) => [
    e.harness,
    String(e.model).split("/").pop() + (e.effort ? `@${e.effort}` : ""),
    e.name,
    e.status,
    e.since ? age(now - Date.parse(e.since) / 1000) : "",
    [e.waiting ?? (e.after && `after ${e.after}`), e.task].filter(Boolean).join(" · "),
  ]);
  const tables = [table([["ACCOUNT", "MODEL", "LANE", "STATE", "AGE", "TASK"], ...rows], width)];
  if (live) {
    const acts = es.filter((e) => e.lane && LIVE.includes(e.status)).flatMap(({ name, status, lane }) => {
      if (status !== "RUNNING") return [[name, `note  ${lane!.last ?? ""}`]];
      const a = activity(lane!);
      const quiet = isNaN(a.quiet) ? "" : a.quiet < 60 ? " · active now" : ` · quiet ${age(a.quiet)}`;
      return [[name, `now   ${a.action || "(no tool call yet)"}`], [
        `${a.calls} tools${quiet}`,
        a.said && `said  ${a.said}`,
      ]];
    });
    tables.push(table([["LANE", "ACTIVITY"], ...acts], width));
  }
  const hidden = Object.values(r.lanes).filter((l) => !es.some((e) => e.lane === l));
  const stuck = hidden.filter((l) => HUMAN.includes(l.status)).map((l) => `${l.name} (${l.status})`);
  const footer = [
    stuck.length ? `needs a human: ${stuck.join(", ")}` : "",
    all ? "" : `${hidden.length} ended lanes: status --all`,
    live ? "Ctrl+C to leave · quota: lanes quota" : "",
  ].filter(Boolean).join("\n");
  const parts = await Promise.all([
    gum(["style", "--bold", "--foreground", "212", `LANES  ${isoNow(now * 1000).slice(0, 16).replace("T", " ")}Z`]),
    gum(["style", "--faint", summary(r, p)]),
    ...tables,
    ...(footer ? [gum(["style", "--faint", "--width", String(width), footer])] : []),
  ]);
  return parts.join("\n");
}

export async function status(all: boolean, json: boolean, p: Paths) {
  if (!json) return console.log(await board(p, all, false));
  const r = loadRegistry(p);
  refresh(r);
  console.log(asJson(entries(r, all)));
}

export async function watch(all: boolean, interval: number, p: Paths) {
  while (true) {
    // A registry that cannot be read (a bad hand edit, a write in progress) is shown and retried: the board is a
    // view, so it outlives a bad frame instead of exiting.
    const frame = await board(p, all, true).catch((e) => `registry unreadable, retrying: ${describe(e)}`);
    console.clear();
    console.log(frame);
    await new Promise((done) => setTimeout(done, interval * 1000));
  }
}

/** One line per change: state transitions, a new commit per lane directory, and a heartbeat. */
export async function events(heartbeat: number, p: Paths) {
  const say = (line: string) => console.log(`[${isoNow()}] ${line}`);
  const [seen, heads] = [new Map<string, string>(), new Map<string, string>()];
  let [beat, up] = [Date.now(), undefined as boolean | undefined];
  while (true) {
    const r = loadRegistry(p);
    refresh(r);
    const sup = Boolean(supervisorPid(p));
    if (sup !== up && (up !== undefined || !sup)) {
      say(
        `supervisor is ${sup ? "running" : "NOT RUNNING: early-ended lanes will not be resumed (supervisor restart)"}`,
      );
    }
    up = sup;
    for (const l of Object.values(r.lanes)) {
      const [was, h] = [seen.get(l.name), l.harness];
      const tag = `${h}/${l.model.split("/").pop()}${kindOf(h) === "claude" && l.effort ? `@${l.effort}` : ""}`;
      if (was && was !== l.status) say(`[${tag}] ${l.name}: ${was} -> ${l.status}  ${(l.last ?? "").slice(0, 110)}`);
      seen.set(l.name, l.status);
    }
    for (const cwd of new Set(Object.values(r.lanes).filter((l) => l.cwd && !l.readonly).map((l) => l.cwd!))) {
      const git = new Deno.Command("git", { args: ["-C", cwd, "log", "-1", "--format=%h %cr %s"], stderr: "null" });
      const out = await git.output().catch(() => null);
      const c = out?.success ? new TextDecoder().decode(out.stdout).trim().slice(0, 70) : "";
      const was = heads.get(cwd);
      if (was !== undefined && c && was.split(" ")[0] !== c.split(" ")[0]) say(`new commit in ${cwd}: ${c}`);
      heads.set(cwd, c);
    }
    if (Date.now() - beat > heartbeat * 60_000) {
      beat = Date.now();
      const tools = Object.values(r.lanes).filter((l) => l.status === "RUNNING")
        .map((l) => `${l.name}:${activity(l).calls} tools`).join("; ");
      say(`heartbeat: ${summary(r, p)} | ${tools || "no lane running"}`);
    }
    await new Promise((done) => setTimeout(done, 30_000));
  }
}
