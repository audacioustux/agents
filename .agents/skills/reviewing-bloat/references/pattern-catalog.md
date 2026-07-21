# Bloat Pattern Catalog

Use this reference after the initial review identifies a concrete symptom or during a systemic audit. Patterns describe decay shapes, not syntax. Do not report a match without evidence, consequence, and a disposition.

Opposing entries are intentional: premature DRY can coexist with missing abstraction; configuration proliferation can coexist with hardcoded policy. Resolve the tension from ownership and change pressure rather than flagging both mechanically.

## Index

Duplication · Abstraction · Coupling and ownership · Structure and consistency · Patchwork · Control flow · Data and persistence · Performance and work amplification · Concurrency and lifecycle · Types and data shape · Time, locale, money, and identity · Security · APIs and integrations · Frontend · Backend and services · Dependencies and tooling · Configuration and flags · Testing · Errors and observability · Infrastructure · Documentation and naming · Change process · Generated changes · Composite indicators

## Duplication and unnecessary volume

- **Copy-paste implementation** — near-identical logic appears in multiple files.
- **Parallel abstractions** — multiple helpers, services, or components solve the same problem differently.
- **Slightly modified clones** — copied logic differs only by a few conditions or field names.
- **Repeated validation or transformation** — the same policy, mapping, normalization, or serialization recurs across layers.
- **Repeated constants** — strings, limits, URLs, statuses, or permissions are hardcoded repeatedly.
- **Boilerplate multiplication** — large amounts of content exist only because no reusable convention was established.
- **Generated-code residue** — unused or unnecessarily verbose generated content remains committed.
- **Compatibility baggage** — obsolete compatibility logic remains after supported versions change.
- **Dead code accumulation** — unreachable functions, abandoned branches, unused components, and commented-out content remain.
- **Zombie features** — partially removed features leave models, flags, routes, jobs, or configuration behind.
- **Speculative code** — functionality is added for hypothetical future use without a current requirement.
- **Premature extensibility** — interfaces, factories, registries, and plugin systems exist for one implementation.
- **Wrapper-on-wrapper code** — layers merely forward arguments without adding policy or boundary value.
- **Trivial helper explosion** — one-line operations are scattered into many low-value functions.
- **File fragmentation or monoliths** — related logic is split across meaningless files, or unrelated responsibilities accumulate in one module.
- **Oversized functions or classes** — one unit performs querying, validation, domain logic, formatting, and side effects together.
- **Semantic duplication** — the same business rule is expressed through structurally different code and must be synchronized manually.

## Abstraction problems

- **Wrong or leaky abstraction** — unrelated cases are forced together, or callers must understand internals.
- **Abstraction inversion** — simple operations require navigating factories, adapters, interfaces, and registries.
- **Premature DRY** — superficially similar code is unified despite different responsibilities or change patterns.
- **Missing abstraction** — stable repeated behavior has no canonical shared implementation.
- **God abstraction** — one generic service, component, or utility handles unrelated domains.
- **Configuration-driven opacity** — complex behavior is hidden in nested configuration instead of explicit code.
- **Stringly typed architecture** — behavior depends heavily on arbitrary strings instead of explicit types or enums.
- **Generic parameter bags** — options, context, or metadata carry undocumented behavior.
- **Interface, factory, adapter, or mapper proliferation** — indirection and conversions add volume without a real boundary or variation.
- **Base-class dumping or mixin stacking** — behavior emerges from inheritance layers with unclear ownership or precedence.
- **Metaprogramming opacity** — reflection, proxies, or magic registration obscure where behavior originates.

## Coupling, ownership, and responsibility

- **Data clumps** — the same variables travel together across many call sites.
- **Divergent change** — one module is edited for many unrelated reasons.
- **Message chains** — long accessor chains reach through several objects' internals.
- **Change amplification / shotgun surgery** — one conceptual change requires coordinated edits across many unrelated modules or artifacts.
- **Temporal coupling** — apparently independent operations require hidden ordering or lifecycle phases.
- **Policy-mechanism entanglement** — business rules are embedded in transport, persistence, scheduling, or infrastructure mechanics.
- **Boundary bypass** — one path circumvents the canonical validation, authorization, transaction, or integration boundary.
- **Side-effect fan-out** — one local operation implicitly triggers many jobs, listeners, webhooks, cache updates, notifications, or writes.

## Missing structure and inconsistent patterns

- **Convention drift** — equivalent features use different layouts, naming, error handling, or data flows.
- **One-off architecture** — a single feature introduces its own architecture without repository-wide justification.
- **Pattern roulette** — services, repositories, commands, managers, utilities, and direct calls are used interchangeably.
- **Inconsistent layering or boundary leakage** — infrastructure details leak into unrelated layers.
- **Responsibility or ownership ambiguity** — it is unclear which module owns validation, orchestration, persistence, or workflow state.
- **No canonical path** — there are multiple ways to perform the same operation.
- **Miscellaneous dumping grounds** — utils, helpers, common, or shared become unrelated collections.
- **Cross-cutting logic scattering** — permissions, logging, retries, caching, and validation are manually repeated.
- **Implicit orchestration or hidden coupling** — order depends on imports, callbacks, signals, hooks, middleware, globals, or initialization state.
- **Accidental public APIs** — internal functions become widely imported and hard to remove.

