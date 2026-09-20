// harness-bridge — invoke a sibling agent CLI under a safety contract.
// The contract is: --continue/-c rejected, --permission-mode plan forced,
// --fork-session forced on resume. Adding a new CLI is a one-line AGENTS
// map entry if it accepts the same argv shape.

import { parseArgs } from "jsr:@std/cli@1/parse-args";

const AGENTS = {
  claude: {
    bin: "claude",
    name: (mode: string, stamp: string) => `harness-bridge-claude-${mode}-${stamp}`,
  },
  "puku-cli": {
    bin: "puku-cli",
    name: (mode: string, stamp: string) => `harness-bridge-puku-${mode}-${stamp}`,
  },
} as const;

type AgentId = keyof typeof AGENTS;
const MODES = ["ask", "plan", "adversarial", "review"] as const;
type Mode = typeof MODES[number];

const SUBJECT_LIMIT = 20_000;
const DIFF_LIMIT = 1_000_000;

// ─── argv → ParsedArgs ───────────────────────────────────────────────────────

export function parseCliArgs(argv: readonly string[]) {
  const cleaned = argv[0] === "--" ? argv.slice(1) : argv;
  const [agentRaw, modeRaw, ...rest] = cleaned;
  if (!(agentRaw in AGENTS)) {
    throw new Error(
      `unknown agent: ${agentRaw ?? "(missing)"}. Supported: ${Object.keys(AGENTS).join(", ")}`,
    );
  }
  if (!(MODES as readonly string[]).includes(modeRaw)) {
    throw new Error(`unknown mode: ${modeRaw ?? "(missing)"}. Supported: ${MODES.join(", ")}`);
  }

  for (const arg of rest) {
    if (arg === "--") break;
    if (arg === "--continue" || arg === "-c") {
      throw new Error("--continue/-c resumes the most recent session globally. Use --resume <id>.");
    }
  }

  const p = parseArgs(rest, {
    boolean: ["fresh", "dry-run"],
    string: ["resume", "model", "base", "head", "extra", "cwd"],
    default: { base: "HEAD~1", head: "HEAD", fresh: false, "dry-run": false },
    unknown: (a) => !a.startsWith("-"),
  });

  const resume = (p.resume as string | undefined)?.trim() || undefined;
  const fresh = p.fresh === true;
  if (resume && fresh) throw new Error("--resume and --fresh are mutually exclusive");

  return {
    agent: agentRaw as AgentId,
    mode: modeRaw as Mode,
    positional: p._.map(String),
    base: (p.base as string) || "HEAD~1",
    head: (p.head as string) || "HEAD",
    fresh,
    resume,
    model: ((p.model as string | undefined) ?? "").trim() || undefined,
    extra: (p.extra as string) || "",
    cwd: ((p.cwd as string | undefined) ?? "").trim() || undefined,
    dryRun: p["dry-run"] === true,
  };
}

type ParsedArgs = ReturnType<typeof parseCliArgs>;

// ─── subject: bounded read of a file by path ─────────────────────────────────

export async function readSubject(
  subject: string,
  cwd: string,
): Promise<{ text: string; path: string; truncated: boolean }> {
  if (!subject) return { text: "", path: "", truncated: false };
  const target = subject.startsWith("/") ? subject : `${cwd}/${subject}`;
  try {
    const stat = await Deno.stat(target);
    if (!stat.isFile) return { text: "", path: "", truncated: false };
    const buf = new Uint8Array(SUBJECT_LIMIT);
    const file = await Deno.open(target, { read: true });
    try {
      const n = (await file.read(buf)) ?? 0;
      return {
        text: new TextDecoder().decode(buf.subarray(0, n)),
        path: target,
        truncated: n === SUBJECT_LIMIT,
      };
    } finally {
      file.close();
    }
  } catch {
    return { text: "", path: "", truncated: false };
  }
}

// ─── prompt + argv ───────────────────────────────────────────────────────────

const RULES = [
  "Use any resumed session history only as optional background; ignore it if unrelated.",
  "Do not edit files. Do not implement fixes. Do not run destructive commands.",
  "Be direct, skeptical, and specific. Prefer concrete risks and actionable changes over generic advice.",
].join("\n");

const PREFIX = (identity: string) => `${identity}\n\n${RULES}\n\n`;

export function buildPrompt(p: {
  mode: Mode;
  subject: string;
  subjectText: string;
  extra: string;
  base: string;
  head: string;
  diff: string;
  identity: string;
}): string {
  const safe = p.subjectText.replaceAll("```", "`\u200b``");
  const target = p.subjectText
    ? `${p.subject}\n\nTarget file contents (bounded):\n\`\`\`text\n${safe}\n\`\`\``
    : p.subject;
  const suffix = p.extra ? `\n\nAdditional instructions:\n${p.extra}` : "";
  const head = PREFIX(p.identity);
  switch (p.mode) {
    case "ask":
      return `${head}Question or task:\n${target}${suffix}`;
    case "plan":
      return `${head}Review this plan for correctness, missing steps, unclear assumptions, sequencing risk, test coverage, and overengineering:\n${target}${suffix}`;
    case "adversarial":
      return `${head}Perform an adversarial review of this target. Attack assumptions, hidden coupling, failure modes, security/operability risks, and weak tests. Return the strongest objections first, then suggested changes:\n${target}${suffix}`;
    case "review":
      return `${head}Perform a read-only adversarial code review for ${p.base}..${p.head}. Return findings by severity with evidence and a merge verdict.\n\nDiff:\n${p.diff}${suffix}`;
  }
}

