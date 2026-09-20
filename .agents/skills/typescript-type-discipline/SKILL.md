---
name: typescript-type-discipline
description: Use when designing or reviewing TypeScript types — handler maps, discriminated unions, branded primitives, route or event string templates — and the choice between `satisfies`, `as`, exhaustive `switch`, or `assertNever` will change what the compiler accepts. Also use when choosing tsconfig strictness flags or deciding whether the codebase needs project references. Maps TS expressions to cross-stack rules. Not for vendor framework APIs, not for runtime validation (types are erased), not for syntax reference.
uses:
  - name: designing-component-systems
    source: audacioustux/agents
  - name: modelling-domain-invariants
    source: audacioustux/agents
  - name: evolving-shared-contracts
    source: audacioustux/agents
  - name: designing-idempotent-boundaries
    source: audacioustux/agents
  - name: speeding-up-rust-builds
    source: audacioustux/agents
  - name: hardening-rust-supply-chain
    source: audacioustux/agents
---

# TypeScript Type Discipline

Every rule in this skill is the **TypeScript expression of a cross-stack rule**
the corpus already owns. The rule itself lives in the named owner; the TS form
lives here. If a TS pattern you want to add has no cross-stack owner, **it does
not belong in this skill** — it belongs at the cross-cutting layer or it does
not belong at all. See "What does not belong here" below.

The type system is fully erased at compile time. None of these patterns have a
runtime cost, and none replace validation at trust boundaries (network input,
user input, `localStorage`, third-party APIs). For runtime validation, the
consumer repo owns the choice between Zod, Valibot, or hand-rolled guards.

## TS expressions and their owners

| TS form | What it expresses | Owned by |
| --- | --- | --- |
| `satisfies Record<string, Handler>` over a literal-keyed map | Preserve concrete keys for autocomplete and exhaustiveness while requiring the declared shape | `designing-component-systems` |
| A trailing `satisfies` clause with no annotation | Force the declared shape but keep the inferred literal types | `designing-component-systems` |
| `type Brand<T, B extends string> = T & { readonly __brand: B }` | Distinct primitives carry distinct types | `modelling-domain-invariants` |
| Brand constructor that validates raw input and casts in one place | The cast lives where the trust boundary is, nowhere else | `modelling-domain-invariants`, `designing-idempotent-boundaries` |
| `assertNever(x: never): never` in a `switch` default | Unreachable states are bugs; the compiler proves it | `modelling-domain-invariants` |
| Exhaustive `switch` over a discriminated union with no fallthrough | Every state has a named writer; readers cannot silently miss a variant | `modelling-domain-invariants` |
| Discriminated union with a `kind` / `type` literal field | A shape two parties agree on; changes go through the contract, not the storage | `evolving-shared-contracts` |
| `type Route<T extends string> = \`/api/${T}\`` | Pattern strings the codebase produces live in the type system | `evolving-shared-contracts` |
| `Parameters<typeof fn>` / `ReturnType<typeof fn>` to derive shapes from a source of truth | One canonical artifact, not several descriptions that drift | `evolving-shared-contracts` |
| Strict tsconfig flags (`strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`, `noImplicitOverride`, `noFallthroughCasesInSwitch`) | Invariants live where they cannot be bypassed; the type-checker is the schema | `modelling-domain-invariants` |
| `incremental` + `tsBuildInfoFile` for the edit-check loop; `skipLibCheck` to stop re-checking `.d.ts` in `node_modules` | Compile-time discipline; a slow type-check destroys the discipline it enables | `speeding-up-rust-builds` |
| `tsc --build` with project references for a monorepo | Split the dependency graph along boundaries that already own their direction | `speeding-up-rust-builds`, `evolving-shared-contracts` |
| `isolatedModules: true` | Every file must be parseable without cross-file type information | `evolving-shared-contracts` |

Read the **Owned by** column to learn the principle. Read the TS form here to
recognise it when you see it and to choose the right expression for a given
case. Do not read this skill in place of the owner.

## The patterns, in detail

### `satisfies` for shape-checked, narrowly-typed values

The rule: when a value's literal shape matters (handler keys, event names,
enum-like records), the **type annotation** widens it to the declared form and
the literal types are lost. The **trailing `satisfies`** forces the declared
shape while keeping the literal types.

