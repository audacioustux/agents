import "./test_config.ts";
import { assertEquals } from "@std/assert";
import {
  banned,
  classifyOutput,
  closing,
  config,
  floorWhy,
  harnessCommand,
  type Lane,
  overlap,
  parseClaudeUsage,
  parseOmp,
  quotaLeft,
  quotaRefusal,
  resetEpoch,
  waitReason,
} from "./rules.ts";
import { claudeResult, lane, longReport, usageText } from "./testing.ts";

const omp = (...events: unknown[]) => events.map((e) => JSON.stringify(e)).join("\n") + "\n";
const ompReply = (text: string, stopReason = "stop") =>
  omp(
    { type: "session", id: "omp-s" },
    {
      type: "message_end",
      message: {
        role: "assistant",
        model: "model-large",
        stopReason,
        usage: { cost: { total: 0.25 } },
        content: [{ type: "toolCall", name: "read", arguments: { path: "src/a.ts" } }, { type: "text", text }],
      },
    },
    { type: "agent_end" },
  );

/** Outputs a finished lane can leave. */
const OUTPUTS: [string, "claude2" | "omp", string | null][] = [
  ["claude report", "claude2", claudeResult(longReport)],
  ["claude one-liner", "claude2", claudeResult("Done.")],
  ["claude 120-character report", "claude2", claudeResult("x".repeat(120))],
  ["claude says it will wait", "claude2", claudeResult(longReport + "\nI'll wait for the gate to finish.")],
  [
    "claude body says still running",
    "claude2",
    claudeResult("The old job is still running.\n" + longReport + "\nAll done."),
  ],
  ["budget", "claude2", claudeResult("", { subtype: "error_max_budget_usd", is_error: true })],
  ["session limit", "claude2", claudeResult("You've hit your session limit · resets 9:20am (UTC)", { is_error: true })],
  ["error naming a rate limit", "claude2", claudeResult("API Error: rate_limit_error", { is_error: true })],
  ["plain error", "claude2", claudeResult("API Error: boom", { is_error: true, session_id: "abc" })],
  ["no json, overloaded", "claude2", "Error: 529 overloaded\n"],
  ["no json", "claude2", "segfault\n"],
  ["no file", "claude2", null],
  ["omp report", "omp", ompReply(longReport)],
  ["omp short", "omp", ompReply("ok")],
  ["omp aborted", "omp", ompReply(longReport, "aborted")],
  ["omp limit", "omp", omp({ type: "session", id: "x" }) + "Error: usage limit reached\n"],
  ["omp nothing", "omp", omp({ type: "session", id: "x" })],
];

Deno.test("classifyOutput names the state of every kind of output", () => {
  assertEquals(OUTPUTS.map(([what, harness, raw]) => `${what}: ${classifyOutput(harness, raw)}`), [
    "claude report: DONE",
    "claude one-liner: EARLY_END",
    "claude 120-character report: EARLY_END",
    "claude says it will wait: EARLY_END",
    "claude body says still running: DONE",
    "budget: BUDGET",
    "session limit: RATE_LIMIT",
    "error naming a rate limit: RATE_LIMIT",
    "plain error: ERROR",
    "no json, overloaded: RATE_LIMIT",
    "no json: ERROR",
    "no file: NO_OUTPUT",
    "omp report: DONE",
    "omp short: EARLY_END",
    "omp aborted: ERROR",
    "omp limit: RATE_LIMIT",
    "omp nothing: ERROR",
  ]);
});

Deno.test("parseOmp sums cost and reads the last action, session and models", () => {
  const o = parseOmp(ompReply(longReport));
  assertEquals([o.session, o.cost, o.models, o.calls, o.lastAction, o.ended], [
    "omp-s",
    0.25,
    ["model-large"],
    1,
    "read src/a.ts",
    true,
  ]);
});

Deno.test("closing is the last two non-empty lines", () => {
  assertEquals(closing("a\n\nb\n  \nc\n"), "b\nc");
});

