// harness-bridge — invoke a sibling agent CLI under a tight safety contract.
//
// The wrapper enforces a small safety contract so a calling agent cannot
// accidentally extend the prior session, write to the repo, or resume the
// "most recent" session globally. Both supported CLIs (claude, puku-cli)
// accept the same argv shape, so the build step is shared.

import { parseArgs } from "jsr:@std/cli@1/parse-args";
import { isAbsolute, relative, resolve } from "jsr:@std/path@1";

// ─── Supported agents ────────────────────────────────────────────────────────
// Adding a new CLI is a one-line entry here, provided it accepts:
//   -p --permission-mode plan [--model M] [--name N]
//   (--resume ID --fork-session | --session-id UUID) <prompt>
//
// Each entry also gets a `sessionName` so the new session is identifiable
// in the child's own session list.

const AGENTS = {
  claude: {
    bin: "claude",
    identity:
      "You are Claude. Treat this as a one-shot task; do not assume ongoing context beyond the prompt below.",
    sessionName: (mode: string, stamp: string) => `harness-bridge-claude-${mode}-${stamp}`,
  },
  "puku-cli": {
    bin: "puku-cli",
    identity:
      "You are Puku. Treat this as a one-shot task; do not assume ongoing context beyond the prompt below.",
    sessionName: (mode: string, stamp: string) => `harness-bridge-puku-${mode}-${stamp}`,
  },
} as const;

type AgentId = keyof typeof AGENTS;

// ─── Constants ───────────────────────────────────────────────────────────────

const MODES = ["ask", "plan", "adversarial", "review"] as const;
type Mode = typeof MODES[number];

// Invariants the bridged CLI is asked to honour in every mode. These are the
// safety floor: read-only, no destructive commands, no session-drift from
// resumed history.
const BRIDGE_RULES = [
  "Use any resumed session history only as optional background; ignore it if unrelated.",
  "Do not edit files. Do not implement fixes. Do not run destructive commands.",
  "Be direct, skeptical, and specific. Prefer concrete risks and actionable changes over generic advice.",
].join("\n");

const SUBJECT_FILE_BYTE_LIMIT = 20_000;
const DIFF_BODY_BYTE_LIMIT = 1_000_000;
const DIFF_STAT_BYTE_LIMIT = 64_000;

// ─── CLI parsing ─────────────────────────────────────────────────────────────
// We parse twice on purpose: first with std/cli to strip flags, then we scan
// the remaining argv for `--continue` / `-c`. Doing it this way means a
// question like "should I use --continue?" (after `--`) still works — the
// scan stops at `--`.

export type ParsedArgs = {
  agent: AgentId;
  mode: Mode;
  positional: string[];
  base: string;
  head: string;
  fresh: boolean;
  resume?: string;
  model?: string;
  extra: string;
  cwd?: string;
  dryRun: boolean;
};

function rejectResumeShortcut(rest: readonly string[]): void {
  for (const arg of rest) {
    if (arg === "--") return;
    if (arg === "--continue" || arg === "-c") {
      throw new Error(
        "--continue/-c is not allowed; it resumes the most recent session globally. " +
          "Use --resume <id> with a session id from a prior run.",
      );
    }
  }
}

