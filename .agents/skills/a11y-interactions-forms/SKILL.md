---
name: a11y-interactions-forms
description: Use when implementing or reviewing accessibility, keyboard behavior, focus management, forms, validation, icon-only controls, reduced motion, target sizes, announcements, and assistive-technology semantics. Stack-agnostic.
---

# Accessibility, Interactions, and Forms

## Role

Make interfaces usable with keyboard, screen readers, zoom, reduced motion, high contrast, touch, mouse, and imperfect input.

Target WCAG AA as the baseline. Prefer native HTML semantics and platform behavior before custom roles, scripts, or ARIA.

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
- Focus indicators must be visible and high-contrast in both light and dark themes.
- Opening overlays should move focus to an appropriate element; closing overlays should restore focus to the trigger when practical.
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

### Targets and gestures

- Frequent and primary actions should be comfortably targetable across device sizes.
- Provide non-drag alternatives for drag-only interactions.
- Avoid tiny adjacent controls, especially in dense tables, toolbars, and overlays.

### Motion and feedback

- Motion should clarify cause, state, hierarchy, or continuity.
- Respect reduced-motion preferences.
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
