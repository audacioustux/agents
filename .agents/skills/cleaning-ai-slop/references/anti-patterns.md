# Anti-Patterns

Ways a cleanup pass goes wrong and changes behaviour.

| Impulse | Why It Fails |
|---|---|
| "I'll clean everything in one big pass" | Mixed changes are impossible to debug when tests break |
| "This abstraction is bad, let me redesign it" | Redesign is a separate task, not cleanup |
| "Tests pass, so I'll skip the per-pass verification" | A later pass may interact with an earlier change |
| "This code nearby also looks sloppy" | Scope creep. Only clean what's in scope |
| "The behavior is wrong anyway, I'll fix it while cleaning" | Behavior changes require their own task with their own tests |
| "I don't need regression tests, the code is simple" | Simple code breaks too. Lock behavior first |
