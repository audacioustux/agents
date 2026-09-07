---
name: writing-async-rust
description: Use when writing or reviewing async Rust — deciding what happens when a future is dropped mid-await, holding state or locks across await points, choosing between concurrency and parallelism, or diagnosing a task that stalls the runtime. Separates language-level guarantees from runtime-specific behaviour that differs between tokio, async-std, and smol.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
---

# Writing Async Rust

## Overview

An async function is a state machine that can be abandoned between any two await
points. Most async defects come from code written as though it cannot be.

The compiler enforces almost none of this. It will not tell you that dropping a future
loses data, that a lock is held across an await, or that a blocking call has stalled
every other task on the thread.

Two layers are in play throughout, and confusing them is its own bug source. Futures
are lazy and cancel-by-drop at the language level, true under any executor. Task
handles, `select!`, and the offload APIs belong to a runtime, and the runtimes
disagree — sometimes in opposite directions. Claims below are marked where it matters.
Runtime specifics are as of tokio 1.53, async-std 1.13, smol 2.0.

## When to use

Use for:

- async functions, and review of them
- anything that races, times out, or selects between futures
- state held across an await point
- a runtime that stalls, or a task that never completes

Ownership, error, and dispatch questions belong to `writing-idiomatic-rust`.

## Cancellation is the default, not the exception

A future does nothing until polled — std states futures "are inert; they must be
actively `poll`ed" — and stops existing when dropped. Cancellation in Rust is that
drop: a timeout, a race, a losing select arm, and an aborted task all cancel by
dropping the future mid-execution. This part is language-level and holds under any
executor. The one caveat is that a leaked future is never dropped, so `mem::forget`
and reference cycles defeat drop-based cleanup.

So the question for any await point is: if the future is dropped here, what has
already happened and what never will? Everything before the await has run. Everything
after it may never run. Cleanup written after an await is cleanup that is not
guaranteed.

"Cancellation safety" is tokio's term, defined on its `select!` documentation; std and
futures-rs do not use it. The idea generalises: an operation is cancel-safe when
dropping it mid-flight leaves no observable trace — either it completed or it did not
happen. The concrete per-API verdicts are tokio's own, about tokio's APIs, and they do
not transfer to another runtime's I/O traits by assumption.

The shape to internalise is where the partial result lives. Under tokio,
`AsyncReadExt::read` is documented cancel safe because a cancelled read guarantees no
data was read; `read_exact` and `read_to_end` are not, because bytes already consumed
from the socket live in the future and die with it. That is data loss with no error
and no panic.

This matters most in a `select!`, whose losing branches are dropped when the macro
returns — both `tokio::select!` and `futures::select!` do this, though they differ in
detail. A select in a loop over a non-cancel-safe operation silently discards work on
every pass. Where an operation is not cancel-safe, hold it outside the loop so it is
polled to completion rather than recreated and dropped.

Where cleanup must happen regardless, it belongs in a guard whose `Drop` runs on the
abandoned path, not in code after the await.

## State held across an await

Anything alive across an await point lives inside the generated state machine, with
two consequences.

A future holding a non-`Send` value across an await is itself non-`Send`. This is
compiler-level: auto traits are derived over the generated state machine, whatever
executor eventually polls it. What is runtime-specific is only where you find out —
the error surfaces at a spawn site that requires `Send`, naming a type the author may
not have realised was held. Under a single-threaded spawn there is no `Send` bound, so
the same code compiles and runs correctly. Non-`Send`ness is a portability constraint,
not a bug in itself.

A blocking lock guard held across an await is the actual hazard, and it is worst
precisely where the `Send` error does not fire. The guard keeps the lock for the whole
suspension, so on a single-threaded runtime a second task waiting on that lock blocks
the only thread. Nothing else can run — including the timer driving a timeout meant to
bound it, which is why this failure presents as a total hang rather than an error.
Take what the lock protects, release it, then await.

