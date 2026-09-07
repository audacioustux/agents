# Bulletproofing Skills Against Rationalization

Closing the loopholes an agent finds when a discipline skill is inconvenient.

Skills that enforce discipline (like TDD) need to resist rationalization. Agents are smart and will find loopholes when under pressure.

**Psychology note:** Understanding WHY persuasion techniques work helps you apply them systematically. See `references/persuasion-principles.md` for research foundation (Cialdini, 2021; Meincke et al., 2025) on authority, commitment, scarcity, social proof, and unity principles.

### Close Every Loophole Explicitly

Don't just state the rule - forbid specific workarounds:

<Bad>
```markdown
Write code before test? Delete it.
```
</Bad>

<Good>
```markdown
Write code before test? Delete it. Start over.

**No exceptions:**
- Don't keep it as "reference"
- Don't "adapt" it while writing tests
- Don't look at it
- Delete means delete
```
</Good>

### Address "Spirit vs Letter" Arguments

Add foundational principle early:

```markdown
**Violating the letter of the rules is violating the spirit of the rules.**
```

This cuts off entire class of "I'm following the spirit" rationalizations.

### Build Rationalization Table

Capture rationalizations from baseline testing (see Testing section below). Every excuse agents make goes in the table, paired with the rebuttal:

```markdown
| Excuse | Reality |
|--------|---------|
| "Too simple to test" | Simple code breaks. Test takes 30 seconds. |
| "I'll test after" | Tests passing immediately prove nothing. |
| "Tests after achieve same goals" | Tests-after = "what does this do?" Tests-first = "what should this do?" |
```

### Create Red Flags List

Make it easy for agents to self-check. The table above owns the excuses; this list
owns the observable states, and repeating an excuse here earns nothing:

```markdown
## Red Flags - STOP and Start Over

Observable states, not excuses. The excuses and their rebuttals live in
Common Rationalizations above.

- Code written before its test
- A test that passes the first time it runs
- Can't explain why the test failed
- Tests deferred to "later"

**All of these mean: Delete code. Start over with TDD.**
```

A quoted excuse belongs in exactly one place. If it is in the table, the red-flag
list points at the table rather than restating it — a reader who has to check two
lists for the same rule will drift one of them.

### Update CSO for Violation Symptoms

Add to description: symptoms of when you're ABOUT to violate the rule:

```yaml
description: use when implementing any feature or bugfix, before writing implementation code
```