## Patchwork and throwaway implementation

- **Ticket-specific conditionals** — branches reference one customer, incident, date, event, or request.
- **Exception stacking** — each edge case adds another if, override, exclusion, or fallback.
- **Enumerated exceptions instead of a matching convention** — named entries multiply when a stable tag, directory, type, module, or naming signal could match them.
- **Boolean patching or parameter creep** — flags and optional arguments accumulate to support unrelated cases.
- **Configuration as patching** — settings are added to avoid fixing the underlying design.
- **Environment-specific branching** — local, CI, staging, production, customer, or region behavior is scattered through core logic.
- **Magic-value branching** — specific IDs, names, strings, or numbers trigger hidden behavior.
- **Temporary code without expiry** — hacks have no owner, deadline, issue, removal criterion, or completion state.
- **Fallback, retry, delay, re-fetch, or reload accumulation** — symptoms are masked without fixing ownership, idempotency, contention, capacity, or state handling.
- **Manual synchronization or mutation chains** — duplicated state is kept aligned through scattered incremental updates.
- **Migration shadow paths** — compatibility, dual-read, dual-write, translation, or fallback logic becomes an unofficial permanent implementation.

## Control-flow bloat

- **Deep nesting or conditional explosion** — combinations of flags, states, and edge cases obscure the main path.
- **Boolean blindness** — positional booleans conceal intent.
- **State-machine-by-conditionals** — lifecycle states are represented through scattered booleans and checks.
- **Switch growth** — large switches expand whenever a new type is added.
- **Early-return abuse** — unrelated guard clauses fragment understanding.
- **Exception-driven control flow** — normal outcomes are represented as thrown and caught exceptions.
- **Nested ternaries, dense chaining, or pipeline opacity** — compressed composition hides side effects and intermediate states.
- **Distributed workflow logic** — one business process is spread across controllers, jobs, signals, hooks, and listeners.

## Data, persistence, and schema bloat

- **Over-fetching or N+1 queries** — more data or round trips are performed than the operation needs.
- **Query duplication** — equivalent queries are reimplemented across features.
- **Redundant storage or derived-data persistence** — the same fact is stored and synchronized without authoritative ownership.
- **JSON-column dumping or schema patch fields** — unvalidated blobs or one-off nullable columns avoid deliberate model design.
- **Status-field overload or boolean-column proliferation** — one field or many flags approximate several states or workflows.
- **Audit-table duplication** — history, events, revisions, and snapshots overlap without clear purpose.
- **Soft-delete everywhere** — records are retained without defined restoration or retention semantics.
- **Migration residue, dual-read, or dual-write persistence** — transitional paths remain without completion criteria.
- **Transaction sprawl or missing boundaries** — transactions cover unrelated work or related writes rely on compensating patches.
- **Unbounded history** — logs, events, revisions, or snapshots grow without retention or archival rules.

## Performance, concurrency, and lifecycle

- **Work amplification or hidden superlinear work** — one operation repeats avoidable computation, calls, serialization, or scans.
- **Unbounded fan-out or missing backpressure** — input-sized work exceeds safe concurrency or volume bounds.
- **Hot-path convenience work** — discovery, parsing, configuration, formatting, or setup repeats on latency-sensitive paths.
- **Chatty boundaries or representation churn** — many calls or conversions replace one coherent operation.
- **Cache without a contract** — ownership, keys, invalidation, expiry, consistency, or versioning differs by caller.
- **Ad-hoc locking or shared mutable state without ownership** — local patches replace a coherent concurrency model.
- **Race-condition whack-a-mole** — the same race is patched differently each time.
- **Idempotency by accident** — retries happen to work without an explicit contract.
- **Unreleased resources or listener accumulation** — close/dispose and teardown are not guaranteed.
- **Leak-patch cycles** — scheduled restarts compensate for unclear resource ownership.

## Types, time, locale, money, and identity

- **Escape-hatch typing** — dynamic types, unchecked casts, or untyped blobs bypass real invariants.
- **Type duplication across layers** — DTO, domain, view, and persistence types drift through manual mapping.
- **Optional or nullable sprawl** — null checks replace modeling required versus absent states.
- **Primitive obsession** — raw strings, numbers, or booleans stand in for domain concepts.
- **Timezone or locale drift** — equivalent formatting and conversion rules differ across features.
- **Money-as-float** — precision errors are patched wherever they appear.
- **Identifier-format inconsistency** — the same ID changes representation by code path.

## Security, APIs, and integrations

- **Authorization drift between entry points** — a resource is protected in one path but reachable through another.
- **Ad-hoc input sanitization or inconsistent trust boundaries** — validation is patched locally or applied inconsistently.
- **Security patch accretion or credential drift** — narrow fixes leave the underlying class of flaw or secret-handling variation open.
- **Endpoint or action sprawl** — minor variations create routes instead of coherent resources.
- **Version accumulation or conditional breaking patches** — old and new API behavior coexist indefinitely.
- **Payload inflation or inconsistent schemas** — endpoints duplicate unused data or disagree on naming, pagination, errors, or metadata.
- **Vendor conditionals or integration-specific domain logic** — provider details leak throughout core behavior.
- **Webhook patchwork, SDK bypassing, or protocol duplication** — verification, retries, signing, pagination, and error translation are reimplemented per integration.

