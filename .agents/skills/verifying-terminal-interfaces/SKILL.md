---
name: verifying-terminal-interfaces
description: Use when verifying an interactive CLI or TUI - prompt flows, keyboard handling, interrupts, resize, startup cost, hangs, or growth across repeated operations - by driving a real terminal session rather than reading the code or poking at it by hand. Stack-agnostic.
uses:
  - name: systematic-debugging
    source: audacioustux/agents
  - name: reviewing-ux-in-browser
    source: audacioustux/agents
  - name: verifying-before-completion
    source: audacioustux/agents
---

# Verifying Terminal Interfaces

A TUI's behaviour lives in what the terminal shows after a sequence of keys.
Reading the source tells you what it intends; driving it tells you what it does.
Manual poking finds the bug once and cannot show it again.

The surface here is a terminal. For a browser UI see `reviewing-ux-in-browser`;
for whether a completion claim is backed by evidence at all, regardless of
surface, see `verifying-before-completion`.

## Reuse before you build

Check for a harness the repo already owns: package scripts, e2e tests, an expect
script, a demo recorder, a PTY helper. A checked-in harness already knows the
startup path, the env the app needs, and which prompts appear — details a fresh
harness gets wrong silently, then reports as a defect in the app.

Build a temporary harness only when none exists, and keep it temporary unless
the user asks for a permanent one.

## The loop

1. Name the command under test and the smallest workspace that reproduces the
   behaviour.
2. Launch it in an isolated session with deterministic environment variables —
   fixed `TERM`, fixed width and height, no user config, no colour unless colour
   is what you are testing.
3. Capture the screen before interacting. This is your baseline; without it you
   cannot tell what your first keystroke changed.
4. Send one action at a time: text, Enter, an arrow, Escape, Ctrl-C, a resize.
5. Wait for a concrete screen pattern before the next action.
6. Save the transcript.
7. Kill the session explicitly.

A `tmux` session does all seven; when tmux is unavailable a PTY plus a poll loop
does the same thing with more code.

```bash
S="probe-$$"
trap 'tmux kill-session -t "$S" 2>/dev/null' EXIT   # survives a failed run
tmux new-session -d -s "$S" -x 100 -y 30 -e TERM=xterm-256color -- <command>
tmux capture-pane -pt "$S" > before.txt          # baseline, before any input

t0=$SECONDS                                      # elapsed, not shell uptime
until tmux capture-pane -pt "$S" | grep -q 'Ready'; do   # pattern, not sleep
  [ $((SECONDS - t0)) -gt 15 ] && { tmux capture-pane -pt "$S"; exit 1; }
  sleep 0.2                                      # do not busy-poll the pane
done

tmux send-keys -t "$S" 'help' Enter
tmux capture-pane -pt "$S" > after.txt
tmux kill-session -t "$S"
```

The deadline matters as much as the pattern: without it a hang becomes a hung
harness, and you learn nothing except that you waited.

## Wait for the screen, never for a duration

A `sleep` between keystrokes encodes a guess about how fast the machine is. It
passes on your laptop and fails under CI load, or it passes everywhere while
silently testing nothing because the app was still painting when you asserted.

Wait for the text that proves the state arrived — the prompt, the row, the
error. If nothing on screen distinguishes the state you are waiting for, that
is a finding about the interface: a user cannot tell either.

This is the same rule the `condition-based-waiting` reference in
`systematic-debugging` applies to tests, and it fails the same way here.

## What terminal verification catches that tests do not

- Keys the app never handles — arrows in a prompt, Ctrl-C during a write
- Layout that breaks at 80 columns, or on resize mid-render
- A spinner that never clears, leaving the cursor hidden after exit
- Startup cost paid on every invocation, invisible to a unit test
- A hang: the process is alive, the screen is unchanged, and no test asserts on
  a screen that stopped changing

For the two that are quantities rather than states — startup cost, and growth
across repeated operations — the harness is also the measuring instrument, and
the rule is comparison. Time the same command on the same machine before and
after the change, from the same working directory and environment, and report
both numbers; a single timing describes your laptop, not the program. For
growth, drive the same operation many times in one session and sample the
process between iterations rather than at the end, since a leak and a
one-off allocation look identical from a single reading.

## Report what the terminal showed

Paste the captured transcript, not a description of it. "The menu appeared" is
your reading of the screen; the screen is the evidence. Where before and after
matter, show both captures — a diff of two transcripts localises a regression
faster than any prose summary.

## Anti-patterns

- Asserting on the process exit code alone. A TUI can exit 0 having rendered
  nothing, or having rendered an error.
- Driving the app by calling its functions directly. That is a unit test with
  extra steps; it cannot see the terminal.
- Sending several keys at once because the waits are slow. The failure you are
  chasing usually lives between two of them.
- Reusing a fixed session name across runs. A session left behind by a failed
  run is still there, `new-session` on that name attaches instead of starting
  clean, and the transcript you capture is a mixture of two runs. Name sessions
  uniquely and kill them in a trap, so a crash cannot poison the next run.