export function parseCliArgs(argv: readonly string[]): ParsedArgs {
  // `deno task run -- foo bar` passes `--` as the first argument; treat it
  // as a no-op separator.
  const cleaned = argv[0] === "--" ? argv.slice(1) : argv;
  const [agentRaw, modeRaw, ...rest] = cleaned;
  if (!agentRaw) throw new Error("missing agent (claude or puku-cli)");
  if (!(agentRaw in AGENTS)) {
    throw new Error(`unknown agent: ${agentRaw}. Supported: ${Object.keys(AGENTS).join(", ")}`);
  }
  if (!modeRaw) throw new Error(`missing mode (${MODES.join(" | ")})`);
  if (!(MODES as readonly string[]).includes(modeRaw)) {
    throw new Error(`unknown mode: ${modeRaw}. Supported: ${MODES.join(", ")}`);
  }

  rejectResumeShortcut(rest);

  const parsed = parseArgs(rest, {
    boolean: ["fresh", "dry-run"],
    string: ["resume", "model", "base", "head", "extra", "cwd"],
    default: { base: "HEAD~1", head: "HEAD", fresh: false, "dry-run": false },
    stopEarly: false,
    unknown: (arg) => !arg.startsWith("-"),
  });

  const resume = typeof parsed.resume === "string" && parsed.resume.trim()
    ? parsed.resume.trim()
    : undefined;
  const fresh = parsed.fresh === true;
  if (resume && fresh) throw new Error("--resume and --fresh are mutually exclusive");

  return {
    agent: agentRaw as AgentId,
    mode: modeRaw as Mode,
    positional: parsed._.map(String),
    base: (typeof parsed.base === "string" && parsed.base) || "HEAD~1",
    head: (typeof parsed.head === "string" && parsed.head) || "HEAD",
    fresh,
    resume,
    model: typeof parsed.model === "string" && parsed.model.trim()
      ? parsed.model.trim()
      : undefined,
    extra: typeof parsed.extra === "string" ? parsed.extra : "",
    cwd: typeof parsed.cwd === "string" && parsed.cwd.trim() ? parsed.cwd.trim() : undefined,
    dryRun: parsed["dry-run"] === true,
  };
}

// ─── Filesystem helpers ──────────────────────────────────────────────────────

export async function readBounded(
  path: string,
  maxBytes: number,
): Promise<{ text: string; truncated: boolean }> {
  const file = await Deno.open(path, { read: true });
  const chunks: Uint8Array[] = [];
  let stored = 0;
  let truncated = false;
  const buffer = new Uint8Array(64 * 1024);
  try {
    while (true) {
      const n = (await file.read(buffer)) ?? 0;
      if (n === 0) break;
      if (stored < maxBytes) {
        const remaining = maxBytes - stored;
        const take = Math.min(n, remaining);
        if (take > 0) {
          chunks.push(buffer.slice(0, take));
          stored += take;
        }
        if (n > remaining) truncated = true;
      } else {
        truncated = true;
      }
      if (truncated) break;
    }
  } finally {
    file.close();
  }
  const out = new Uint8Array(stored);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return { text: new TextDecoder().decode(out), truncated };
}

async function findRepoRoot(start: string): Promise<string> {
  // Try `git rev-parse --show-toplevel` first; fall back to walking up
  // looking for `.git`.
  try {
    const result = await new Deno.Command("git", {
      args: ["rev-parse", "--show-toplevel"],
      cwd: start,
      stdout: "piped",
      stderr: "null",
    }).output();
    if (result.success) {
      const text = new TextDecoder().decode(result.stdout).trim();
      if (text) return text;
    }
  } catch { /* fall through */ }

  let current = resolve(start);
  while (true) {
    try {
      const stat = await Deno.stat(`${current}/.git`);
      if (stat.isDirectory || stat.isFile) return current;
    } catch { /* not here */ }
    const parent = resolve(current, "..");
    if (parent === current) return resolve(start);
    current = parent;
  }
}

async function gitDiff(
  args: string[],
  cwd: string,
  maxBytes: number,
): Promise<{ stdout: string; status: number; truncated: boolean }> {
  const child = new Deno.Command("git", { args, cwd, stdout: "piped", stderr: "piped" }).spawn();
  let killed = false;
  const kill = () => {
    if (killed) return;
    killed = true;
    try {
      child.kill("SIGTERM");
    } catch { /* may already be exited */ }
  };

  const reader = child.stdout.getReader();
  const chunks: Uint8Array[] = [];
  let stored = 0;
  let truncated = false;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (stored < maxBytes) {
        const remaining = maxBytes - stored;
        const take = Math.min(value.byteLength, remaining);
        chunks.push(value.subarray(0, take));
        stored += take;
        if (value.byteLength > remaining) {
          truncated = true;
          kill();
          await reader.cancel().catch(() => undefined);
          break;
        }
      } else {
        truncated = true;
        kill();
        await reader.cancel().catch(() => undefined);
        break;
      }
    }
  } finally {
    reader.releaseLock();
  }
  const status = await child.status;
  const out = new Uint8Array(stored);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return { stdout: new TextDecoder().decode(out), status: status.code, truncated };
}