export function buildCommand(p: {
  agent: AgentId;
  mode: Mode;
  prompt: string;
  resume?: string;
  newSessionId: string;
  model?: string;
  stamp: string;
  cwd: string;
}): { bin: string; args: string[]; cwd: string } {
  const a = AGENTS[p.agent];
  const args = ["-p", "--permission-mode", "plan"];
  if (p.model) args.push("--model", p.model);
  args.push("--name", a.name(p.mode, p.stamp));
  if (p.resume) args.push("--resume", p.resume, "--fork-session");
  else args.push("--session-id", p.newSessionId);
  args.push(p.prompt);
  return { bin: a.bin, args, cwd: p.cwd };
}

// ─── run ─────────────────────────────────────────────────────────────────────

export async function run(args: ParsedArgs, now: () => Date = () => new Date()): Promise<number> {
  if (args.mode === "review" && args.positional.length > 0) {
    throw new Error("review mode does not take a positional subject; use --base/--head");
  }
  if (args.mode !== "review" && args.positional.join(" ").trim() === "") {
    throw new Error(`${args.mode} mode requires a question, file path, or description`);
  }

  const cwd = args.cwd ?? Deno.cwd();
  const subject = args.positional.join(" ").trim();
  const identity = `You are ${
    args.agent === "claude" ? "Claude" : "Puku"
  }. Treat this as a one-shot task; do not assume ongoing context beyond the prompt below.`;

  // Build the prompt + meta in one branch.
  let prompt: string;
  let meta: Record<string, unknown> = {};
  if (args.mode === "review") {
    const range = `${args.base}..${args.head}`;
    const stat = await gitDiff(["diff", "--stat", range, "--"], cwd);
    const diff = await gitDiff(
      ["diff", "--no-ext-diff", "--find-renames", "--function-context", range, "--"],
      cwd,
    );
    if (stat.status !== 0 && !stat.truncated) {
      throw new Error(`git diff --stat failed (${stat.status})`);
    }
    if (diff.status !== 0 && !diff.truncated) throw new Error(`git diff failed (${diff.status})`);
    prompt = buildPrompt({
      mode: args.mode,
      subject: range,
      subjectText: "",
      extra: args.extra,
      base: args.base,
      head: args.head,
      diff: `## Diff stat\n${stat.stdout}\n\n## Diff\n${diff.stdout}`,
      identity,
    });
    meta = { diffTruncated: diff.truncated, diffStatTruncated: stat.truncated };
  } else {
    const subj = args.dryRun
      ? { text: "", path: "", truncated: false }
      : await readSubject(subject, cwd);
    const display = subj.path ? `${subject} (${subj.path})` : subject;
    prompt = buildPrompt({
      mode: args.mode,
      subject: display,
      subjectText: subj.truncated
        ? `${subj.text}\n\n[harness-bridge: target file content truncated]`
        : subj.text,
      extra: args.extra,
      base: args.base,
      head: args.head,
      diff: "",
      identity,
    });
    meta = {
      subjectTruncated: subj.truncated,
      targetResolvedPath: subj.path || undefined,
    };
  }

  const newSessionId = args.resume ? undefined : crypto.randomUUID();
  const stamp = now().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const cmd = buildCommand({
    agent: args.agent,
    mode: args.mode,
    prompt,
    resume: args.resume,
    newSessionId: newSessionId ?? "",
    model: args.model,
    stamp,
    cwd,
  });

  if (args.dryRun) {
    const redacted = cmd.args.map((a) =>
      a === prompt ? `[prompt redacted: ${prompt.length} chars]` : a
    );
    console.log(JSON.stringify(
      {
        agent: args.agent,
        model: args.model,
        resume: args.resume,
        fresh: args.fresh,
        newSessionId: args.resume ? null : newSessionId,
        prompt: { redacted: true, chars: prompt.length, ...meta },
        command: [cmd.bin, ...redacted],
      },
      null,
      2,
    ));
    return 0;
  }

  if (newSessionId) console.error(`harness-bridge: new session id = ${newSessionId}`);
  const child = new Deno.Command(cmd.bin, {
    args: cmd.args,
    cwd: cmd.cwd,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();
  return (await child.status).code;
}

async function gitDiff(
  args: string[],
  cwd: string,
): Promise<{ stdout: string; status: number; truncated: boolean }> {
  const r = await new Deno.Command("git", { args, cwd, stdout: "piped", stderr: "piped" }).output();
  const stdout = new TextDecoder().decode(r.stdout);
  return {
    stdout: stdout.length > DIFF_LIMIT
      ? stdout.slice(0, DIFF_LIMIT) + "\n[harness-bridge: diff truncated]"
      : stdout,
    status: r.code,
    truncated: stdout.length > DIFF_LIMIT,
  };
}

// ─── entry ───────────────────────────────────────────────────────────────────

if (import.meta.main) {
  try {
    Deno.exit(await run(parseCliArgs(Deno.args)));
  } catch (e) {
    console.error(`harness-bridge: ${e instanceof Error ? e.message : String(e)}`);
    Deno.exit(1);
  }
}
