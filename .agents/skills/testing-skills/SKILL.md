---
name: testing-skills
description: Use when a skill is written and before relying on it - pressure-tests the skill against an agent that wants to skip it, and closes the loopholes it finds
uses:
  - name: verifying-before-completion
    source: audacioustux/agents
  - name: test-driven-development
    source: audacioustux/agents
  - name: writing-skills
    source: audacioustux/agents
---

# Testing Skills

A skill that has never been tested is a hypothesis. Skills that enforce discipline are the ones agents rationalize their way around, so the test is adversarial: give an agent a reason to skip the skill and see whether it does.

## The Iron Law (Same as TDD)

``
NO SKILL WITHOUT A FAILING TEST FIRST
``

This applies to NEW skills AND EDITS to existing skills.

Write skill before testing? Delete it. Start over.
Edit skill without testing? Same violation.

**No exceptions:**
- Not for "simple additions"
- Not for "just adding a section"
- Not for "documentation updates"
- Don't keep untested changes as "reference"
- Don't "adapt" while running tests
- Delete means delete

**Background:** the `test-driven-development` skill explains why this matters, if installed. Same principles apply to documentation.

## Testing All Skill Types

Test approaches per skill type: technique, pattern, reference, and workflow. See `references/testing-skill-types.md`.

## Common Rationalizations for Skipping Testing

| Excuse | Reality |
|--------|---------|
| "Skill is obviously clear" | Clear to you ≠ clear to other agents. Test it. |
| "It's just a reference" | References can have gaps, unclear sections. Test retrieval. |
| "Testing is overkill" | Untested skills have issues. Always. 15 min testing saves hours. |
| "I'll test if problems emerge" | Problems = agents can't use skill. Test BEFORE deploying. |
| "Too tedious to test" | Testing is less tedious than debugging bad skill in production. |
| "I'm confident it's good" | Overconfidence guarantees issues. Test anyway. |
| "Academic review is enough" | Reading ≠ using. Test application scenarios. |
| "No time to test" | Deploying untested skill wastes more time fixing it later. |

**All of these mean: Test before deploying. No exceptions.**

## Bulletproofing Skills Against Rationalization

Closing the loopholes an agent finds when a discipline skill is inconvenient. See `references/bulletproofing.md`.

## RED-GREEN-REFACTOR for Skills

Follow the TDD cycle:

### RED: Write Failing Test (Baseline)

Run pressure scenario with subagent WITHOUT the skill. Document exact behavior:
- What choices did they make?
- What rationalizations did they use (verbatim)?
- Which pressures triggered violations?

This is "watch the test fail" - you must see what agents naturally do before writing the skill.

### GREEN: Write Minimal Skill

Write skill that addresses those specific rationalizations. Don't add extra content for hypothetical cases.

Run same scenarios WITH skill. Agent should now comply.

### REFACTOR: Close Loopholes

Agent found new rationalization? Add explicit counter. Re-test until bulletproof.

**Testing methodology:** See `references/testing-with-subagents.md` for the complete testing methodology:
- How to write pressure scenarios
- Pressure types (time, sunk cost, authority, exhaustion)
- Plugging holes systematically
- Meta-testing techniques

## STOP: Before Moving to Next Skill

**After writing ANY skill, you MUST STOP and complete the deployment process.**

**Do NOT:**
- Create multiple skills in batch without testing each
- Move to next skill before current one is verified
- Skip testing because "batching is more efficient"

**The deployment checklist below is MANDATORY for EACH skill.**

Deploying untested skills = deploying untested code. It's a violation of quality standards.

## Writing the skill itself

Structure, naming, description, and file layout are the `writing-skills` skill. This one starts once a draft exists.

## Deeper references

| Read | When |
|---|---|
| `references/bulletproofing.md` | Closing loopholes a test exposed |
| `references/testing-skill-types.md` | Choosing a test approach per skill type |
| `references/testing-with-subagents.md` | Running the test with a subagent |