```ts
// WIDENED — handlers is Record<string, Handler>, keys are string
const handlers: Record<string, Handler> = {
  login: handleLogin,
  logout: handleLogout,
};

// PRESERVED — keys remain the literals "login" | "logout"
const handlers = {
  login: handleLogin,
  logout: handleLogout,
} satisfies Record<string, Handler>;
```

`designing-component-systems` owns the principle: the public shape of a thing
is the thing you actually want consumers to see, not a wider form that happens
to type-check. `satisfies` is one TS expression of that rule. Other languages
have analogues (Rust enum variants, Haskell GADTs), but the principle is
universal.

**Do not combine a type annotation with `satisfies` on the same declaration.**
The annotation wins; the value still gets widened. Use the trailing `satisfies`
alone, or the annotation alone.

### `assertNever` and exhaustive switches

The rule: a state machine whose states are enumerated in code is silently
incomplete when a new variant is added and the switch falls through. The
default branch should make adding an unhandled variant a compile error.

```ts
type RequestState =
  | { kind: "pending" }
  | { kind: "fulfilled"; data: string }
  | { kind: "rejected"; error: Error };

function describe(state: RequestState): string {
  switch (state.kind) {
    case "pending":   return "...";
    case "fulfilled": return state.data;
    case "rejected":  return state.error.message;
    default:          return assertNever(state);
  }
}

function assertNever(x: never): never {
  throw new Error(`Unexpected variant: ${JSON.stringify(x)}`);
}
```

`modelling-domain-invariants` owns the rule: every state in a lifecycle has a
named writer, and a state nothing writes silently removes every path that
runs through it. The TS expression is `assertNever`. The Rust expression is an
exhaustive `match` with `_ => unreachable!()` or a never-typed branch. The
principle does not depend on the language.

**Pair with `@typescript-eslint/switch-exhaustiveness-check`** so the
exhaustiveness failure shows up in the editor, not only at the next build.
`assertNever` throws only if reached, which it should not be — its value is
the compile-time check, not the runtime error.

### Branded types for distinct primitives

The rule: a `UserId` and an `OrderId` are both `string` at runtime but
semantically different facts. A function that takes the wrong one should not
compile.

```ts
type Brand<T, B extends string> = T & { readonly __brand: B };

type UserId  = Brand<string, "UserId">;
type OrderId = Brand<string, "OrderId">;

// Cast lives in one place — the trust-boundary constructor.
function toUserId(raw: string): UserId {
  if (!/^u_[a-z0-9]{16}$/.test(raw)) {
    throw new Error(`invalid UserId: ${raw}`);
  }
  return raw as UserId;
}

// Now: sendOrder(orderId: OrderId, userId: UserId)
// sendOrder(userId, orderId) is a compile error.
```

`modelling-domain-invariants` owns the rule: a fact that is distinct should
be a distinct type, and the cast that crosses the trust boundary lives in
exactly one place. Branded types are the TS expression. Rust has nominal
types natively; Haskell has `newtype`. The pattern is universal but the
syntax is per-language.

**The cast lives in exactly one place.** Anywhere else, `as UserId` is a
lie. The constructor is the trust boundary — it validates raw input and
returns the branded type. Code downstream of the boundary cannot mint a
`UserId` from arbitrary strings.

### Template literal types for owned strings

The rule: a string the codebase produces and controls (an API route, a CSS
custom property, an event name, an i18n key) should follow a pattern the
compiler can check. A route that does not match the route template should
not compile.

```ts
type PlanId = string & { readonly __brand: "PlanId" };
type PlanRoute = `/api/plans/${PlanId}`;

function fetchPlan(id: PlanId): Promise<Plan> {
  return fetch(`/api/plans/${id}` satisfies PlanRoute);
}

// fetchPlan("/api/plans/abc")          — compile error, not a PlanRoute literal
// fetchPlan("/api/plans/" as string)   — works at runtime, lies to the compiler
```

`evolving-shared-contracts` owns the rule: there is one canonical artifact
both sides derive from, and the agreement precedes the code. Template
literal types are the TS expression. The pattern fails at any boundary
where untrusted strings enter — at that point, runtime validation owns the
check, not the type system.

**Recursive parameter extraction is possible.** A conditional type written
against a route shape can pull typed parameter names out of a literal:

```ts
// Not a built-in — this is what the conditional type looks like:
type ExtractParams<T> =
  T extends `${string}:${infer P}/${infer R}` ? P | ExtractParams<R>
  : T extends `${string}:${infer P}` ? P
  : never;

// ExtractParams<"/users/:userId/posts/:postId">
//   yields "userId" | "postId"
```

