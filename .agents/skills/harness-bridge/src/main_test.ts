// Tests pin the safety contract. If you change any behaviour below, update
// SKILL.md and src/main.ts together.

import { assert, assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import { buildCommand, buildPrompt, parseCliArgs, readSubject } from "./main.ts";

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
  });
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
  });
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
  });
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