## Frontend, backend, and service structure

- **Component duplication or page-specific primitives** — equivalent UI behavior is independently implemented.
- **Prop, conditional, effect, context, or global-store sprawl** — state and variants multiply without a clear owner.
- **CSS override chains, breakpoint patching, or design-token bypassing** — local styling fixes accumulate instead of improving structure.
- **Inconsistent loading, empty, error, form, or accessibility behavior** — each surface invents its own contract.
- **Fat controllers** — handlers contain business logic, persistence, integrations, and formatting.
- **Anemic services or repository ceremony** — layers merely rename underlying calls.
- **Model-hook, signal, middleware, or background-job sprawl** — important workflow behavior becomes implicit or fragmented.
- **Cron and reconciliation dependence** — periodic jobs are the primary way to restore correctness.
- **Permission duplication or role-condition sprawl** — authorization differs across routes, services, templates, and queries.

## Dependencies, tooling, configuration, and flags

- **Dependency for triviality or overlapping dependencies** — packages duplicate small or existing capabilities.
- **Abandoned, transitive, forked, or patched dependencies** — usage and ownership are unclear.
- **Build-tool or runtime overlap** — multiple tools perform the same work without boundaries.
- **Script or plugin proliferation** — similar automation is copied rather than consolidated.
- **Configuration proliferation or duplication** — equivalent settings exist across code, environment, databases, deployment, or profiles.
- **Ambiguous precedence or inconsistent defaults** — the winning configuration source is unclear.
- **Boolean, nested, negative, or permanent feature flags** — combinations create untestable or never-retired behavior.
- **Per-customer configuration hacks or environment workaround scope** — local fixes spread beyond the condition that justified them.
- **Configuration without validation or scattered secrets** — malformed values and credentials fail inconsistently.

## Testing, errors, observability, and infrastructure

- **Duplicate, implementation-coupled, mock-heavy, or snapshot-heavy tests** — tests verify wiring rather than distinct behavioral risk.
- **Fixture, factory, golden-file, or E2E compensation** — setup and expensive layers grow around untestable design.
- **Flaky retries, sleeps, skipped tests, and coverage filler** — symptoms replace deterministic synchronization and risk coverage.
- **Error swallowing or generic fallbacks** — failures become empty results or defaults without visibility.
- **Exception wrapping and log duplication** — layers add noise without actionable context.
- **Metric, alert, dashboard, or health-check proliferation** — signals multiply without decisions or owners.
- **Correlation gaps or manual diagnostic code** — behavior cannot be traced, and temporary probes remain.
- **Environment drift or deployment-script duplication** — local, CI, staging, and production disagree.
- **Infrastructure copy-paste or multiple sources of truth** — reusable boundaries are missing.
- **Resource overprovisioning, sidecar, proxy, port, or scheduled-cleanup proliferation** — operational patches conceal application or ownership problems.
- **Rollback ambiguity** — application, schema, and configuration changes cannot be reverted together.

## Documentation, naming, change process, and generated changes

- **Mechanics-based, numbered, temporary, context-dependent, or misleading names** — purpose and ownership are obscured.
- **Commented history, comment-code disagreement, or duplicated documentation** — readers cannot tell which rule is authoritative.
- **Undocumented exceptions or invariants** — correctness depends on tribal memory.
- **Additive-only changes or no simplification pass** — new artifacts accumulate without removing obsolete paths.
- **Diff-local reasoning or no pattern search** — equivalent implementations and broader ownership are ignored.
- **No migration completion criteria or lifecycle ownership** — flags, hacks, jobs, metrics, and compatibility paths become permanent.
- **Unrelated-change bundling or merge-driven mangling** — review cannot distinguish behavior from churn.
- **Works-here validation** — one environment or example stands in for representative verification.
- **Generated over-explanation, confidence padding, unrequested abstraction, full-file regeneration, or per-change style drift** — automation adds volume and inconsistency without capability.

## Strong composite indicators

- The same business operation has multiple entry points and different outcomes.
- Understanding a feature requires tracing controllers, signals, jobs, hooks, flags, and cron tasks.
- A simple change requires editing many unrelated files or artifacts.
- Removing one branch feels dangerous because nothing clearly owns the behavior.
- Temporary fixes have no owner, deadline, or removal criterion.
- New features increase concepts without replacing older ones.
- Similar code looks familiar but is never identical enough to reuse safely.
- The repository contains many ways to do things but no preferred way.
- Complexity grows faster than capability.
- Most changes are locally reasonable but globally inconsistent.
- The system stays functional through reconciliation, retries, fallbacks, and manual intervention.
- Two or more implementations exist and no one can say which is authoritative.
- Confirming behavior requires reading production logs rather than the source.