It is worth using when the consumer side is typed, but it does not replace
a real router — it expresses what the router should produce, so a hand-typed
route string fails to compile against the extracted parameter list.

### tsconfig posture — strictness as a schema

The rule: invariants belong where they cannot be bypassed. A `tsconfig.json`
is the type-checker's schema for the codebase; the flags in it decide which
classes of bug the compiler will reject, and which the application code
must catch by hand. A loose tsconfig is the type-system equivalent of a
nullable column that the code assumes is required — every reader writes
the check, some forget, and the failure surfaces later.

The discipline is not "turn every flag on". The flags that earn their
place are the ones that catch a class of bug that has actually bitten the
codebase, or that turn a runtime crash into a compile error. The
starting posture is the floor; add flags one at a time as the absence
hurts, not because a blog post said so.

```jsonc
// tsconfig.json — one file. The flags below are split into three
// groups (strictness / build / emit) for the reader, not because they
// live in different files. Comments name the failure each catches.
{
  "compilerOptions": {
    // ---------- strictness ----------
    // These decide which classes of bug the compiler rejects. Each
    // earns its place by catching a real class of bug.
    "strict": true,
    // strict enables: noImplicitAny, strictNullChecks, strictFunctionTypes,
    // strictBindCallApply, strictPropertyInitialization, alwaysStrict,
    // useUnknownInCatchVariables, noImplicitThis, noImplicitReturns.

    "noUncheckedIndexedAccess": true,
    // arr[i] becomes T | undefined; record[k] becomes T | undefined.
    // Forces every reader to handle the missing case. The single most
    // underrated flag — turns array-out-of-bounds from a runtime crash
    // into a compile error.

    "exactOptionalPropertyTypes": true,
    // { x?: number } no longer accepts { x: undefined }. Distinguishes
    // "the property is missing" from "the property is present and undefined",
    // which is a real distinction that the default config erases.

    "noImplicitOverride": true,
    // class B extends A { override foo() {} } — without the keyword,
    // the override silently disappears when A's method is renamed.

    "noFallthroughCasesInSwitch": true,
    // Catches fallthrough between non-empty `case` arms. Distinct
    // from `assertNever` (which catches missing variants): this flag
    // catches a present variant that forgot to `break` or `return`.
    // Both have a place.

    "noPropertyAccessFromIndexSignature": true,
    // Forces explicit `obj["key"]` for index-signature access. Catches
    // typos on record keys at compile time.

    "forceConsistentCasingInFileNames": true,
    // Cross-platform safety; rejects `import './Foo'` against `foo.ts`.

    // TypeScript recommends but does not require:
    // "noUnusedLocals": true,
    // "noUnusedParameters": true,
    // These catch dead code but can fight generated code and tests.
    // Enable per-package once the friction is judged lower than the bug cost.

    // ---------- build / type-check performance ----------
    // These decide how long the type-check takes. See the next section.
    "incremental": true,
    "tsBuildInfoFile": "./node_modules/.cache/tsbuildinfo",
    // `incremental` makes tsc emit a build info file and skip checking
    // files whose inputs have not changed. `tsBuildInfoFile` keeps it
    // out of the source tree.

    "skipLibCheck": true,
    // Stops the compiler from re-checking `.d.ts` in `node_modules` and
    // bundled libs. See the section on the strictness/build trade-off.

    "isolatedModules": true,
    // Required for any code that goes through a transpiler (swc,
    // esbuild, Bun, ts-jest). Forces every file to be parseable
    // without cross-file type information. Catches a class of "this
    // works in tsc but breaks in the bundler" bugs at compile time.

    // ---------- emit ----------
    // For codebases where a transpiler (esbuild / swc / Bun) emits
    // JavaScript and tsc only type-checks. See the section below.
    "noEmit": true,
    // Skip JavaScript output. `noEmit` (config flag) and `--noEmit`
    // (CLI flag) are two different surfaces — set this in tsconfig so
    // the IDE and CLI agree, and run `tsc --noEmit` in CI scripts to
    // make the intent explicit.
  }
}
```

`modelling-domain-invariants` owns the rule: the invariant must live where
it cannot be bypassed. A strict tsconfig is the type-level equivalent of
a CHECK constraint — every writer goes through it (every `.ts` file the
compiler reads), and the failure mode is the same (the build rejects
the bad value before it lands).

