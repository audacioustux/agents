---
name: building-accessible-interfaces
description: Use when implementing or reviewing accessibility, keyboard behavior, focus management, forms, validation, icon-only controls, reduced motion, target sizes, announcements, and assistive-technology semantics. Stack-agnostic.
uses:
  - name: building-color-systems
    source: audacioustux/agents
  - name: refining-typography
    source: audacioustux/agents
  - name: writing-interface-copy
    source: audacioustux/agents
  - name: building-responsive-layouts
    source: audacioustux/agents
---

# Accessibility, Interactions, and Forms

## Role

Make interfaces usable with keyboard, screen readers, zoom, reduced motion, high contrast, touch, mouse, and imperfect input.

Target WCAG AA as the baseline. Prefer native HTML semantics and platform behavior before custom roles, scripts, or ARIA.

This skill decides when contrast is required and whether a pair fails. Measuring a rendered pair and changing colors belongs to `building-color-systems`. Text sizing and wrapping belong to `refining-typography`, error wording belongs to `writing-interface-copy`, and whether the layout leaves room for a target or a focus ring belongs to `building-responsive-layouts`. Where those skills are not installed, apply the rule here and note the gap.

## When to use

Use for:

- forms, inputs, validation, checkout, auth, onboarding, search, filters, settings, account recovery, and multi-step flows
- buttons, links, icon controls, menus, tabs, accordions, dialogs, drawers, toasts, live updates, drag/drop, and keyboard shortcuts
- accessibility reviews and remediation
- motion, focus, disabled, loading, and error state behavior

## Accessibility rules

### Native first

- Use real buttons for actions and real links for navigation.
- Use form controls with associated labels.
- Use semantic headings, landmarks, lists, tables, and fieldsets when the structure calls for them.
- Add ARIA only when native semantics are insufficient; incorrect ARIA is worse than no ARIA.

### Keyboard and focus

- All functionality must be reachable and operable by keyboard.
- Focus order should match visual/task order.
- Style the focus-visible state rather than bare focus, so keyboard users get an indicator without forcing one onto every mouse click. Never remove the outline without a verified replacement.
- Focus indicators must be visible and high-contrast in both light and dark themes, checked against every adjacent color the indicator crosses.
- Opening overlays should move focus to an appropriate element; closing overlays should restore focus to the trigger when practical.
- Modals mark background content inert and contain overscroll, so neither focus nor scroll leaks behind the overlay.
- Do not create keyboard traps. Dialogs may trap focus while open but must provide Escape and visible close behavior unless the flow is intentionally blocking.
- Sticky headers, footers, toasts, and overlays must not fully obscure focused elements.

### Forms

- Labels stay adjacent to controls and are programmatically associated.
- Helper text belongs near the field it explains.
- Error messages must identify the problem and how to fix it.
- On submit failure, focus the first actionable error or provide a summary that links to fields.
- Mark invalid fields programmatically when possible.
- Accept forgiving input where practical and normalize internally.
- Do not force redundant re-entry across multi-step flows unless security or data freshness requires it.
- Never block paste. Users paste passwords and one-time codes, and blocking it breaks password managers rather than improving security.
- Keep submit enabled until the request starts, then disable it with a pending indicator. A submit button disabled until the form validates hides why it cannot be pressed.
- Give each input a meaningful autocomplete token and name, and the input type and inputmode that summon the right keyboard. This is an accessibility affordance, not a convenience: it removes typing from users who find typing costly.
- Turn spellcheck off on emails, usernames, codes, and identifiers, where red squiggles flag correct input as wrong.
- A checkbox or radio and its label share one hit target, with no dead zone between them.
- Warn before discarding unsaved changes on navigation.

### Targets and gestures

