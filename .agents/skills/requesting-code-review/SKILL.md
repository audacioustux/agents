---
name: requesting-code-review
description: Use when a task or feature is complete and before merging — dispatches a code-reviewer subagent with purpose-built context, rather than reviewing the work yourself.
---

# Requesting Code Review

Dispatch a code reviewer subagent to catch issues before they cascade. The reviewer gets precisely crafted context for evaluation — never your session's history. This keeps the reviewer focused on the work product, not your thought process, and preserves your own context for continued work.

**Core principle:** Review early, review often.

## When to Request Review

**Mandatory:**
- After each task in subagent-driven development
- After completing major feature
- Before merge to main

**Optional but valuable:**
- When stuck (fresh perspective)
- Before refactoring (baseline check)
- After fixing complex bug

## How to Request

**1. Get git SHAs:**
```bash
BASE_SHA=$(git rev-parse HEAD)   # capture BEFORE the work starts
HEAD_SHA=$(git rev-parse HEAD)   # after it finishes
```

Record the base before dispatching, not after. `HEAD~1` is wrong for anything
that landed as more than one commit: it reviews the last commit and silently
hides the rest of the task. Deriving it by grepping log messages is worse — it
breaks the moment a commit is reworded or squashed, and it fails open, handing
the reviewer a narrower diff with no indication anything is missing.

**2. Dispatch code reviewer subagent:**

Use Task tool with `general-purpose` type, fill template at `prompts/code-reviewer.md`

**Order the change for reading, not for the file tree.** Alphabetical order puts
a generated file above the one behavior actually changed in. Lead with core
logic — new behavior, algorithm and state changes, API surface — then the wiring
that connects it, then mechanical churn (renames, imports, formatting,
generated output) as a list of filenames rather than diffs. Attention is
freshest at the top; spend it on the part that can be wrong.

Where a core change is dense — nested conditions, a state machine, retry and
backoff — put a few lines of pseudocode beside it, stripped of syntax and error
handling, so the reviewer can confirm the intent before reading the code. Only
where the diff is genuinely hard to scan; a straightforward change does not need
a second description of itself.

**Placeholders:**
- `{DESCRIPTION}` - Brief summary of what you built
- `{PLAN_OR_REQUIREMENTS}` - What it should do
- `{BASE_SHA}` - Starting commit
- `{HEAD_SHA}` - Ending commit

**3. Act on feedback:**
- Fix Critical issues immediately
- Fix Important issues before proceeding
- Note Minor issues for later
- Push back if reviewer is wrong (with reasoning)

## Example

```
[Before starting Task 2]

BASE_SHA=$(git rev-parse HEAD)        # a7981ec — recorded up front

[Task 2 lands as three commits: Add verification function]

You: Let me request code review before proceeding.

HEAD_SHA=$(git rev-parse HEAD)        # 3df7661

[Dispatch code reviewer subagent]
  DESCRIPTION: Added verifyIndex() and repairIndex() with 4 issue types
  PLAN_OR_REQUIREMENTS: Task 2 from docs/plans/deployment-plan.md
  BASE_SHA: a7981ec
  HEAD_SHA: 3df7661

[Subagent returns]:
  Strengths: Clean architecture, real tests
  Issues:
    Important: Missing progress indicators
    Minor: Magic number (100) for reporting interval
  Assessment: Ready to proceed

You: [Fix progress indicators]
[Continue to Task 3]
```

## Integration with Workflows

**Subagent-Driven Development:**
- Review after EACH task
- Catch issues before they compound
- Fix before moving to next task

**Executing Plans:**
- Review after each task or at natural checkpoints
- Get feedback, apply, continue

**Ad-Hoc Development:**
- Review before merge
- Review when stuck

**Never pre-judge a finding in the prompt.** If your brief tells the reviewer not
to flag something, treat X as acceptable, or cap a class of issue at Minor, you
have decided the outcome and are paying for a review that can only agree with
you. Let it raise the finding and adjudicate afterwards. The phrasings to catch
in your own drafts are "do not flag", "don't treat X as a defect", "at most
Minor", and "the plan chose this" — each usually written to avoid a fix round.

**A finding the plan mandates is a decision, not a dismissal.** When the reviewer
flags something the plan explicitly required, the plan does not get to grade its
own work. Put the finding and the plan text side by side and decide which
governs — escalating if the choice is the user's. Dispatching a fix that
contradicts the plan, or waving the finding away because the plan asked for it,
both skip the decision.

**Resolve "cannot verify from diff" items yourself.** A reviewer sees the diff; a
requirement satisfied in unchanged code or spanning several tasks is outside what
it can check. Those items are not failures and do not block the rest of the
review, but each one is yours to close before the work counts as reviewed — you
hold the context the reviewer lacks. Confirm a real gap and it enters the fix
loop like any other finding.

**When earlier findings already exist:**

A reviewer that reads prior comments first anchors on them and returns a verdict on
someone else's list. Have it complete its own pass, then read what came before, then
fold in what it missed and attribute it.

A fix-round re-review inside a task loop is the one deliberate exception, and it
is narrow: its job is to verdict each open finding addressed or not, plus catch
new breakage inside the fix diff itself. Give it the list, and bound it to that
diff. Anywhere else — a re-review of the whole change after fixes, a second
opinion, a final pass before merge — point it at the current state rather than
the earlier verdict, or it grades the diff against a list instead of the code.

That independence is also what makes agreement informative: a finding two reviewers
reach separately is worth more than one raised twice, and worth far more than one
echoed after reading the other's report. Order the fixes by that weight.

## Red Flags

**Never:**
- Skip review because "it's simple"
- Ignore Critical issues
- Proceed with unfixed Important issues
- Argue with valid technical feedback

**If reviewer wrong:**
- Push back with technical reasoning
- Show code/tests that prove it works
- Request clarification

See template at `prompts/code-reviewer.md`.