Deno.test("resetEpoch: same day, next day, noon and midnight", () => {
  const at = (iso: string) => Date.parse(iso) / 1000;
  const reset = (text: string, after: string) => resetEpoch(text, at(after));
  assertEquals(reset("resets 9:20am (UTC)", "2030-01-02T08:00:00Z"), at("2030-01-02T09:20:00Z"));
  assertEquals(reset("resets 9:20am (UTC)", "2030-01-02T10:00:00Z"), at("2030-01-03T09:20:00Z"));
  assertEquals(reset("resets 12pm (UTC)", "2030-01-02T08:00:00Z"), at("2030-01-02T12:00:00Z"));
  assertEquals(reset("resets 12am (UTC)", "2030-01-02T08:00:00Z"), at("2030-01-03T00:00:00Z"));
  assertEquals(reset("limit · resets 11:05 PM (UTC)", "2030-01-02T23:30:00Z"), at("2030-01-03T23:05:00Z"));
  assertEquals(reset("no time here", "2030-01-02T08:00:00Z"), null);
});

Deno.test("overlap: literal, wildcard and ** scopes", () => {
  const pairs: [string[], string[], boolean][] = [
    [["src/**"], ["src/a.ts"], true],
    [["src/a/*"], ["src/b/*"], false],
    [["a/*.ts"], ["a/x*"], true],
    [["apps/web/**"], ["apps/api/**"], false],
    [["apps/web/src/x.ts"], ["apps/web/**"], true],
    [["apps/web/src/x.ts"], ["apps/web/src/y.ts"], false],
    [["docs/[ab]*.md"], ["docs/c.md", "docs/a1.md"], true],
    [["docs/[!a]*.md"], ["docs/a1.md"], false],
    [["packages"], ["packages/db/**"], false],
    [["/lead/**/"], ["lead/x"], true],
  ];
  assertEquals(pairs.map(([a, b]) => Boolean(overlap(a, b))), pairs.map(([, , expected]) => expected));
});

Deno.test("a banned model is refused with its reason; other models pass", () => {
  assertEquals(banned("provider-a/model-retired"), "retired: use provider-a/model-large");
  assertEquals([banned("provider-a/model-large"), banned("sonnet")], [null, null]);
});

const USAGE = `Provider A — <account>
  ● 5 hours  ████░░░░  85.5% used
  ● 7 days   ██░░░░░░  40% used
Provider B — <account>
  ● 5 hours  ██░░░░░░  60% used
`;

Deno.test("quotaLeft reads the percent left per window; quotaRefusal refuses at the floor and when unreadable", () => {
  assertEquals(quotaLeft(USAGE, "Provider A"), { "5 hours": 14.5, "7 days": 60 });
  assertEquals(quotaLeft(USAGE, "Provider B"), { "5 hours": 40 });
  assertEquals(quotaLeft(USAGE, "Nothing"), {});
  assertEquals(quotaRefusal("provider-a/model-large", USAGE)?.includes("5 hours 14.5%"), true);
  assertEquals(quotaRefusal("provider-b/model-small", USAGE), null);
  assertEquals(quotaRefusal("provider-a/model-large", "")?.startsWith("cannot read Provider A quota"), true);
  assertEquals(quotaRefusal("sonnet", ""), null);
});

const base = { cwd: "/w", prompt: "/p", effort: "high", budget: 200, attempts: 0, session: "sess-1" };
const argvOf = (l: Partial<Lane> & { name: string }, resume = false) =>
  harnessCommand(lane({ ...base, harness: "claude4", ...l }), "BRIEF", resume, "/root");
const PREFIX = ["nice", "-n", "10", "timeout", "14400"];

Deno.test("a claude attempt: tools, budget, model, effort and the session id it can be resumed by", () => {
  const { argv, env } = argvOf({ name: "a", tools: "full", model: "opus", chrome: true });
  assertEquals(argv, [
    ...PREFIX,
    ...["claude", "-p", "BRIEF", "--max-budget-usd", "200", "--output-format", "json"],
    ...["--permission-mode", "acceptEdits", "--allowedTools"],
    ...["Bash", "Read", "Edit", "Write", "Glob", "Grep", "WebFetch", "mcp__claude-in-chrome", "--chrome"],
    ...["--model", "opus", "--effort", "high", "--session-id", "sess-1"],
  ]);
  assertEquals(env.CLAUDE_CONFIG_DIR?.endsWith("/.claude.account4"), true);
});