- Meet the WCAG 2.5.8 AA floor of a 24 by 24 CSS-pixel target, or qualify for one of its spacing, inline, equivalent-control, or user-agent exceptions. Aim higher on touch, around 44 by 44, where density permits.
- Extend a small control's hit area with a pseudo-element rather than growing the visible element, and never let extended hit areas overlap.
- Provide non-drag alternatives for drag-only interactions.
- Avoid tiny adjacent controls, especially in dense tables, toolbars, and overlays.
- Set touch-action to suppress the double-tap zoom delay on controls, and set the tap-highlight deliberately rather than inheriting it.
- Gestures (drag, swipe, pinch, path) need a tap and keyboard alternative unless the gesture is genuinely essential.
- A drag that commits an action needs a stated commit threshold, and on release the element settles to a defined resting position rather than wherever the finger stopped. Below the threshold it returns to rest, so a hesitant drag is always cancellable.
- Suppress fling momentum on gestures that commit an action. Velocity deciding the outcome means a flick and a deliberate drag do different things, and the user cannot tell which they performed until it is done.
- Where a drag reveals several actions, stage them by distance rather than showing all at once, so the shallow gesture offers the common action and the deeper one is deliberate.
- Use autofocus sparingly: a single primary desktop input at most, and avoid it on mobile where it forces the keyboard open.

### Motion and feedback

- Motion should clarify cause, state, hierarchy, or continuity.
- An element that persists across a state change should move, not disappear and reappear: the same identity in both states is what tells the user this is the thing they were already looking at. Give that identity to exactly one element per state, since two claimants leave the transition no single thing to travel between and it collapses into a jump.
- Continuity and entrance are mutually exclusive for the same element. Something moving between two positions must not also be running an enter or exit, or it fades while it travels and reads as two objects rather than one; let the persisting element move and let only the surrounding content fade.
- Animate a property the engine can interpolate rather than swapping the class or rule that sets it. A swap flips to the end value immediately while the rest of the transition still runs, so corners and colours snap mid-flight even though the movement itself is smooth.
- A press must cancel when the pointer travels away before release, so a drag that starts on a button does not fire it, and a user who presses then thinks better of it can slide off to escape.
- Keep focus feedback subtle. It fires on every keyboard step, so motion sized for a deliberate click becomes disorienting when it repeats down a whole form.
- Content revealed on scroll must not depend on the reveal to exist. Anything gated on entering the viewport should already be in the document and readable if the animation never runs, since an observer that does not fire leaves it permanently invisible to search, assistive technology, and anyone whose reduced-motion setting cancelled the transition.
- Respect reduced-motion preferences: ship a reduced variant rather than only disabling, so the state change still reads.
- Motion that autoplays longer than a few seconds beside other content needs a pause, stop, or hide control.
- Animations must stay interruptible and respond to input mid-flight; a user who acts again should not wait out the previous animation.
- Stagger a composite entrance instead of animating one large block: animate the heading, body, and actions as separate pieces a short beat apart, so the eye is led through the content in reading order rather than meeting all of it at once.
- Exits should be quieter than entrances. An element leaving does not need the attention one arriving does, so let fade and softening carry most of a dismissal rather than travelling the full distance back out.
- Provide immediate visual feedback for user actions.
- Long-running work needs progress, optimistic feedback, skeletons, or clear pending states; avoid leaving users unsure whether anything happened.

### Dynamic content

- Announce important async changes without stealing focus unless the user's next step requires it.
- Loading, empty, error, partial, and success states must be understandable without color alone.
- Toasts and banners should be reachable or mirrored in-page when they contain critical information.

## Multi-step flows

For long or multi-step flows:

- show the current step and remaining path,
- keep a stable summary of key information,
- preserve in-progress work when practical,
- provide clear back/cancel behavior,
- keep support copy local and brief,
- avoid promotional or off-task content.

## Manual test checklist

- Tab through the full flow.
- Activate all controls with Enter or Space as appropriate.
- Check visible focus in light and dark themes.
- Test at 200% zoom.
- Review screen-reader names for icon-only controls.
- Submit invalid forms and confirm focus/error behavior.
- Enable reduced motion and verify non-essential animation is removed.
- Confirm color is not the only state signal.

## Review output format

For each issue:

- user affected,
- failure mode,
- minimal fix,
- verification step.

## Anti-patterns

- Replacing native controls with custom roles before native semantics are exhausted.
- Using placeholder text as the only label or relying on color alone for errors.
- Shipping icon-only, disabled, loading, or destructive controls without accessible names and focus behavior.
- Adding ARIA to silence audits without testing keyboard and screen-reader semantics.