**A flag you do not understand is a flag you will turn off.** Adding
`noUncheckedIndexedAccess` to a codebase that has been written without it
produces hundreds of compile errors, most of them real, some of them
cargo-culted-on fix-ups. Treat the adoption as a migration: land the
flag, fix what is real, route the rest through `as` casts with a comment
saying why the cast is safe, and remove the casts as the surrounding code
is rewritten. The discipline is the posture, not the flag count.

**`skipLibCheck: true` is the one exception to "be strict".** It stops
the compiler from checking `.d.ts` files in `node_modules` and bundled
libs. The cost of leaving it off is real compile time (a complex React
type tree can add seconds per build) for a class of bug (a third-party
type definition is wrong) that almost never bites you, because a wrong
lib type fails in a way that is immediately visible at the call site.
Keep it on.

### Build and type-check performance

The rule: a slow build loop destroys the discipline the type system
enables. A type error that takes thirty seconds to surface is one a
developer will route around rather than fix. Compile-time discipline
is cross-stack (`speeding-up-rust-builds` owns the principle for Rust;
`hardening-rust-supply-chain` owns the supply-chain half); the TS
expressions are below.

The four performance flags are listed in the consolidated tsconfig example
in the posture section above: `incremental`, `tsBuildInfoFile`, `skipLibCheck`,
and `isolatedModules`. Each comment in that example names the failure it
catches or the cost it avoids. Do not duplicate them here.

For monorepos and large codebases, the next step is **project
references**:

```
packages/
  shared/      tsconfig.json  (composite: true, declaration: true)
  app/         tsconfig.json  (references: [{ "path": "../shared" }])
  worker/      tsconfig.json  (references: [{ "path": "../shared" }])
```

`tsc --build` walks the graph and type-checks each project against its
references rather than re-checking the world. The boundaries that earn
a project reference are:

- A package with its own `package.json` and its own publish/install story.
- A boundary that already owns its dependency direction (no cycles).
- A boundary where the build output is consumed by something else
  (a downstream package, a worker, a published library).

A boundary that does not own those three is a candidate for a folder,
not a project. Splitting a 5,000-line app into "core" and "feature"
projects before the dependency graph justifies it adds build-graph
boilerplate without saving any time, because the editor and bundler
still load both projects to resolve a single import.

**The discipline is measurement first.** When the type-check becomes
the slow step in the loop, do not guess where the time goes. Run:

```bash
# Time a no-op type-check (everything cached) — this is the floor.
time npx tsc --noEmit

# Time a full type-check with the cache cleared.
rm -rf node_modules/.cache/tsbuildinfo && time npx tsc --noEmit

# Trace which files the slow project re-checks.
npx tsc --build --verbose --dry
```

A floor under five seconds and a clean build under thirty is a
reasonable target for a medium codebase. If the clean build is
minutes, the answer is almost always project references, not a
tsconfig flag.

