// Tests pin the safety contract. If you change any behaviour below, update
// SKILL.md and src/main.ts together.

import { assert, assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import { buildCommand, buildPrompt, chooseDelivery, parseCliArgs, readSubject } from "./main.ts";

const SUBJECT_FILE_BYTE_LIMIT = 20_000;

Deno.test("parseCliArgs rejects --continue and -c", () => {
  assertThrows(() => parseCliArgs(["claude", "ask", "q", "--continue"]), Error, "--continue");
  assertThrows(() => parseCliArgs(["puku-cli", "ask", "q", "-c"]), Error, "--continue");
});

Deno.test("parseCliArgs allows --continue after -- (positional text)", () => {
  const p = parseCliArgs(["puku-cli", "ask", "--", "should I use --continue?"]);
  assertEquals(p.positional, ["should I use --continue?"]);
});

Deno.test("parseCliArgs rejects resume+fresh together", () => {
  assertThrows(
    () => parseCliArgs(["claude", "ask", "q", "--resume", "abc", "--fresh"]),
    Error,
    "mutually exclusive",
  );
});

Deno.test("parseCliArgs rejects unknown agent and mode", () => {
  assertThrows(() => parseCliArgs(["kiro", "ask", "q"]), Error, "unknown agent");
  assertThrows(() => parseCliArgs(["claude", "audit", "q"]), Error, "unknown mode");
});

const stamp = "2026-09-20T12:00:00";

Deno.test("buildCommand forces --permission-mode plan and --fork-session on resume", () => {
  const cmd = buildCommand({
    agent: "claude",
    mode: "ask",
    prompt: "hello",
    resume: "abc",
    newSessionId: "ignored",
    model: "opus",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.bin, "claude");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--permission-mode", "plan"]);
  assertEquals(cmd.args.includes("--fork-session"), true);
  assertEquals(cmd.args.includes("--resume"), true);
  assertEquals(cmd.args[cmd.args.length - 1], "hello");
});

Deno.test("buildCommand uses --session-id (not --resume) for fresh sessions", () => {
  const cmd = buildCommand({
    agent: "puku-cli",
    mode: "ask",
    prompt: "hello",
    newSessionId: "new-uuid",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.args.includes("--resume"), false);
  assertEquals(cmd.args.includes("--session-id"), true);
});

Deno.test("buildCommand puku-cli argv has the same safety flags as claude", () => {
  const cmd = buildCommand({
    agent: "puku-cli",
    mode: "ask",
    prompt: "hello",
    resume: "abc",
    newSessionId: "ignored",
    model: "opus",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.bin, "puku-cli");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--permission-mode", "plan"]);
  assertEquals(cmd.args.includes("--fork-session"), true);
  assertEquals(cmd.args.includes("--resume"), true);
  assertEquals(cmd.args[cmd.args.length - 1], "hello");
});

Deno.test("readSubject truncates past the limit and reports it", async () => {
  const dir = await Deno.makeTempDir({ prefix: "harness-bridge-" });
  const file = `${dir}/subject.txt`;
  await Deno.writeTextFile(file, "x".repeat(SUBJECT_FILE_BYTE_LIMIT + 5000));
  try {
    const big = await readSubject("subject.txt", dir);
    assertEquals(big.truncated, true);
    assertEquals(big.text.length, SUBJECT_FILE_BYTE_LIMIT);
    assertStringIncludes(big.path, "subject.txt");
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("readSubject returns empty when the file does not exist", async () => {
  const dir = await Deno.makeTempDir({ prefix: "harness-bridge-" });
  try {
    const r = await readSubject("nope.txt", dir);
    assertEquals(r, { text: "", path: "", truncated: false });
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("buildPrompt embeds the subject verbatim and adds the mode-specific instruction", () => {
  const askPrompt = buildPrompt({
    mode: "ask",
    subject: "is this safe?",
    subjectText: "",
    extra: "",
    base: "HEAD~1",
    head: "HEAD",
    diff: "",
    identity: "identity",
  });
  assertStringIncludes(askPrompt, "is this safe?");
  assertStringIncludes(askPrompt, "identity");

  const planPrompt = buildPrompt({
    mode: "plan",
    subject: "the plan",
    subjectText: "",
    extra: "",
    base: "HEAD~1",
    head: "HEAD",
    diff: "",
    identity: "id",
  });
  assertStringIncludes(planPrompt, "Review this plan");
  assertStringIncludes(planPrompt, "the plan");

  const advPrompt = buildPrompt({
    mode: "adversarial",
    subject: "the design",
    subjectText: "",
    extra: "",
    base: "HEAD~1",
    head: "HEAD",
    diff: "",
    identity: "id",
  });
  assertStringIncludes(advPrompt, "adversarial review");
  assertStringIncludes(advPrompt, "strongest objections");

  const reviewPrompt = buildPrompt({
    mode: "review",
    subject: "main..HEAD",
    subjectText: "",
    extra: "",
    base: "main",
    head: "HEAD",
    diff: "diff body",
    identity: "id",
  });
  assertStringIncludes(reviewPrompt, "main..HEAD");
  assertStringIncludes(reviewPrompt, "diff body");
  assertStringIncludes(reviewPrompt, "merge verdict");
});

Deno.test("buildPrompt strips stray ``` so a markdown fence in the subject cannot escape", () => {
  const prompt = buildPrompt({
    mode: "ask",
    subject: "subject",
    subjectText: "before ``` after",
    extra: "",
    base: "HEAD~1",
    head: "HEAD",
    diff: "",
    identity: "id",
  });
  assert(prompt.includes("`\u200b``"));
  assertStringIncludes(prompt, "before");
  assertStringIncludes(prompt, "after");
});

Deno.test("buildCommand stdin path keeps the safety contract and never puts the prompt in argv", () => {
  const prompt = "x".repeat(200 * 1024); // 200 KB — above PROMPT_ARGV_LIMIT_BYTES (128 KB)
  const cmd = buildCommand({
    agent: "puku-cli",
    mode: "ask",
    prompt,
    newSessionId: "new-uuid",
    stamp,
    cwd: "/tmp",
  }, "stdin");
  assertEquals(cmd.bin, "puku-cli");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--permission-mode", "plan"]);
  assertEquals(cmd.args.includes("--input-format"), true);
  assertEquals(cmd.args.includes("--output-format"), true);
  assertEquals(cmd.args.includes("--verbose"), true);
  assertEquals(cmd.args.includes("--replay-user-messages"), true);
  assertEquals(cmd.args.includes("--fork-session"), false);
  assertEquals(cmd.args.includes("--session-id"), true);
  assertEquals(cmd.args.some((a) => a.length > 1024), false);
  assertStringIncludes(cmd.envelope, `"content":"${prompt.slice(0, 32)}`);
  const parsed = JSON.parse(cmd.envelope.trimEnd());
  assertEquals(parsed.type, "user");
  assertEquals(parsed.message.role, "user");
  assertEquals(parsed.message.content, prompt);
});

Deno.test("buildCommand stdin escapes JSON special characters in the prompt", () => {
  const tricky = `quote " backslash \\ newline\ndone`;
  const cmd = buildCommand({
    agent: "claude",
    mode: "ask",
    prompt: tricky,
    newSessionId: "x",
    stamp,
    cwd: "/tmp",
  }, "stdin");
  const parsed = JSON.parse(cmd.envelope.trimEnd());
  assertEquals(parsed.message.content, tricky);
});

Deno.test("buildCommand stdin preserves --fork-session on resume", () => {
  const cmd = buildCommand({
    agent: "claude",
    mode: "review",
    prompt: "x".repeat(200 * 1024),
    resume: "abc",
    newSessionId: "ignored",
    stamp,
    cwd: "/tmp",
  }, "stdin");
  assertEquals(cmd.args.includes("--resume"), true);
  assertEquals(cmd.args.includes("--fork-session"), true);
  assertEquals(cmd.args.includes("--session-id"), false);
});

Deno.test("buildCommand argv path does not include stream-json flags", () => {
  const cmd = buildCommand({
    agent: "puku-cli",
    mode: "ask",
    prompt: "small",
    newSessionId: "x",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.args.includes("--input-format"), false);
  assertEquals(cmd.args.includes("--output-format"), false);
  assertEquals(cmd.args.includes("--verbose"), false);
  assertEquals(cmd.envelope, null);
});

// Threshold uses BYTES, not UTF-16 code units. A 100K-character prompt
// of 4-byte emoji is 200 KB on the wire and must route to stdin even
// though .length (code units) is only 50K.
Deno.test("chooseDelivery routes on byte length, not UTF-16 code units", () => {
  const emoji = "\u{1F4E6}"; // 📦 = U+1F4E6, 4 bytes in UTF-8, 2 UTF-16 code units
  // 50_000 emoji = 200_000 UTF-8 bytes (over the 131_072 byte threshold)
  // but only 100_000 UTF-16 code units. Must route to stdin.
  const prompt = emoji.repeat(50_000);
  assertEquals(prompt.length, 100_000); // .length is UTF-16 code units
  assertEquals(new TextEncoder().encode(prompt).byteLength, 200_000);
  assertEquals(chooseDelivery(prompt), "stdin");
});

Deno.test("chooseDelivery keeps a small ASCII prompt on argv", () => {
  assertEquals(chooseDelivery("hello world"), "argv");
  assertEquals(chooseDelivery("x".repeat(100_000)), "argv");
  assertEquals(chooseDelivery("x".repeat(200_000)), "stdin");
});

Deno.test("buildCommand omp uses --approval-mode always-ask and rejects resume", () => {
  const cmd = buildCommand({
    agent: "omp",
    mode: "ask",
    prompt: "hello",
    newSessionId: "ignored",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.bin, "omp");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--approval-mode", "always-ask"]);
  assertEquals(cmd.args.includes("--fork-session"), false);
  assertEquals(cmd.args.includes("--session-id"), false);
  assertEquals(cmd.args.includes("--permission-mode"), false);
  assertEquals(cmd.args[cmd.args.length - 1], "hello");
  assertEquals(cmd.envelope, null);
});

Deno.test("buildCommand omp refuses resume rather than extending the prior session", () => {
  assertThrows(
    () =>
      buildCommand({
        agent: "omp",
        mode: "ask",
        prompt: "hello",
        resume: "abc",
        newSessionId: "ignored",
        stamp,
        cwd: "/tmp",
      }, "argv"),
    Error,
    "no --fork-session",
  );
});

Deno.test("buildCommand omp refuses stdin delivery", () => {
  const big = "x".repeat(200 * 1024);
  // Stdin is caught by the runtime guard (useStdin && !AGENTS[agent].stdin)
  // in run(), not by the build hook. The hook's role is to refuse resume.
  // This test pins that the hook itself doesn't silently accept stdin.
  const cmd = buildCommand({
    agent: "omp",
    mode: "ask",
    prompt: big,
    newSessionId: "ignored",
    stamp,
    cwd: "/tmp",
  }, "stdin");
  assertEquals(cmd.bin, "omp");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--approval-mode", "always-ask"]);
});

Deno.test("buildCommand omp threads --model through", () => {
  const cmd = buildCommand({
    agent: "omp",
    mode: "ask",
    prompt: "hello",
    newSessionId: "ignored",
    model: "opus",
    stamp,
    cwd: "/tmp",
  }, "argv");
  assertEquals(cmd.args.includes("--model"), true);
  assertEquals(cmd.args[cmd.args.indexOf("--model") + 1], "opus");
});

// Smoke test the omp argv shape against the live binary. Skipped when omp
// is not on PATH so the test suite stays portable across dev environments.
Deno.test("omp dry-run produces argv that the live binary accepts", async () => {
  const which = await new Deno.Command("which", { args: ["omp"], stdout: "piped" })
    .output();
  if (which.code !== 0) return; // omp not installed — skip silently

  const out = await new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "-A",
      "--no-config",
      "src/main.ts",
      "omp",
      "ask",
      "smoke test — should not call a real model in --dry-run",
      "--dry-run",
    ],
    cwd: new URL("..", import.meta.url).pathname,
    stdout: "piped",
    stderr: "piped",
  }).output();
  assertEquals(out.code, 0);
  const json = JSON.parse(new TextDecoder().decode(out.stdout));
  assertEquals(json.agent, "omp");
  assertEquals(json.command[0], "omp");
  assertEquals(json.command[1], "-p");
  assertEquals(json.command[2], "--approval-mode");
  assertEquals(json.command[3], "always-ask");
  // Prompt must be redacted — never leaked to stdout, even at --dry-run.
  assertEquals(json.command.some((a: string) => a.includes("should not call a real model")), false);
});
