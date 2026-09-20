---
name: typescript-type-discipline
description: Use when designing or reviewing TypeScript types — handler maps, discriminated unions, branded primitives, route or event string templates, or a tsconfig posture — and the choice between `assertNever`, exhaustive `switch`, branded types, template literal types, or strict tsconfig flags will change what the compiler accepts. Maps TS expressions to corpus-owned rules; the rule itself lives at the owner. Not for vendor framework APIs, not for runtime validation (types are erased), not for syntax reference.
---

# TypeScript Type Discipline

Each pattern here is the TypeScript expression of a rule this corpus already owns.
The principle lives at the owner; this skill names the form. The type system is
erased at compile time, so none of these replace validation at trust boundaries
(network input, user input, `localStorage`, third-party APIs). The consumer repo
owns Zod, Valibot, or hand-rolled guards.

## TS expressions and their owners

| TS form | Owned by |
| --- | --- |
| `assertNever(x: never): never` in a `switch` default, paired with an exhaustive `switch` over a discriminated union with no fallthrough | `modelling-domain-invariants` § "Every state needs a named writer" |
| `type Brand<T, B extends string> = T & { readonly __brand: B }` for two primitives that share a runtime representation but represent different facts | `modelling-domain-invariants` § "One fact, one home" |
| Discriminated union with a `kind` / `type` literal field | `evolving-shared-contracts` § "Change the contract first, then the implementations" |
| `type Route<T extends string> = \`/api/${T}\`` for pattern strings the codebase produces | `evolving-shared-contracts` § "Change the contract first, then the implementations" |
| `Parameters<typeof fn>` / `ReturnType<typeof fn>` to derive a consumer shape from a single source of truth | `evolving-shared-contracts` § "Change the contract first, then the implementations" |
| Strict tsconfig flags (`strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`, `noImplicitOverride`, `noFallthroughCasesInSwitch`) | `modelling-domain-invariants` § "Put the invariant where it cannot be bypassed" |
| `incremental` + `tsBuildInfoFile` for the edit-check loop; `skipLibCheck` to stop re-checking `.d.ts` in `node_modules` | `speeding-up-rust-builds` § "Measure first" and "The crate graph" |
| `tsc --build` with project references for a monorepo, at boundaries that already own their dependency direction | `speeding-up-rust-builds` § "The crate graph"; `evolving-shared-contracts` § "Do not let storage shape the contract" |
| `isolatedModules: true` so every file parses without cross-file type information | `evolving-shared-contracts` § "Change the contract first, then the implementations" |

Read the owner for the principle. Read this skill to recognise the TS form and
choose the right one for a case at hand.

## Worked forms

The forms below illustrate each row. The principle behind each one is at the
owner named in the table; this section only shows the shape.

### `assertNever` and exhaustive switches

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

`assertNever` exists for the compile-time check, not the runtime error — its
value is making the next variant a compile error. The TS expression of "every
state has a named writer" is an exhaustive `switch` whose default makes
forgetting a variant a build failure.

`noFallthroughCasesInSwitch` is a separate concern: it catches a present variant
that forgot to `break` or `return`. The two flags are complementary, not
substitutes.

### Branded types for distinct primitives

```ts
type Brand<T, B extends string> = T & { readonly __brand: B };

type UserId  = Brand<string, "UserId">;
type OrderId = Brand<string, "OrderId">;

// sendOrder(orderId: OrderId, userId: UserId)
// sendOrder(userId, orderId) — compile error.
```

The TS expression of "one fact, one home" for primitive types. Two strings that
should not be confused become types the compiler distinguishes.

### Template literal types and discriminated unions

```ts
type PlanId    = string & { readonly __brand: "PlanId" };
type PlanRoute = `/api/plans/${PlanId}`;

function fetchPlan(id: PlanId): Promise<Plan> {
  return fetch(`/api/plans/${id}` satisfies PlanRoute);
}

// fetchPlan("/api/plans/abc") — compile error, not a PlanRoute literal
```

`Parameters<typeof fn>` covers the case where the shape is derived from a
canonical function rather than written twice. Both are TS expressions of
"change the contract first, then the implementations."

### tsconfig posture

A loose tsconfig is the type-system equivalent of a nullable column the code
assumes is required — every reader writes the check, some forget, and the
failure surfaces later. The flags that earn their place are the ones that
catch a class of bug that has actually bitten the codebase. See
`modelling-domain-invariants` for the principle; the working example is in
[`examples/tsconfig.example.jsonc`](examples/tsconfig.example.jsonc).

