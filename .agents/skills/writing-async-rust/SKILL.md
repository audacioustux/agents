---
name: writing-async-rust
description: Use when writing or reviewing async Rust — deciding what happens when a future is dropped mid-await, holding state across await points, choosing between concurrency and parallelism, or diagnosing a task that stalls the runtime.
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

## When to use

Use for:

- async functions, and review of them
- anything that races, times out, or selects between futures
- state held across an await point
- a runtime that stalls, or a task that never completes

Ownership, error, and dispatch questions belong to `writing-idiomatic-rust`.

## Cancellation is the default, not the exception

A future does nothing until polled, and stops existing when dropped. Every timeout,
every race, every select arm that loses, and every aborted task drops a future
somewhere mid-execution.

So the question for any await point is: if the future is dropped here, what has
already happened and what never will? Everything before the await has run. Everything
after it may never run. Cleanup written after an await is cleanup that is not
guaranteed.

An operation is cancel-safe when dropping it mid-flight leaves no observable trace —
either it completed or it did not happen. Reading from a socket into a caller-owned
buffer is typically safe; reading into a buffer owned by the future is not, because
the bytes are consumed from the socket and then dropped with it. That is data loss
with no error and no panic.

This matters most in a select, whose losing arms are dropped every iteration. A
select in a loop over a non-cancel-safe operation will silently discard work on every
pass. Where an operation is not cancel-safe, hold it outside the loop so it is
polled to completion rather than recreated and dropped.

Where cleanup must happen regardless, it belongs in a guard whose `Drop` runs on the
abandoned path, not in code after the await.

## State held across an await

Anything alive across an await point lives inside the generated state machine, with
two consequences.

A future holding a non-`Send` value across an await is itself non-`Send` and cannot
move between threads. The error surfaces at the spawn site, far from the cause,
naming a type the author may not have realised was held.

More seriously, a lock guard held across an await keeps the lock for the entire
suspension — potentially forever, if the awaited operation never completes. This is
how async deadlocks are usually built: not by lock ordering, but by a guard living
longer than anyone intended. Take what the lock protects, release it, then await.
Where the lock genuinely must span the await, that is a design decision to state
explicitly, and it needs an async-aware lock rather than a blocking one.

Large values held across awaits inflate the state machine, which is allocated as a
whole. A future carrying a large buffer across several awaits pays for it at every
suspension point.

## Blocking

A blocking call inside an async task stalls the thread, and with it every other task
scheduled there. Runtimes multiplex many tasks onto few threads, so one blocking file
read or one CPU-bound loop can stop unrelated work with no error anywhere.

The symptom is characteristic: latency that degrades under load in code that is not
itself slow, because the queue behind the blocked thread grows while the profile
looks fine.

Move blocking and CPU-bound work to a facility built for it, and keep the async path
for waiting. The rule is not "no synchronous code" — it is that nothing which waits
on the OS or occupies the CPU for a long time belongs on a thread that other tasks
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

A spawned task outlives the scope that spawned it and keeps running when its handle
is dropped. Nothing cancels it implicitly, so a task with no owner is a leak that
also holds whatever it captured.

Detached tasks that fail silently are worth avoiding for the same reason: a panic in a
spawned task surfaces through its handle, and a dropped handle discards it.

## Review checklist

- For each await: if the future is dropped here, is any work lost?
- Does any select loop poll a non-cancel-safe operation?
- Is cleanup written after an await rather than in a `Drop` guard?
- Is any lock guard held across an await?
- Is anything non-`Send` held across an await in a future that must be spawned?
- Is any blocking or CPU-bound call sitting on the async path?
- Are sequential awaits intended, or meant to overlap?
- Is per-item concurrency bounded?
- Does every spawned task have an owner that observes its outcome?

## Anti-patterns

- A select loop that recreates and drops a non-cancel-safe read each pass.
- Cleanup after an await, on a path that can be cancelled.
- A blocking lock guard held across a suspension.
- Blocking I/O on the runtime, diagnosed as a slow database.
- A sequential await loop presented as concurrency.
- One spawned task per input item, unbounded.
- A detached task whose panic nobody observes.