**`tsc --noEmit` for type-checking, a transpiler for emit.** Most
TypeScript codebases in 2026 do not run `tsc` to emit JavaScript; they
run `tsc --noEmit` for type-checking and `esbuild` / `swc` / Bun for
transformation. The two halves serve different purposes and should
not be confused. The tsconfig flag is `noEmit: true` (see the
posture section); the CLI form `tsc --noEmit` overrides the config
flag for a single invocation and is the right one for CI scripts.
A flag like `target` and `module` controls emit semantics and
matters to the transpiler; a flag like `strict` and
`noUncheckedIndexedAccess` controls what the compiler accepts and
matters only to `tsc`.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `const handlers: Record<string, Handler> = { login: ..., logout: ... }` — autocomplete shows every string | Move the annotation to a trailing `satisfies`; keep the literal keys |
| A `switch` whose `default` returns a fallback string | Replace with `assertNever`; the next variant becomes a compile error |
| `as UserId` in more than one place, or `as UserId` in code that does not own validation | The cast belongs in one constructor at the trust boundary; remove the rest |
| A type alias whose right-hand side is `string` and whose name suggests a distinct fact | Brand it; the next caller that confuses it with another string fails to compile |
| Route string `"/api/plans/" + id` typed as `string` | Template-literal-type the route; the concatenation now produces a typed value |
| A duplicated shape — `type UserResponse` defined in two modules, drifting apart | Derive both from one source with `Parameters<typeof fetchUser>` or similar; see `evolving-shared-contracts` |
| `tsconfig.json` with `"strict": true` but no `noUncheckedIndexedAccess`, and the codebase uses `arr[i]` without an undefined check | Add the flag; treat the resulting compile errors as a migration, not a one-off fix-up |
| A `tsconfig.json` that turns on every strict flag regardless of project shape | Strip the unused flags back; each flag earns its place by catching a real class of bug in this codebase |
| A `tsc` invocation that takes thirty seconds on a no-op edit | `incremental` + `tsBuildInfoFile` is missing or pointing inside the source tree; fix the cache location before adding project references |
| A monorepo where `tsc` re-checks every package on every save | Project references at the boundaries that already own their dependency direction; do not split a 5,000-line app into "core" and "feature" projects |
| A codebase that uses `esbuild`/`swc` for emit but `tsc` for types, and still has flags like `target` and `module` adjusted as if `tsc` were emitting | Type-checking flags (`strict`, `noUncheckedIndexedAccess`) and emit flags (`target`, `module`) are different concerns; pick the one the tool actually consumes |
| A strict flag (e.g. `noUncheckedIndexedAccess`) commented out or set to `false` with a `// TODO fix later` next to it | The flag was adopted to catch a class of bug; turning it off to silence CI undoes the discipline. Either fix the call sites or revert the flag |
| Project references added to a single-package repo to "speed up the build" | Project references split a graph that already has clean boundaries; a single-package repo does not have one. Measure first, add the flag or reference only where measurement shows the time goes |
| A `tsconfig.base.json` (or `tsconfig.common.json`) imported by every package without any package overriding the strict flags | Strict flags belong in the base; if every package overrides them to `false`, the base is a fiction and the discipline is per-package, not cross-package |

## What does not belong here

1. **Vendor-tool APIs.** A pattern that exists only because of a library
   (React prop types, Prisma generated types, tRPC inferred routes) belongs
   with that library. Install the publisher's skill; do not fork it here.
2. **Runtime validation.** `satisfies`, `assertNever`, and template literal
   types are erased at compile time. They do not validate `JSON.parse`,
   `URLSearchParams`, or any untrusted input. The consumer repo owns Zod,
   Valibot, or hand-rolled guards; this skill only names the type-level
   expression of an already-validated value.
3. **Syntax reference.** "How do I write a mapped type" is the TypeScript
   handbook's job. This skill only covers patterns that express a
   cross-stack rule already owned elsewhere.
4. **TS-only rules without a cross-stack owner.** If you want to add a
   pattern here, first prove the principle has a cross-stack form (or that
   it should — and add the cross-stack rule instead). A pattern that only
   makes sense in TypeScript belongs in a consumer repo's stack-specific
   skill, not here.
5. **Performance or compile-time tricks that exist for their own sake.**
   `const`-narrowing tricks and conditional-type recursion can produce
   types that are impressive and unmaintainable. The discipline is: did
   the cross-stack rule demand it? If not, write the boring form.

## Adding or modifying a pattern

1. Identify the cross-stack owner. If you cannot name one, the pattern does
   not belong in this skill — see "What does not belong here" § 4.
2. Read the owner. Confirm the TS form is a faithful expression of the
   rule, not a workaround that papers over a TS limitation the rule itself
   would never recommend.
3. Add one row to the **TS expressions and their owners** table.
4. Add a section under **The patterns, in detail** if the pattern needs
   worked code. Skip it if the TS form is obvious to a reader who has read
   the owner.
5. Add a row to **Detection patterns** if there is an observable symptom
   that distinguishes the wrong form from the right one.
6. Update `provenance.json` per the maintenance ledger's discipline.

## Review checklist

- For every TS pattern in the codebase: which cross-stack rule does it
  express, and where is that rule owned?
- For every `as` cast: is it inside the single constructor that validates
  raw input, or is it papering over a type error elsewhere?
- For every `switch` over a union: does the `default` make adding a
  variant a compile error, or does it silently fall through?
- For every branded primitive: is the cast in exactly one place?
- For every shared type: does one canonical artifact exist that both sides
  derive from, or are there several descriptions that can drift? (See the
  rule in `evolving-shared-contracts`.)
- For the tsconfig: is the strictness posture earned by what the codebase
  has actually bitten on, or is it copied from a blog post?
- For a slow type-check: did you measure where the time goes before
  reaching for project references or a new flag?
- For every added pattern here: did the cross-stack owner get cited, or
  did a TS-only rule sneak in?
