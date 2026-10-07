#!/usr/bin/env -S deno run -A
// The lane runner's CLI. Usage: ../README.md, or --help.

import { parseArgs } from "node:util";
import * as db from "./db.ts";
import { defaultPaths, loadRegistry, withRegistry } from "./registry.ts";
import { config, type Registry } from "./rules.ts";
import * as run from "./runner.ts";
import * as view from "./view.ts";
import * as pool from "./worktrees.ts";

// Each command: its argument, and its options with their defaults ("" none, false a flag).
const d = config.defaults;
const LAUNCH = {
  cwd: "",
  prompt: "",
  harness: d.harness,
  model: d.model,
  effort: d.effort,
  budget: d.budget,
  tools: "",
};
const START = { ...LAUNCH, readonly: false, globs: "", task: "", chrome: false, force: false };
const COMMANDS: Record<string, [string, Record<string, string | number | boolean>]> = {
  start: ["NAME", START],
  queue: ["NAME", { ...START, after: "", light: false }],
  resume: ["NAME", { budget: d.budget, message: "", force: false }],
  release: ["NAME", {}],
  stop: ["NAME", { reason: "" }],
  forget: ["NAME", {}],
  quota: ["", {}],
  status: ["", { all: false, json: false }],
  watch: ["", { interval: 10, all: false }],
  events: ["", { heartbeat: 10 }],
  supervise: ["", { interval: config.supervisor.interval, "max-resumes": config.supervisor.maxResumes }],
  supervisor: ["status|restart|stop", {}],
  "db new": ["LANE", { worktree: "" }],
  "db drop": ["LANE", {}],
  "db sweep": ["", {}],
  "worktree take": ["PATH", { branch: "", from: "" }],
  "worktree fill": ["", {}],
  "worktree status": ["", {}],
};
const USAGE = "usage: lanes <command> [options]\n" +
  Object.entries(COMMANDS).map(([c, [arg, opts]]) =>
    `  ${c} ${arg} ` +
    Object.entries(opts).map(([k, v]) => `[--${k}${v === false ? "" : v === "" ? " X" : ` ${v}`}]`).join(" ")
  ).join("\n");

const usage = (why: string) => new run.Refusal(`${why}\n${USAGE}`, false, 2);
const number = (v: string, name: string) => {
  if (!/^\d+(\.\d+)?$/.test(v)) throw usage(`--${name} must be a number, not "${v}"`);
  return Number(v);
};
function oneOf<T extends string>(v: string, allowed: readonly T[], name: string): T {
  if (!allowed.includes(v as T)) throw usage(`${name} must be one of ${allowed.join(", ")}`);
  return v as T;
}