// ─── Subject loading ─────────────────────────────────────────────────────────
// Subject paths must resolve inside the repo root. We try the invocation cwd
// first (so `harness-bridge claude ask ./src/foo.ts` works), then the repo
// root.

async function readSubject(
  subject: string,
  invocationCwd: string,
  repoRoot: string,
): Promise<{ text: string; resolvedPath: string; truncated: boolean }> {
  if (!subject) return { text: "", resolvedPath: "", truncated: false };
  for (const base of [invocationCwd, repoRoot]) {
    const candidate = resolve(base, subject);
    try {
      const stat = await Deno.stat(candidate);
      if (!stat.isFile) continue;
      const real = await Deno.realPath(candidate);
      const realRoot = await Deno.realPath(repoRoot);
      const rel = relative(realRoot, real);
      if (rel.startsWith("..") || isAbsolute(rel)) {
        throw new Error(
          `subject path "${subject}" resolves outside the repo root (${repoRoot}); ` +
            `refusing to read it. Move the file inside the repo or pass it as a question.`,
        );
      }
      const read = await readBounded(real, SUBJECT_FILE_BYTE_LIMIT);
      return { text: read.text, resolvedPath: real, truncated: read.truncated };
    } catch (error) {
      if (error instanceof Error && error.message.includes("outside the repo root")) throw error;
      // try next candidate
    }
  }
  return { text: "", resolvedPath: "", truncated: false };
}

// ─── Prompt building ─────────────────────────────────────────────────────────

export function buildPrompt(
  mode: Mode,
  subject: string,
  subjectText: string,
  extra: string,
  base: string,
  head: string,
  diff: string,
  identity: string,
): string {
  // Strip stray ``` so a markdown fence in the subject can't escape ours.
  const safe = subjectText.replaceAll("```", "`\u200b``");
  const target = subjectText
    ? `${subject}\n\nTarget file contents (bounded):\n\`\`\`text\n${safe}\n\`\`\``
    : subject;
  const suffix = extra ? `\n\nAdditional instructions:\n${extra}` : "";
  switch (mode) {
    case "ask":
      return `${identity}\n\n${BRIDGE_RULES}\n\nQuestion or task:\n${target}${suffix}`;
    case "plan":
      return `${identity}\n\n${BRIDGE_RULES}\n\nReview this plan for correctness, missing steps, unclear assumptions, sequencing risk, test coverage, and overengineering:\n${target}${suffix}`;
    case "adversarial":
      return `${identity}\n\n${BRIDGE_RULES}\n\nPerform an adversarial review of this target. Attack assumptions, hidden coupling, failure modes, security/operability risks, and weak tests. Return the strongest objections first, then suggested changes:\n${target}${suffix}`;
    case "review":
      return `${identity}\n\n${BRIDGE_RULES}\n\nPerform a read-only adversarial code review for ${base}..${head}. Return findings by severity with evidence and a merge verdict.\n\nDiff:\n${diff}${suffix}`;
  }
}

// ─── Argv construction ───────────────────────────────────────────────────────
// Shared by claude and puku-cli. Both CLIs accept the same shape:
//   <bin> -p --permission-mode plan [--model M] [--name N]
//          (--resume ID --fork-session | --session-id UUID) <prompt>

export function buildCommand(
  agent: AgentId,
  prompt: string,
  resume: string | undefined,
  newSessionId: string,
  model: string | undefined,
  mode: Mode,
  stamp: string,
  repoRoot: string,
): { bin: string; args: string[]; cwd: string } {
  const a = AGENTS[agent];
  const args = ["-p", "--permission-mode", "plan"];
  if (model) args.push("--model", model);
  args.push("--name", a.sessionName(mode, stamp));
  if (resume) {
    args.push("--resume", resume, "--fork-session");
  } else {
    args.push("--session-id", newSessionId);
  }
  args.push(prompt);
  return { bin: a.bin, args, cwd: repoRoot };
}

// ─── Orchestration ───────────────────────────────────────────────────────────

