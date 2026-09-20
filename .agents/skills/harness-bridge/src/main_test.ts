// Tests pin the safety contract. If you change any behaviour below, update
// SKILL.md and src/main.ts together.

import { assert, assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import { buildCommand, buildPrompt, parseCliArgs, readBounded } from "./main.ts";

Deno.test("parseCliArgs rejects --continue and -c", () => {
  assertThrows(() => parseCliArgs(["claude", "ask", "q", "--continue"]), Error, "--continue/-c");
  assertThrows(() => parseCliArgs(["puku-cli", "ask", "q", "-c"]), Error, "--continue/-c");
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

Deno.test("buildCommand forces --permission-mode plan and --fork-session on resume", () => {
  const cmd = buildCommand(
    "claude",
    "hello",
    "abc",
    "ignored",
    "opus",
    "ask",
    "2026-09-20T12:00:00",
    "/tmp",
  );
  assertEquals(cmd.bin, "claude");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--permission-mode", "plan"]);
  assertEquals(cmd.args.includes("--fork-session"), true);
  assertEquals(cmd.args.includes("--resume"), true);
  assertEquals(cmd.args[cmd.args.length - 1], "hello");
});

Deno.test("buildCommand uses --session-id (not --resume) for fresh sessions", () => {
  const cmd = buildCommand(
    "puku-cli",
    "hello",
    undefined,
    "new-uuid",
    undefined,
    "ask",
    "x",
    "/tmp",
  );
  assertEquals(cmd.args.includes("--resume"), false);
  assertEquals(cmd.args.includes("--session-id"), true);
});

Deno.test("buildCommand puku-cli argv has the same safety flags as claude", () => {
  const cmd = buildCommand(
    "puku-cli",
    "hello",
    "abc",
    "ignored",
    "opus",
    "ask",
    "2026-09-20T12:00:00",
    "/tmp",
  );
  assertEquals(cmd.bin, "puku-cli");
  assertEquals(cmd.args.slice(0, 3), ["-p", "--permission-mode", "plan"]);
  assertEquals(cmd.args.includes("--fork-session"), true);
  assertEquals(cmd.args.includes("--resume"), true);
  assertEquals(cmd.args[cmd.args.length - 1], "hello");
});

Deno.test("readBounded truncates past maxBytes and reports it", async () => {
  const path = await Deno.makeTempFile({ prefix: "harness-bridge-", suffix: ".txt" });
  try {
    await Deno.writeTextFile(path, "x".repeat(100));
    const small = await readBounded(path, 50);
    assertEquals(small.text.length, 50);
    assertEquals(small.truncated, true);

    const exact = await readBounded(path, 100);
    assertEquals(exact.text.length, 100);
    assertEquals(exact.truncated, false);

    const roomy = await readBounded(path, 10_000);
    assertEquals(roomy.text.length, 100);
    assertEquals(roomy.truncated, false);
  } finally {
    await Deno.remove(path).catch(() => undefined);
  }
});

Deno.test("buildPrompt embeds the subject verbatim and adds the mode-specific instruction", () => {
  const askPrompt = buildPrompt("ask", "is this safe?", "", "", "HEAD~1", "HEAD", "", "identity");
  assertStringIncludes(askPrompt, "is this safe?");
  assertStringIncludes(askPrompt, "identity");

  const planPrompt = buildPrompt("plan", "the plan", "", "", "HEAD~1", "HEAD", "", "id");
  assertStringIncludes(planPrompt, "Review this plan");
  assertStringIncludes(planPrompt, "the plan");

  const advPrompt = buildPrompt("adversarial", "the design", "", "", "HEAD~1", "HEAD", "", "id");
  assertStringIncludes(advPrompt, "adversarial review");
  assertStringIncludes(advPrompt, "strongest objections");

  const reviewPrompt = buildPrompt(
    "review",
    "main..HEAD",
    "",
    "",
    "main",
    "HEAD",
    "diff body",
    "id",
  );
  assertStringIncludes(reviewPrompt, "main..HEAD");
  assertStringIncludes(reviewPrompt, "diff body");
  assertStringIncludes(reviewPrompt, "merge verdict");
});

Deno.test("buildPrompt strips stray ``` so a markdown fence in the subject cannot escape", () => {
  const prompt = buildPrompt(
    "ask",
    "subject",
    "before ``` after",
    "",
    "HEAD~1",
    "HEAD",
    "",
    "id",
  );
  // Our fence uses backticks; a stray ``` in the subject would close it
  // early. The zero-width space we insert breaks the triple-backtick.
  assert(prompt.includes("`\u200b``"));
  // And the subject content is still present in some form.
  assertStringIncludes(prompt, "before");
  assertStringIncludes(prompt, "after");
});
