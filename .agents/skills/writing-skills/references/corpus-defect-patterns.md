# Corpus Defect Patterns

Defects found auditing this corpus, grouped by the mechanism that produced
them. Each entry states the shape, a real instance, and the check that catches
it. Use when editing a skill, splitting one, or reviewing a corpus change.

These are maintenance defects, not authoring advice. `writing-skills` says how
to write a skill; this file says how a written skill goes wrong later.

## 1. Extraction breaks the pointer it was meant to follow

**Shape.** Content moves from `SKILL.md` into `references/`. The pointer moves
with it instead of staying behind, so a forward reference becomes a
self-reference or points into the wrong directory.

**Instance.** `brainstorming/references/visual-companion.md` was 287 lines. A
split moved the offer text into that file and left the operational half behind,
and the file ended: "read the detailed guide before proceeding:
`./references/visual-companion.md`" — itself. 267 lines were lost, and four
scripts shipped with no usage documentation for four months.

**Second instance.** `writing-skills/SKILL.md` cited
`references/testing-with-subagents.md`. That file lives in
`testing-skills/references/`. The bare path reads as relative to the citing
skill, so the reference resolved nowhere.

**Check.** After any split or move:

```sh
# a file that references its own basename
for f in $(find .agents/skills -name '*.md'); do
  grep -l "references/$(basename "$f")" "$f" 2>/dev/null
done

# cross-skill references must carry the owning skill's directory
grep -rn '`references/' .agents/skills/*/SKILL.md
```

A cross-skill path is always `skill-name/references/file.md`, never bare.

## 2. The same fact stated twice, corrected once

**Shape.** A value appears in prose and again in a checklist, table, or
template. One copy is corrected; the other is not. The stale copy is usually
the one a reader acts on, because checklists get followed and prose gets
skimmed.

**Instance.** `writing-skills` said "Max 1024 characters total" for frontmatter
in both a rule list and a checklist. The real limits are 64 for `name` and 1024
for `description`, as its own `references/anthropic-best-practices.md` states.
No skill was near either limit, so the corpus was measuring against a number
that does not exist.

**Check.** Before changing any number or limit, find every copy:

```sh
grep -rn '1024\|max.*char' .agents/skills/
```

Fix all of them in one edit, or the next reader finds the wrong one.

## 3. A rule the corpus states and then breaks

**Shape.** A skill documents a convention. Another skill — or the documenting
skill itself — violates it. Nothing detects this, because the rule is prose and
the violation is prose.

**Instances.**

| Rule | Violated by |
| --- | --- |
| `writing-skills`: "Never write `REQUIRED SUB-SKILL` or `You MUST understand X`" | `testing-skills/references/testing-with-subagents.md`: "**REQUIRED BACKGROUND:** You MUST understand test-driven-development" |
| `writing-skills`: "A skill must work when a skill it references is absent" | `executing-plans` and `subagent-driven-development` both headed their integration list "**Required** workflow skills" |
| `writing-skills` checklist: "No narrative storytelling" | Four `## Real-World Impact` sections quoting statistics from sessions nobody can re-run |

**Check.** Grep the corpus for each phrase its own rules ban:

```sh
grep -rn 'You MUST understand\|REQUIRED SUB-SKILL\|REQUIRED BACKGROUND' .agents/skills/
grep -rn '^\*\*Required' .agents/skills/*/SKILL.md
grep -rn 'Real-World Impact' .agents/skills/
```

**The deeper fix is upstream.** The `Real-World Impact` sections existed because
`writing-skills`' own section template recommended one. Removing four instances
without fixing the template would have regrown them. When the same violation
appears in several skills, look for the template, checklist, or example that
taught it.

## 4. A reference that resolves but does not transfer

**Shape.** A cross-skill citation points at a section that exists, so every
link check passes — but the section's content is specific to a domain the
citing skill does not share. The reader follows it and finds nothing they can
apply.

**Instance.** `typescript-type-discipline` routed two tsconfig rows to
`speeding-up-rust-builds` § "Measure first" and § "The crate graph". Both
sections exist. Both are cargo mechanics: `cargo build --timings`,
`target/cargo-timings/`, rlib pipelining, proc-macro crates, `error: cyclic
package dependency`. A TypeScript reader gets Rust tooling, not a portable
principle.

**Check.** A citation across domains has to name a rule, not a location. Read
the cited section and ask whether its *text* generalises — not whether the two
topics are related. Where it does not, state the rule inline and declare no
owner, or rewrite the owning section to be domain-neutral first.

**Consequence for `uses:`.** Dropping the citation means dropping the `uses:`
entry with it. A declared dependency whose target is no longer mentioned in the
body is a dead declaration.

## 5. Declared and undeclared dependencies drift apart

**Shape.** Cross-skill links exist twice: machine-readable `uses:` frontmatter,
and a prose hand-off in the body. Edits touch one.

**Instances.** `typescript-type-discipline` had no `uses:` block while handing
off to siblings by name. `subagent-driven-development/references/why-subagents.md`
had zero inbound references — 32 lines with 19 bullets not stated anywhere
else, including the cost side of the tradeoff.

**Check.**

```sh
# every uses: target must appear in the body, and vice versa
# every companion file must be referenced by something in its own skill
```

Both directions matter. A `uses:` entry with no body mention is a dead
declaration; a companion file with no inbound reference is invisible.

**Before deleting an orphan, diff it against its skill.** `why-subagents.md`
looked redundant and was not: the SKILL.md had two summary lines, the reference
had the tradeoffs. Orphaned is not the same as duplicated.

## 6. Numbers that disagree across the same skill

**Shape.** A threshold in the description and a different one in the body. The
description is the discovery surface, so a reader matching on it is then told
the skill does not apply.

**Instance.** `dispatching-parallel-agents` described itself as "Use when facing
2+ independent problems" and its body said "3+ test files failing with
different root causes". A reader with exactly two is told both.

**Check.** Extract every numeric threshold per skill and compare:

```sh
grep -oE '[0-9]+\+' .agents/skills/*/SKILL.md | sort -u
```

## Running the whole sweep

Mechanical checks catch 1, 2, 3, 5 and 6. They do not catch 4 — a citation that
resolves but does not transfer needs someone to read the cited section.

```sh
# frontmatter: name matches directory, description present and under 1024
# uses: targets exist, and each is mentioned in the body
# companion files: each referenced by something in its own skill
# no file references its own basename
# banned phrases absent
```

A clean mechanical pass is necessary and not sufficient. The defects that
mislead a reader most — a false technical claim, a citation that does not
transfer — survive every automated check in this file.