export async function run(args: ParsedArgs, now: () => Date = () => new Date()): Promise<number> {
  if (args.mode === "review" && args.positional.length > 0) {
    throw new Error("review mode does not take a positional subject; use --base/--head");
  }
  if (args.mode !== "review" && args.positional.join(" ").trim() === "") {
    throw new Error(`${args.mode} mode requires a question, file path, or description`);
  }

  const invocationCwd = args.cwd ? resolve(args.cwd) : Deno.cwd();
  const repoRoot = await findRepoRoot(invocationCwd);

  let prompt: string;
  let promptMeta: Record<string, unknown> = {};

  if (args.mode === "review") {
    const range = `${args.base}..${args.head}`;
    const stat = await gitDiff(["diff", "--stat", range, "--"], repoRoot, DIFF_STAT_BYTE_LIMIT);
    if (stat.status !== 0 && !stat.truncated) {
      throw new Error(`git diff --stat failed (status ${stat.status})`);
    }
    const diff = await gitDiff(
      ["diff", "--no-ext-diff", "--find-renames", "--function-context", range, "--"],
      repoRoot,
      DIFF_BODY_BYTE_LIMIT,
    );
    if (diff.status !== 0 && !diff.truncated) {
      throw new Error(`git diff failed (status ${diff.status})`);
    }
    const statText = stat.stdout +
      (stat.truncated ? "\n[harness-bridge: diff stat truncated]" : "");
    const diffText = diff.stdout + (diff.truncated ? "\n[harness-bridge: diff truncated]" : "");
    prompt = buildPrompt(
      args.mode,
      range,
      "",
      args.extra,
      args.base,
      args.head,
      `## Diff stat\n${statText}\n\n## Diff\n${diffText}`,
      AGENTS[args.agent].identity,
    );
    promptMeta = { diffTruncated: diff.truncated, diffStatTruncated: stat.truncated };
  } else {
    const subject = args.positional.join(" ").trim();
    const subjectFile = args.dryRun
      ? { text: "", resolvedPath: "", truncated: false }
      : await readSubject(subject, invocationCwd, repoRoot);
    const display = subjectFile.resolvedPath ? `${subject} (${subjectFile.resolvedPath})` : subject;
    const subjectText = subjectFile.truncated
      ? `${subjectFile.text}\n\n[harness-bridge: target file content truncated]`
      : subjectFile.text;
    prompt = buildPrompt(
      args.mode,
      display,
      subjectText,
      args.extra,
      args.base,
      args.head,
      "",
      AGENTS[args.agent].identity,
    );
    promptMeta = {
      subjectTruncated: subjectFile.truncated,
      targetResolvedPath: subjectFile.resolvedPath || undefined,
    };
  }

  const newSessionId = args.resume ? undefined : crypto.randomUUID();
  const stamp = now().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const command = buildCommand(
    args.agent,
    prompt,
    args.resume,
    newSessionId ?? "",
    args.model,
    args.mode,
    stamp,
    repoRoot,
  );

  const redactedArgs = command.args.map((a) =>
    a === prompt ? `[prompt redacted: ${prompt.length} chars]` : a
  );

  if (args.dryRun) {
    const summary = {
      agent: args.agent,
      model: args.model,
      resume: args.resume,
      fresh: args.fresh,
      newSessionId: args.resume ? null : newSessionId,
      prompt: { redacted: true, chars: prompt.length, ...promptMeta },
      command: [command.bin, ...redactedArgs],
    };
    console.log(JSON.stringify(summary, null, 2));
    return 0;
  }

  // The agent must be able to discover the new session id to resume later.
  if (newSessionId) {
    console.error(`harness-bridge: new session id = ${newSessionId}`);
  }

  const child = new Deno.Command(command.bin, {
    args: command.args,
    cwd: command.cwd,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();
  const status = await child.status;
  return status.code;
}

// ─── Entry point ─────────────────────────────────────────────────────────────

if (import.meta.main) {
  try {
    const parsed = parseCliArgs(Deno.args);
    Deno.exit(await run(parsed));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`harness-bridge: ${message}`);
    Deno.exit(1);
  }
}

// ─── Tests live in main_test.ts ──────────────────────────────────────────────
// Keeping the source file free of Deno.test blocks makes the production
// entry point scannable. The tests pin the safety contract; if they break,
// SKILL.md and this file must change together.
