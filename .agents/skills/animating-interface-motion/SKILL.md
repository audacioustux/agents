---
name: animating-interface-motion
description: Use when adding, tuning, or reviewing interface motion — entrances, exits, transitions between states, and whether an interaction should animate at all. Covers continuity, staging, and technique choice. Stack-agnostic.
uses:
  - name: building-accessible-interfaces
    source: audacioustux/agents
---

# Animating Interface Motion

## Role

Decide whether motion earns its place, and shape the motion that does.

`building-accessible-interfaces` owns what motion must satisfy: reduced-motion
preferences, interruptibility, pause controls, and the rule that motion is never the
only way a state change is communicated. Those are constraints, and they outrank
everything here. This skill covers the choices left once they are met.

## When to use

Use for:

- entrance and exit animation on any element
- transitions between two states of the same element
- deciding whether an interaction should animate at all
- reviewing motion that feels sluggish, busy, or unmotivated

## Whether to animate

Motion should clarify cause, state, hierarchy, or continuity. Motion that
communicates none of those is decoration, and decoration is paid for on every
interaction that carries it.

Frequency decides whether to animate at all. A duration that reads as polish on a
once-a-session action becomes a tax on one performed hundreds of times a day: a 300ms
menu opened 200 times costs a minute daily, and the user waits on every one. Animate
the rare and consequential; let the high-frequency path be instant, or animate only
its exit where that keeps the surface feeling settled.

When in doubt, cut the duration rather than the clarity. A fast animation that reads
beats a slow one that impresses.

## Continuity

An element that persists across a state change should move, not disappear and
reappear. The same identity in both states is what tells the user this is the thing
they were already looking at.

Give that identity to exactly one element per state. Two claimants leave the
transition no single thing to travel between, and it collapses into a jump.

Continuity and entrance are mutually exclusive for the same element. Something moving
between two positions must not also be running an enter or exit, or it fades while it
travels and reads as two objects rather than one. Let the persisting element move, and
let only the surrounding content fade.

## Entrances and exits

Stagger a composite entrance instead of animating one large block. Animate the
heading, body, and actions as separate pieces a short beat apart, so the eye is led
through the content in reading order rather than meeting all of it at once.

Exits should be quieter than entrances. An element leaving does not need the attention
one arriving does, so let fade and softening carry most of a dismissal rather than
travelling the full distance back out.

Direction of easing follows direction of travel. An element entering decelerates into
place, starting fast and settling; an element leaving accelerates away, starting at
rest and departing at speed. Material has specified this split across all three of its
versions, with the reason that a dismissal needs less attention than whatever the user
is doing next, and the same reasoning makes an exit shorter than its entrance.

No published source gives an empirical basis for any specific duration. Material
publishes concrete values — a token scale from 50ms upward — but they are vendor
convention, and each revision has removed more of the prose that once justified them;
Apple publishes no figure at all. The response-time limits often quoted here — a tenth
of a second for instantaneous, one second for unbroken flow — are latency budgets for
the system to begin responding, not durations for the motion that follows. Adopt a
vendor scale or declare your own, state it once as tokens, and scale with the distance
travelled rather than deriving a number from perception research that measured
something else.

## Technique

Interruptibility is a property of the technique, not of care taken. A transition
interpolates toward whatever the current target is, so a new intent mid-flight
retargets it. A keyframe sequence runs a fixed timeline and cannot change course once
started. Drive interactive state changes with transitions, and reserve keyframes for
staged sequences that run once.

Animate a property the engine can interpolate rather than swapping the class or rule
that sets it. A swap flips to the end value immediately while the rest of the
transition still runs, so corners and colours snap mid-flight even though the movement
itself is smooth.

A hover effect must not change the element's layout box. Growing a card by width or
height reflows its neighbours under the cursor; a transform scale avoids that but
overlaps whatever sits next to it and resamples text mid-flight. Either way the
cheapest correct move is to leave the box alone — lift the whole card, deepen its
shadow, scale an image inside a clipped frame — so nothing around it has to respond.

Motion tied to scroll position belongs on a scroll timeline rather than a scroll
listener. `animation-timeline: scroll()` drives an animation from a scroller's
progress and `view()` from an element's passage through the viewport, both off the
main thread, where the hand-rolled equivalent measures geometry on every scroll event.
Firefox ships neither outside preview, so put them behind `@supports` and make the
un-animated state the readable one. The reduced-motion obligation is external to this
feature — the scroll-animations spec's normative text imposes none — and scroll-triggered
decoration is exactly the non-essential motion WCAG names, parallax included.

## Review checklist

- Does each animation clarify cause, state, hierarchy, or continuity?
- Is anything on a high-frequency path animated that should be instant?
- Does a persisting element move, rather than vanishing and returning?
- Is any element running a continuity move and an entrance at once?
- Can every interactive animation be interrupted by a new intent?
- Does any value snap mid-transition because it is set by a class swap?
- Does an entrance decelerate and its exit accelerate, or do both share one curve?
- Does any hover state resize the element, reflowing or overlapping what sits beside it?
- Is scroll-linked motion driven by a scroll timeline, and readable where unsupported?

## Anti-patterns

- Animating a menu, tooltip, or toggle that opens hundreds of times a day.
- Two elements claiming the same continuity identity in one state.
- An exit that mirrors its entrance at full strength.
- Keyframes driving an interactive state change, so a second click waits out the first.
- A duration justified by the hundred-millisecond response-time limit, which measures
  when the system starts responding rather than how long the motion runs.
- Growing a card's width or height on hover, reflowing its neighbours under the cursor.
- Parallax or scroll reveals hand-rolled on a scroll listener with no reduced-motion path.
- Motion added because a surface looked static, with no state change to explain.
