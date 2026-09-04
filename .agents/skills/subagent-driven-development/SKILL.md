---
name: subagent-driven-development
description: Use when executing a written implementation plan and subagents are available - dispatches one subagent per task with curated context, then reviews each before integrating. Without subagents, use `executing-plans`.
uses:
  - name: writing-plans
    source: audacioustux/agents
  - name: executing-plans
    source: audacioustux/agents
  - name: using-git-worktrees
    source: audacioustux/agents
  - name: finishing-a-development-branch
    source: audacioustux/agents
  - name: requesting-code-review
    source: audacioustux/agents
  - name: test-driven-development
    source: audacioustux/agents
---

# Subagent-Driven Development

Execute plan by dispatching fresh subagent per task, with two-stage review after each: spec compliance review first, then code quality review.

**Why subagents:** You delegate tasks to specialized agents with isolated context. By precisely crafting their instructions and context, you ensure they stay focused and succeed at their task. They should never inherit your session's context or history — you construct exactly what they need. This also preserves your own context for coordination work.

**Core principle:** Fresh subagent per task + two-stage review (spec then quality) = high quality, fast iteration

**Continuous execution:** Do not pause to check in with your human partner between tasks. Execute all tasks from the plan without stopping. The only reasons to stop are: BLOCKED status you cannot resolve, ambiguity that genuinely prevents progress, or all tasks complete. "Should I continue?" prompts and progress summaries waste their time — they asked you to execute the plan, so execute it.

## When to Use

```mermaid
flowchart TD
    N1{"Have implementation plan?"} -->|"yes"| N2{"Tasks mostly independent?"}
    N1 -->|"no"| N3["Manual execution or brainstorm first"]
    N2 -->|"yes"| N4{"Stay in this session?"}
    N2 -->|"no - tightly coupled"| N3
    N4 -->|"yes"| N5["subagent-driven-development"]
    N4 -->|"no - parallel session"| N6["`executing-plans`"]
```

**vs. Executing Plans (parallel session):**
- Same session (no context switch)
- Fresh subagent per task (no context pollution)
- Two-stage review after each task: spec compliance first, then code quality
- Faster iteration (no human-in-loop between tasks)

## The Process

```mermaid
flowchart TD
    subgraph per_task["Per Task"]
        N1["Dispatch implementer subagent (./prompts/implementer.md)"]
        N2{"Implementer subagent asks questions?"}
        N3["Answer questions, provide context"]
        N4["Implementer subagent implements, tests, commits, self-reviews"]
        N5["Dispatch spec reviewer subagent (./prompts/spec-reviewer.md)"]
        N6{"Spec reviewer subagent confirms code matches spec?"}
        N7["Implementer subagent fixes spec gaps"]
        N8["Dispatch code quality reviewer subagent (./prompts/code-quality-reviewer.md)"]
        N9{"Code quality reviewer subagent approves?"}
        N10["Implementer subagent fixes quality issues"]
        N11["Mark task complete in TodoWrite"]
        N12["Read plan, extract all tasks with full text, note context, create TodoWrite"]
        N13{"More tasks remain?"}
        N14["Dispatch final code reviewer subagent for entire implementation"]
        N15["Use `finishing-a-development-branch`"]
    end
    N12 --> N1
    N1 --> N2
    N2 -->|"yes"| N3
    N3 --> N1
    N2 -->|"no"| N4
    N4 --> N5
    N5 --> N6
    N6 -->|"no"| N7
    N7 -->|"re-review"| N5
    N6 -->|"yes"| N8
    N8 --> N9
    N9 -->|"no"| N10
    N10 -->|"re-review"| N8
    N9 -->|"yes"| N11
    N11 --> N13
    N13 -->|"yes"| N1
    N13 -->|"no"| N14
    N14 --> N15
    style N15 fill:#d4edda
```

## Model Selection

Use the least powerful model that can handle each role to conserve cost and increase speed.

**Mechanical implementation tasks** (isolated functions, clear specs, 1-2 files): use a fast, cheap model. Most implementation tasks are mechanical when the plan is well-specified.

**Integration and judgment tasks** (multi-file coordination, pattern matching, debugging): use a standard model.

**Architecture, design, and review tasks**: use the most capable available model.

**Task complexity signals:**
- Touches 1-2 files with a complete spec → cheap model
- Touches multiple files with integration concerns → standard model
- Requires design judgment or broad codebase understanding → most capable model

## Handling Implementer Status

Implementer subagents report one of four statuses. Handle each appropriately:

**DONE:** Proceed to spec compliance review.

**DONE_WITH_CONCERNS:** The implementer completed the work but flagged doubts. Read the concerns before proceeding. If the concerns are about correctness or scope, address them before review. If they're observations (e.g., "this file is getting large"), note them and proceed to review.

**NEEDS_CONTEXT:** The implementer needs information that wasn't provided. Provide the missing context and re-dispatch.

**BLOCKED:** The implementer cannot complete the task. Assess the blocker:
1. If it's a context problem, provide more context and re-dispatch with the same model
2. If the task requires more reasoning, re-dispatch with a more capable model
3. If the task is too large, break it into smaller pieces
4. If the plan itself is wrong, escalate to the human

**Never** ignore an escalation or force the same model to retry without changes. If the implementer said it's stuck, something needs to change.

## Prompt Templates

- `./prompts/implementer.md` - Dispatch implementer subagent
- `./prompts/spec-reviewer.md` - Dispatch spec compliance reviewer subagent
- `./prompts/code-quality-reviewer.md` - Dispatch code quality reviewer subagent

## Example Workflow

An end-to-end run: dispatch, review, integrate. See `examples/full-workflow.md`.

## Advantages

Fresh context per task, curated inputs, automatic review checkpoints. See `references/why-subagents.md`.

## Red Flags

**Never:**
- Start implementation on main/master branch without explicit user consent
- Skip reviews (spec compliance OR code quality)
- Proceed with unfixed issues
- Dispatch multiple implementation subagents in parallel (conflicts)
- Make subagent read plan file (provide full text instead)
- Skip scene-setting context (subagent needs to understand where task fits)
- Ignore subagent questions (answer before letting them proceed)
- Accept "close enough" on spec compliance (spec reviewer found issues = not done)
- Skip review loops (reviewer found issues = implementer fixes = review again)
- Let implementer self-review replace actual review (both are needed)
- **Start code quality review before spec compliance is ✅** (wrong order)
- Move to next task while either review has open issues

**If subagent asks questions:**
- Answer clearly and completely
- Provide additional context if needed
- Don't rush them into implementation

**If reviewer finds issues:**
- Implementer (same subagent) fixes them
- Reviewer reviews again
- Repeat until approved
- Don't skip the re-review

**If subagent fails task:**
- Dispatch fix subagent with specific instructions
- Don't try to fix manually (context pollution)

## Integration

**Required workflow skills:**
- **`using-git-worktrees`** - Ensures isolated workspace (creates one or verifies existing)
- **`writing-plans`** - Creates the plan this skill executes
- **`requesting-code-review`** - Code review template for reviewer subagents
- **`finishing-a-development-branch`** - Complete development after all tasks; if not installed, merge or open a PR yourself and remove any worktree you created

**Subagents should use:**
- **`test-driven-development`** - Subagents follow TDD for each task

**Alternative workflow:**
- **`executing-plans`** - Use for parallel session instead of same-session execution