async function main([first, ...rest]: string[]) {
  // `lanes db new LANE` is one command of two words
  const command = first === "db" || first === "worktree" ? `${first} ${rest.shift()}` : first;
  if (!COMMANDS[command]) throw usage(command ? `unknown command ${command}` : "");
  const [arg, opts] = COMMANDS[command];
  const options = Object.fromEntries(
    Object.entries(opts).map(([k, v]) => [
      k,
      v === false
        ? { type: "boolean" as const, default: false }
        : { type: "string" as const, default: v ? `${v}` : undefined },
    ]),
  );
  const parsed = parseArgs({ args: rest, options, allowPositionals: true, strict: true });
  // deno-lint-ignore no-explicit-any
  const o = parsed.values as Record<string, any>;
  if (parsed.positionals.length !== +!!arg) throw usage(`${command} takes ${arg ? "one argument" : "no arguments"}`);
  const name = parsed.positionals[0];
  const paths = defaultPaths();
  const ctx: run.Ctx = { paths, seams: run.realSeams, say: (line) => console.log(line) };
  const locked = (fn: (r: Registry) => unknown) => withRegistry(paths, fn);
  const num = (k: string) => number(o[k], k);
  const startArgs = (): run.StartArgs => {
    if (!o.cwd || !o.prompt) throw usage("--cwd and --prompt are required");
    const harness = oneOf(o.harness, Object.keys(config.harnesses), "--harness");
    const { cwd, prompt, model, effort, globs = null, readonly, force, tools = null, task = null, chrome } = o;
    return {
      name,
      cwd,
      prompt,
      model,
      effort,
      budget: num("budget"),
      globs,
      readonly,
      force,
      harness,
      tools,
      task,
      chrome,
    };
  };
  const { interval, maxResumes } = config.supervisor;
  const self = [Deno.execPath(), "run", "-A", import.meta.filename!, "supervise", "--interval", `${interval}`];
  const RUN: Record<string, () => unknown> = {
    start: async () => {
      const a = startArgs();
      const why = await run.quotaPrecheck(a, ctx);
      return locked((r) => run.start(a, r, ctx, why));
    },
    queue: () => {
      const a = { ...startArgs(), after: o.after, light: o.light };
      return locked((r) => run.queue(a, r, ctx));
    },
    resume: async () => {
      const why = await run.resumePrecheck({ name, force: o.force }, ctx);
      return locked((r) => run.resume({ name, budget: num("budget"), message: o.message }, r, ctx, why));
    },
    release: () => locked((r) => run.release(name, r, ctx)),
    stop: () => locked((r) => run.stop({ name, reason: o.reason }, r, ctx)),
    forget: () => locked((r) => run.forget(name, r, ctx)),
    "db new": async () => {
      if (!o.worktree) throw usage("--worktree is required");
      const env = await db.create(ctx.seams.cluster, { lane: name, worktree: o.worktree });
      Object.entries(env).forEach(([k, v]) => ctx.say(`export ${k}=${v}`));
    },
    "db drop": async () => ctx.say(await db.drop(ctx.seams.cluster, name)),
    "db sweep": async () => {
      db.settings(); // refuses when the module is off, instead of reporting an empty sweep
      const dropped = await db.maintain(ctx.seams.cluster, loadRegistry(paths));
      ctx.say(dropped.length ? dropped.map((d) => `dropped ${d}`).join("\n") : "nothing to drop");
    },
    "worktree take": async () => {
      ctx.say(await pool.take(pool.poolSettings(), name, { branch: o.branch || undefined, from: o.from || undefined }));
    },
    "worktree fill": async () => {
      const pooled = await pool.fill(pool.poolSettings(), loadRegistry(paths));
      ctx.say(pooled.length ? pooled.map((p) => `pooled ${p}`).join("\n") : "nothing to pool");
    },
    "worktree status": async () => {
      const s = pool.poolSettings();
      ctx.say(`pool: ${pool.members(s).length} worktrees`);
      ctx.say(
        `recyclable now: ${(await pool.recyclable(s, loadRegistry(paths))).map((p) => p.split("/").pop()).join(" ")}`,
      );
    },
    quota: () => run.quota(ctx),
    status: () => view.status(o.all, o.json, paths),
    watch: () => view.watch(o.all, num("interval"), paths),
    events: () => view.events(num("heartbeat"), paths),
    supervise: () => run.supervise(num("interval"), num("max-resumes"), ctx),
    supervisor: () =>
      run.supervisor(oneOf(name, ["status", "restart", "stop"], "the action"), [
        ...self,
        "--max-resumes",
        `${maxResumes}`,
      ], ctx),
  };
  return await RUN[command]();
}

if (import.meta.main) {
  try {
    await (Deno.args.includes("--help") ? console.log(USAGE) : main(Deno.args));
  } catch (e) {
    const parse = (e as { code?: string }).code?.startsWith("ERR_PARSE_ARGS");
    if (!(e instanceof run.Refusal) && !parse) throw e;
    console.error(parse ? `${(e as Error).message}\n${USAGE}` : (e as Error).message);
    Deno.exit(parse ? 2 : (e as run.Refusal).exit);
  }
}
