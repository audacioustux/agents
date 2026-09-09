---
name: dispatching-parallel-agents
description: Use when facing 2+ independent problems — unrelated test failures, separate broken subsystems, parallel investigations — that can be worked concurrently. Not for executing an implementation plan; use subagent-driven-development for that.
uses:
  - name: subagent-driven-development
    source: audacioustux/agents
---

# Dispatching Parallel Agents

> Executing a written implementation plan task-by-task is a different job — that is `subagent-driven-development`, if installed.

## Overview

Subagents never inherit your session's context or history — you construct exactly what each one needs. `subagent-driven-development` explains why isolated context is the right default; this skill covers what changes when the work is parallel rather than sequential.

When you have multiple unrelated failures (different test files, different subsystems, different bugs), investigating them sequentially wastes time. Each investigation is independent and can happen in parallel.

**Core principle:** Dispatch one agent per independent problem domain. Let them work concurrently.

## When to use

```mermaid
flowchart TD
    N1{"Multiple failures?"} -->|"yes"| N2{"Are they independent?"}
    N2 -->|"no - related"| N3["Single agent investigates all"]
    N2 -->|"yes"| N4{"Can they work in parallel?"}
    N4 -->|"yes"| N5["Parallel dispatch"]
    N4 -->|"no - shared state"| N6["Partition ownership,<br/>else sequential"]
    N7["One agent per problem domain"]
```

**Use when:**
- 3+ test files failing with different root causes
- Multiple subsystems broken independently
- Each problem can be understood without context from others
- No shared state between investigations

**Don't use when:**
- Failures are related (fix one might fix others)
- Need to understand full system state
- Agents would interfere with each other

## The Pattern

### 1. Identify Independent Domains

Group failures by what's broken:
- File A tests: Tool approval flow
- File B tests: Batch completion behavior
- File C tests: Abort functionality

Each domain is independent - fixing tool approval doesn't affect abort tests.

### 2. Create Focused Agent Tasks

Each agent gets:
- **Specific scope:** One test file or subsystem
- **Clear goal:** Make these tests pass
- **Constraints:** Don't change other code
- **Expected output:** Summary of what you found and fixed

### 3. Dispatch in Parallel

```typescript
// In Claude Code / AI environment
Task("Fix agent-tool-abort.test.ts failures")
Task("Fix batch-completion-behavior.test.ts failures")
Task("Fix tool-approval-race-conditions.test.ts failures")
// All three run concurrently
```

### 4. Review and Integrate

When agents return:
- Read each summary
- Verify fixes don't conflict
- Run full test suite
- Integrate all changes

## Agent Prompt Structure

Good agent prompts are:
1. **Focused** - One clear problem domain
2. **Self-contained** - All context needed to understand the problem
3. **Specific about output** - What should the agent return?

```markdown
Fix the 3 failing tests in src/agents/agent-tool-abort.test.ts:

1. "should abort tool with partial output capture" - expects 'interrupted at' in message
2. "should handle mixed completed and aborted tools" - fast tool aborted instead of completed
3. "should properly track pendingToolCount" - expects 3 results but gets 0

These are timing/race condition issues. Your task:

1. Read the test file and understand what each test verifies
2. Identify root cause - timing issues or actual bugs?
3. Fix by:
   - Replacing arbitrary timeouts with event-based waiting
   - Fixing bugs in abort implementation if found
   - Adjusting test expectations if testing changed behavior

Do NOT just increase timeouts - find the real issue.

Return: Summary of what you found and what you fixed.
```

## Topology

These hold at any depth, and they are what let a fan-out converge without a
coordinator.

**The dispatcher does not also do the work.** Splitting scopes, reading results,
and deciding what comes next is one job; editing files is another. A dispatcher
that starts fixing something itself stops reading incoming results, and the
agents it launched drift or duplicate work. If you feel the urge to fix it
yourself, dispatch it as one more task.

**Agents do not talk sideways.** Each gets one scope and reports once, upward.
Sibling-to-sibling messaging reintroduces the coordination cost that fanning out
was meant to avoid, and makes any failure ambiguous — you can no longer tell
whether an agent was wrong or was told something wrong.

**Delegation is recursive.** An agent whose slice turns out to hold independent
parts can fan out over them itself and return one aggregated result. The rules
above apply unchanged at that level: you still see one result from one agent,
and you do not need to know it split the work.

**A late result reopens the plan.** Treat "done" as your decision to stop
dispatching, not as a state the agents reach. A result that lands after you
thought you were finished is information, not noise.

## Common Mistakes

**❌ Too broad:** "Fix all the tests" - agent gets lost
**✅ Specific:** "Fix agent-tool-abort.test.ts" - focused scope

**❌ No context:** "Fix the race condition" - agent doesn't know where
**✅ Context:** Paste the error messages and test names

**❌ No constraints:** Agent might refactor everything
**✅ Constraints:** "Do NOT change production code" or "Fix tests only"

**❌ Vague output:** "Fix it" - you don't know what changed
**✅ Specific:** "Return summary of root cause and changes"

## When NOT to use

**Related failures:** Fixing one might fix others - investigate together first
**Need full context:** Understanding requires seeing entire system
**Exploratory debugging:** You don't know what's broken yet
**Shared state:** Agents would interfere (editing same files, using same resources). Before falling back to sequential, try removing the sharing: give each agent its own file, key, or branch and merge at the read boundary. Two agents writing separate fields into one state file is still shared mutation; two files is not. Serialize only where a single writer is a real invariant, and do it structurally — a lock, a sequential phase, one owning agent. "Do not touch X" in a prompt is not concurrency control; it is a request.
**A deterministic lever exists:** A codemod, query, or script answers it exactly. Fan-out costs more and returns judgement where a tool returns a fact.

## Real Example from Session

**Scenario:** 6 test failures across 3 files after major refactoring

**Failures:**
- agent-tool-abort.test.ts: 3 failures (timing issues)
- batch-completion-behavior.test.ts: 2 failures (tools not executing)
- tool-approval-race-conditions.test.ts: 1 failure (execution count = 0)

**Decision:** Independent domains - abort logic separate from batch completion separate from race conditions

**Dispatch:**
```
Agent 1 → Fix agent-tool-abort.test.ts
Agent 2 → Fix batch-completion-behavior.test.ts
Agent 3 → Fix tool-approval-race-conditions.test.ts
```

**Results:**
- Agent 1: Replaced timeouts with event-based waiting
- Agent 2: Fixed event structure bug (threadId in wrong place)
- Agent 3: Added wait for async tool execution to complete

**Integration:** All fixes independent, no conflicts, full suite green

**Time saved:** 3 problems solved in parallel vs sequentially

## Key Benefits

1. **Parallelization** - Multiple investigations happen simultaneously
2. **Focus** - Each agent has narrow scope, less context to track
3. **Independence** - Agents don't interfere with each other
4. **Speed** - 3 problems solved in time of 1

## Verification

After agents return:
1. **Review each summary** - Understand what changed
2. **Check for conflicts** - Did agents edit same code?
3. **Run full suite** - Verify all fixes work together
4. **Spot check** - Agents can make systematic errors

## Real-World Impact

From debugging session (2025-10-03):
- 6 failures across 3 files
- 3 agents dispatched in parallel
- All investigations completed concurrently
- All fixes integrated successfully
- Zero conflicts between agent changes
