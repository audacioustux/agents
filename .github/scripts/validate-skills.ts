#!/usr/bin/env -S deno run --allow-read --allow-net --allow-env
/**
 * Skill corpus gate.
 *
 * Checks the invariants this corpus was audited into, so an edit that breaks
 * one fails the push instead of surfacing months later inside an agent run:
 *
 *   1. frontmatter parses; `name` matches the directory
 *   2. `description` exists, is under 500 chars, and states an activation trigger
 *   3. no hard dependency on another skill being installed
 *   4. every `uses:` target resolves to a skill in this repo
 *   5. every companion file is referenced; every companion pointer resolves
 *   6. every mermaid fence parses
 *
 * Mermaid note: this validates syntax with mermaid.parse. Full render
 * validation needs a real browser (jsdom dies on CSSStyleSheet), which is why
 * that pass is run locally rather than here. Parse catches the malformed-syntax
 * class; the error-SVG class it cannot see is only reachable through render.
 */

import { JSDOM } from "npm:jsdom@25";

const SKILLS = new URL("../../.agents/skills/", import.meta.url).pathname;

type Failure = { skill: string; problem: string };
const failures: Failure[] = [];
const fail = (skill: string, problem: string) =>
  failures.push({ skill, problem });

async function walk(dir: string): Promise<string[]> {
  const out: string[] = [];
  for await (const e of Deno.readDir(dir)) {
    const p = `${dir}/${e.name}`;
    if (e.isDirectory) out.push(...await walk(p));
    else out.push(p);
  }
  return out;
}

const skills: string[] = [];
for await (const e of Deno.readDir(SKILLS)) {
  if (e.isDirectory) {
    try {
      await Deno.stat(`${SKILLS}${e.name}/SKILL.md`);
      skills.push(e.name);
    } catch {
      fail(e.name, "directory has no SKILL.md");
    }
  }
}
skills.sort();

const fences: { skill: string; line: number; code: string }[] = [];

for (const name of skills) {
  const dir = `${SKILLS}${name}`;
  const text = await Deno.readTextFile(`${dir}/SKILL.md`);

  const fm = text.match(/^---\n([\s\S]*?)\n---/)?.[1];
  if (fm === undefined) {
    fail(name, "frontmatter missing or unterminated");
    continue;
  }

  const declared = fm.match(/^name:\s*(.+)$/m)?.[1].trim();
  if (declared !== name) {
    fail(name, `frontmatter name is "${declared}", directory is "${name}"`);
  }

  const desc = fm.match(/^description:\s*(.+)$/m)?.[1]?.trim();
  if (!desc) fail(name, "no description");
  else {
    if (desc.length > 500) {
      fail(name, `description is ${desc.length} chars, limit is 500`);
    }
    if (!/\buse (this )?(when|before|after|for)\b/i.test(desc)) {
      fail(
        name,
        "description states no activation trigger (expected /use (when|before|after|for)/)",
      );
    }
  }

  // A skill must work when a skill it references is absent. writing-skills is
  // exempt: it quotes these forms as examples of what not to write.
  if (
    name !== "writing-skills" &&
    /REQUIRED SUB-SKILL|You MUST understand/.test(text)
  ) {
    fail(name, "hard dependency on another skill (use a fallback instead)");
  }

  for (const m of fm.matchAll(/^\s*- name:\s*(.+)$/gm)) {
    const target = m[1].trim();
    if (!skills.includes(target)) {
      fail(name, `uses: "${target}" is not a skill in this repo`);
    }
  }

  const files = (await walk(dir)).map((p) => p.slice(dir.length + 1));
  const corpus = await Promise.all(
    files.map((f) => Deno.readTextFile(`${dir}/${f}`)),
  );
  const joined = corpus.join("\n");
  for (const f of files) {
    if (f === "SKILL.md") continue;
    const base = f.split("/").pop()!;
    if (!joined.includes(f) && !joined.includes(base)) {
      fail(name, `companion "${f}" is never referenced`);
    }
  }

  for (const [i, f] of files.entries()) {
    if (!f.endsWith(".md")) continue;
    // Vendored upstream docs cite paths from their own examples, not ours.
    if (f === "references/anthropic-best-practices.md") continue;
    const body = corpus[i];
    for (
      const m of body.matchAll(
        /`((?:references|prompts|scripts|examples)\/[a-z0-9-]+\.[a-z]+)`/gi,
      )
    ) {
      try {
        await Deno.stat(`${dir}/${m[1]}`);
      } catch {
        fail(name, `${f} points at "${m[1]}", which does not exist`);
      }
    }
    const lines = body.split("\n");
    for (let n = 0; n < lines.length; n++) {
      if (!/^\s*```mermaid\s*$/.test(lines[n])) continue;
      const code: string[] = [];
      let j = n + 1;
      while (j < lines.length && !/^\s*```\s*$/.test(lines[j])) {
        code.push(lines[j++].replace(/^\s{0,4}/, ""));
      }
      fences.push({
        skill: `${name}/${f}`,
        line: n + 2,
        code: code.join("\n"),
      });
      n = j;
    }
  }
}

if (fences.length) {
  const dom = new JSDOM("<!DOCTYPE html><body></body>", {
    pretendToBeVisual: true,
  });
  const g = globalThis as Record<string, unknown>;
  g.window = dom.window;
  g.document = dom.window.document;
  Object.defineProperty(globalThis, "navigator", {
    value: dom.window.navigator,
    configurable: true,
  });
  g.HTMLElement = dom.window.HTMLElement;
  g.SVGElement = dom.window.SVGElement;
  g.DOMPurify = { addHook() {}, sanitize: (s: string) => s, setConfig() {} };

  // Exception to the static-import rule: mermaid captures DOMPurify and the DOM
  // globals at module-evaluation time, so it MUST load after the lines above.
  const mermaid = (await import("npm:mermaid@11")).default;
  mermaid.initialize({ startOnLoad: false, securityLevel: "loose" });

  for (const f of fences) {
    try {
      await mermaid.parse(f.code);
    } catch (e) {
      fail(
        f.skill,
        `mermaid fence at line ${f.line}: ${
          String((e as Error).message).split("\n")[0]
        }`,
      );
    }
  }

  // A validator that has never rejected anything has not earned its green.
  let rejected = 0;
  for (
    const bad of ["flowchart TD\n A[Unclosed --> B", "flowchat TD\n A --> B"]
  ) {
    try {
      await mermaid.parse(bad);
    } catch {
      rejected++;
    }
  }
  if (rejected !== 2) {
    fail(
      "(harness)",
      `mermaid validator accepted malformed input (${rejected}/2 rejected)`,
    );
  }
}

console.log(`skills: ${skills.length}  mermaid fences: ${fences.length}`);
if (failures.length) {
  console.error(`\n${failures.length} failure(s):`);
  for (const f of failures) console.error(`  ${f.skill}: ${f.problem}`);
  Deno.exit(1);
}
console.log("all checks passed");
