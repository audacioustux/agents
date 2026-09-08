---
name: building-accessible-interfaces
description: Use when implementing or reviewing accessibility, keyboard behavior, focus management, forms, validation, icon-only controls, reduced motion, target sizes, announcements, and assistive-technology semantics. Stack-agnostic.
uses:
  - name: animating-interface-motion
    source: audacioustux/agents
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
- Modals mark background content inert and contain overscroll, so neither focus nor scroll leaks behind the overlay. The backdrop must also visibly recede — dimmed or blurred, not merely present — because a modal that only differs from the page behind it by elevation gives no cue that the rest of the interface stopped responding.
- Do not create keyboard traps. Dialogs may trap focus while open but must provide Escape and visible close behavior unless the flow is intentionally blocking. Where surfaces stack, Escape dismisses one layer per press, innermost first, so a menu inside a dialog closes the menu rather than discarding the dialog and the work in it.
- Sticky headers, footers, toasts, and overlays must not fully obscure focused elements.
- Where repeated navigation or chrome precedes the content, a skip link is the first focusable element on the page, so a keyboard user does not tab the whole header on every view.
- Give anchor targets scroll margin clearing any sticky header. Without it, following an in-page link parks the destination heading underneath the header, out of sight.

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
- Native disabled and the ARIA disabled state are different tools and never belong on the same element. The native attribute removes the control from focus order entirely, which also means any tooltip explaining why it is unavailable can never be reached by keyboard or touch; the ARIA state only announces, leaving focus, behaviour, and styling to you. Reach for the native one when the control is genuinely inert, the ARIA one when the user still needs to discover why.
- Give each input a meaningful autocomplete token and name, and the input type and inputmode that summon the right keyboard. This is an accessibility affordance, not a convenience: it removes typing from users who find typing costly.
- Turn spellcheck off on emails, usernames, codes, and identifiers, where red squiggles flag correct input as wrong.
- A checkbox or radio and its label share one hit target, with no dead zone between them.
- Warn before discarding unsaved changes on navigation.
- Prefer a reversal window to a confirmation prompt where the action can be undone: perform it, announce it, and offer undo for long enough to notice and act. A prompt taxes every correct invocation to catch the rare wrong one, and is dismissed reflexively by exactly the users who most needed to read it. Reserve typed confirmation for actions that are genuinely irreversible, and make the reversal reachable by keyboard rather than only by a toast that times out.

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
- A press must cancel when the pointer travels away before release, so a drag that starts on a button does not fire it, and a user who presses then thinks better of it can slide off to escape.
- Keep focus feedback subtle. It fires on every keyboard step, so motion sized for a deliberate click becomes disorienting when it repeats down a whole form.
- Content revealed on scroll must not depend on the reveal to exist. Anything gated on entering the viewport should already be in the document and readable if the animation never runs, since an observer that does not fire leaves it permanently invisible to search, assistive technology, and anyone whose reduced-motion setting cancelled the transition.
- Respect reduced-motion preferences: ship a reduced variant rather than only disabling, so the state change still reads.
- When cancelling animation for reduced motion, collapse the duration to a near-zero value rather than removing the animation, so completion events still fire and any logic waiting on them does not hang.
- Motion is never the only feedback channel. Every animated state change also needs a static cue in colour, an icon, or a label, because the animation is gone a moment later and absent entirely under reduced motion.
- Motion that autoplays longer than a few seconds beside other content needs a pause, stop, or hide control.
- Animations must stay interruptible and respond to input mid-flight; a user who acts again should not wait out the previous animation. Which technique delivers that, and how motion is shaped once these constraints are met, belong to `animating-interface-motion`.
- Provide immediate visual feedback for user actions.
- Long-running work needs progress, optimistic feedback, skeletons, or clear pending states; avoid leaving users unsure whether anything happened.

### Dynamic content

- Announce important async changes without stealing focus unless the user's next step requires it.
- Loading, empty, error, partial, and success states must be understandable without color alone.
- Toasts and banners should be reachable or mirrored in-page when they contain critical information.
- A live region must already be in the document before its text changes. Inserting the region and its message together is the most common reason an announcement never fires, because the assistive technology had nothing to watch.
- Default announcements to polite. Reserve the assertive level for something the user must act on now, since it interrupts whatever is being read mid-sentence.
- On a client-side route change, update the document title and move focus to the new view's heading or main landmark. Without it the page silently replaces itself while focus stays on a control that no longer exists.

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
- Enable reduced motion and confirm each animation is reduced rather than merely switched off, so the state change still reads.
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