Do not reach for an async mutex reflexively. Tokio's own documentation is blunt about
this: "Contrary to popular belief, it is ok and often preferred to use the ordinary
Mutex from the standard library in asynchronous code." A blocking mutex around a short
critical section that never spans an await is the simpler and usually faster choice.
The async-aware lock earns its cost only when the guard genuinely must be held across
an await — and that is a design decision worth stating rather than defaulting into.

Large values held across awaits inflate the state machine, which is allocated as a
whole. A future carrying a large buffer across several awaits pays for it at every
suspension point.

## Blocking

Rust has no preemptive async scheduler — tokio's own docs note that "since Rust does
not have a runtime, it is difficult to forcibly preempt a long-running task". Tasks
run until they yield at an await, so a blocking call stalls its thread and every other
task scheduled on that thread. The blast radius is the thread, not the process: on a
multi-threaded runtime other workers keep going, which is why this often degrades
throughput rather than stopping everything.

The symptom is characteristic: latency that degrades under load in code that is not
itself slow, because the queue behind the blocked thread grows while the profile
looks fine.

Move blocking and CPU-bound work to a facility built for it, and keep the async path
for waiting. The concept is general; the API is not, and std provides nothing here.
Tokio offers `spawn_blocking`, plus `block_in_place`, which is multi-threaded-only and
panics on a current-thread runtime. Other runtimes have their own, and some have no
equivalent at all. The rule is not "no synchronous code" — it is that nothing which
waits on the OS or occupies the CPU for a long time belongs on a thread other tasks
need.

## Concurrency is not parallelism

Awaiting futures one after another runs them sequentially, whatever their type. Two
awaits in a row are two waits in a row.

To overlap them, hand them to something that polls them together — a join for "all
must finish", a select for "first one wins", a bounded stream when there are many. A
loop that awaits each item in turn is sequential no matter how async it looks, and
this is the most common reason async code is no faster than the blocking version it
replaced.

Unbounded concurrency is the opposite failure. Spawning one task per item in an
unbounded input is a resource exhaustion path; bound it explicitly.

## Task lifetime

Dropping a task handle does opposite things depending on the runtime, so this is the
one place a habit carried between codebases silently breaks. Under tokio and
async-std, dropping the handle detaches: the task keeps running and its result is
discarded. Under smol and anything else built on `async-task`, dropping the handle
**cancels** the task — it is marked `#[must_use]` for exactly this reason, and
`detach()` is the explicit opt-in to keep it running.

Measured on tokio 1.53 and smol 2.0: the same shape — spawn, drop the handle, wait —
runs the task under tokio and never runs it under smol.

So "a task with no owner" means a leak on one runtime and a silent no-op on another.
Either way the handle is the thing to keep: it is also how a panic reaches you, since
a detached task's failure surfaces nowhere. Give every spawned task an owner that
observes its outcome, and where a task truly should outlive its spawner, detach it
explicitly rather than by dropping the handle and hoping.

## Review checklist

- For each await: if the future is dropped here, is any work lost?
- Does any select loop poll a non-cancel-safe operation?
- Is cleanup written after an await rather than in a `Drop` guard?
- Is a blocking lock guard held across an await, especially on a single-threaded runtime?
- Is an async mutex used where a short blocking critical section would do?
- Is anything non-`Send` held across an await in a future that must be spawned across threads?
- Is any blocking or CPU-bound call sitting on the async path?
- Are sequential awaits intended, or meant to overlap?
- Is per-item concurrency bounded?
- Does every spawned task have an owner that observes its outcome?
- Does the code assume handle-drop detaches, on a runtime where it cancels?

## Anti-patterns

- A select loop that recreates and drops a non-cancel-safe read each pass.
- Cleanup after an await, on a path that can be cancelled.
- A blocking lock guard held across a suspension.
- Blocking I/O on the runtime, diagnosed as a slow database.
- A sequential await loop presented as concurrency.
- One spawned task per input item, unbounded.
- A detached task whose panic nobody observes.
- Assuming a dropped task handle detaches, which inverts between tokio and smol.
- Reaching for an async mutex by default, against tokio's own recommendation.