A flag you do not understand is a flag you will turn off. Adopting
`noUncheckedIndexedAccess` on a codebase written without it produces hundreds
of compile errors, most real, some cargo-culted-on fix-ups. Treat the adoption
as a migration, not a one-off patch.

`skipLibCheck: true` is the one exception to "be strict." It stops the
compiler from re-checking `.d.ts` in `node_modules`. The cost of leaving it
off is real compile time for a class of bug (a third-party type definition is
wrong) that almost never bites, because a wrong lib type fails visibly at the
call site. Keep it on.

### Build and type-check performance

A slow build loop destroys the discipline the type system enables — a type
error that takes thirty seconds to surface is one a developer routes around
rather than fixes. The performance flags in the consolidated tsconfig example
(`incremental`, `tsBuildInfoFile`, `skipLibCheck`, `isolatedModules`) cover
the in-package case; the monorepo case is project references at boundaries
that already own their dependency direction.

`tsc --noEmit` for type-checking, a transpiler for emit: a transpiler
(`esbuild`, `swc`, Bun) handles JavaScript output and `tsc` handles types.
A flag like `target` and `module` controls emit semantics and matters to the
transpiler; a flag like `strict` and `noUncheckedIndexedAccess` controls what
the compiler accepts and matters only to `tsc`. Set `noEmit: true` in
tsconfig so the IDE and CLI agree, and run `tsc --noEmit` in CI to make the
intent explicit.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| A `switch` whose `default` returns a fallback string | Replace with `assertNever`; the next variant becomes a compile error |
| Two primitives aliased to the same `string` / `number` and passed interchangeably | Brand each one; the next swap fails to compile |
| Route string `"/api/plans/" + id` typed as `string` | Template-literal-type the route; the concatenation now produces a typed value |
| A duplicated shape — `type UserResponse` defined in two modules, drifting apart | Derive both from one source with `Parameters<typeof fetchUser>`; see `evolving-shared-contracts` |
| `tsconfig.json` with `"strict": true` but no `noUncheckedIndexedAccess`, and `arr[i]` used without an undefined check | Add the flag; treat the resulting compile errors as a migration |
| `tsconfig.json` with every strict flag on regardless of project shape | Strip the unused flags back; each flag earns its place by catching a real class of bug |
| A strict flag commented out or set to `false` with a `// TODO fix later` next to it | The flag was adopted to catch a class of bug; turning it off to silence CI undoes the discipline. Either fix the call sites or revert the flag |
| A `tsc` invocation that takes thirty seconds on a no-op edit | `incremental` + `tsBuildInfoFile` is missing or pointing inside the source tree; fix the cache location before adding project references |
| A monorepo where `tsc` re-checks every package on every save | Project references at the boundaries that already own their dependency direction; do not split a single-package app into "core" and "feature" projects |
| A `tsconfig.base.json` imported by every package, with every package overriding the strict flags to `false` | Strict flags belong in the base; if every package overrides them, the base is a fiction and the discipline is per-package, not cross-package |

## What does not belong here

1. **Vendor-tool APIs.** A pattern that exists only because of a library (React
   prop types, Prisma generated types, tRPC inferred routes) belongs with that
   library. Install the publisher's skill; do not fork it here.
2. **Runtime validation.** `assertNever`, branded types, and template literal
   types are erased at compile time. They do not validate `JSON.parse`,
   `URLSearchParams`, or any untrusted input. The consumer repo owns Zod,
   Valibot, or hand-rolled guards.
3. **Syntax reference.** "How do I write a mapped type" is the TypeScript
   handbook's job. This skill covers only the TS expressions of corpus-owned
   rules.
4. **TS-only rules without a corpus owner.** The bridge discipline admits only
   patterns whose principle is owned elsewhere. A pattern that only makes sense
   in TypeScript belongs in a consumer repo's stack-specific skill.

## Adding or modifying a pattern

1. Identify the corpus owner. The bridge discipline admits only TS expressions
   of corpus-owned rules. If no owner exists, the pattern does not belong here.
2. Add one row to the **TS expressions and their owners** table with the
   owner's name and section.
3. Add a worked form under **Worked forms** only if the TS shape is not
   obvious from the row in the table.
4. Add a row to **Detection patterns** if there is an observable symptom that
   distinguishes the wrong form from the right one.
5. Update `provenance.json` per the maintenance ledger's discipline.