Deno.test("a claude resume names the session and repeats no model, effort or session id", () => {
  const { argv } = argvOf({ name: "a", tools: "ro", budget: 8.5 }, true);
  assertEquals(argv.slice(5), [
    ...["claude", "-p", "BRIEF", "--max-budget-usd", "8.5", "--output-format", "json"],
    ...["--permission-mode", "acceptEdits", "--allowedTools", "Read", "Glob", "Grep", "WebFetch"],
    ...["--resume", "sess-1"],
  ]);
});

Deno.test("an omp attempt: thinking level, time limit, its own session directory, and no bash", () => {
  const edit = argvOf({ name: "o", harness: "omp", tools: "edit", model: "provider-a/model-large" });
  assertEquals(edit.argv.slice(5), [
    ...["omp", "-p", "BRIEF", "--model", "provider-a/model-large", "--thinking", "high", "--mode", "json"],
    ...["--no-title", "--max-time", "45m", "--session-dir", "/root/omp-sessions/o"],
    ...["--tools", "read,grep,glob,edit,write", "--auto-approve"],
  ]);
  assertEquals(edit.env, {});
  const none = argvOf({ name: "n", harness: "omp", tools: "none" }, true);
  assertEquals(none.argv.slice(-3), ["--no-tools", "--resume", "sess-1"]);
});

Deno.test("waitReason: account limit, model cap, harness cap, file scope, and --force", () => {
  const now = 1_000_000;
  const running = (n: number, over: Partial<Lane> = {}) =>
    Array.from({ length: n }, (_, i) => lane({ name: `r${i}`, harness: "claude4", status: "RUNNING", ...over }));
  const req = { name: "new", harness: "claude4" as const, model: "sonnet", globs: ["apps/web/**"], force: false };
  assertEquals(waitReason(req, running(4), [], now), null);
  assertEquals(waitReason(req, running(5), [], now), "REFUSED: claude4 already runs 5 lanes (cap 5); wait or --force");
  assertEquals(waitReason({ ...req, force: true }, running(9), [now + 60], now), null);
  assertEquals(waitReason(req, [], [now + 60], now)?.includes("at its limit until"), true);
  assertEquals(waitReason(req, [], [now - 60], now), null);
  const capped = { ...req, harness: "omp" as const, model: "provider-a/model-large" };
  assertEquals(
    waitReason(capped, running(2, { harness: "omp", model: capped.model }), [], now)?.includes("cap 2"),
    true,
  );
  const web = running(1, { globs: ["apps/web/src/**"] });
  assertEquals(waitReason(req, web, [], now)?.includes("overlap running lane r0"), true);
  assertEquals(waitReason(req, running(1, { globs: ["apps/web/src/**"], readonly: true }), [], now), null);
});

Deno.test("parseClaudeUsage reads the session and the all-models week; anything else is unreadable", () => {
  assertEquals(parseClaudeUsage(usageText(11, 96)), {
    session: { used: 11, resets: "Jan 2, 11:29am (UTC)" },
    week: { used: 96, resets: "Jan 3, 9:59am (UTC)" },
  });
  assertEquals(parseClaudeUsage("Current session: 0% used\nCurrent week (all models): 0% used\n")?.week, {
    used: 0,
    resets: "",
  });
  assertEquals(parseClaudeUsage("Current session: 3% used · resets Jan 2, 1:29pm (UTC)\n"), null, "no week");
  assertEquals(parseClaudeUsage("Total cost:            $0.0000\nTotal duration (wall): 2s\n"), null, "an API account");
  assertEquals(parseClaudeUsage(""), null);
});

Deno.test("floorWhy stops an account at the weekly or the 5-hour floor, and names the reset", () => {
  const { weeklyMax, sessionMax } = config.claudeQuota;
  const at = (session: number, week: number) => floorWhy("claude2", parseClaudeUsage(usageText(session, week)));
  assertEquals(at(sessionMax - 1, weeklyMax - 1), null);
  assertEquals(at(0, weeklyMax)?.includes(`week ${weeklyMax}% used`), true, "the floor itself stops");
  assertEquals(at(0, 96)?.endsWith("resets Jan 3, 9:59am (UTC)"), true);
  assertEquals(at(sessionMax, 0)?.includes("5-hour session"), true);
  assertEquals(at(sessionMax, 96)?.split("; ").length, 2, "both are named");
  assertEquals(floorWhy("claude2", null), null, "unreadable is not gated");
});
