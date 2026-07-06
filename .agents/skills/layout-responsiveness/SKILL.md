---
name: layout-responsiveness
description: Use when implementing or reviewing responsive layouts, navigation, drawers, dialogs, dynamic result lists, progressive disclosure, constrained-width UI, container-query behavior, and layout stability. Stack-agnostic.
---

# Layout Responsiveness

## Role

Design and review layouts that adapt to available space without hiding core actions, breaking navigation, shifting content unnecessarily, or creating ambiguous intermediate states.

## When to use

Use for:

- page shells, app shells, headers, nav bars, sidebars, drawers, dialogs, cards, grids, tables, filters, search, command menus, and responsive forms
- components whose local container width matters more than viewport width
- overlays or panels with dynamic content while typing, filtering, loading, or paginating
- bug reports involving wrapping, overflow, scroll traps, layout shift, or hidden actions

## Principles

### Preserve task continuity

- Core actions must never disappear without an equally discoverable fallback.
- A fallback trigger must not appear while its fallback panel is unavailable.
- Navigation should collapse before it wraps into a broken or ambiguous half-state.
- Search/filter/result flows should keep the user's input, context, and primary next action visible.

### Collapse by importance

When space tightens, remove or collapse in this order:

1. decorative detail,
2. redundant metadata,
3. secondary helper copy,
4. secondary actions,
5. labels that still have accessible names,
6. layout density,
7. primary actions only when a clear fallback exists.

Never collapse labels, summaries, or controls that are required to understand the task unless an accessible alternative remains.

### Prefer local responsiveness

- Use container-driven behavior when a component's parent width is the real constraint.
- Avoid viewport breakpoints for reusable components when those components can appear in sidebars, split panes, modals, dashboards, or embedded cards.
- Define meaningful states such as compact, regular, dense, full, and overflow rather than scattering one-off breakpoints.

### Stable dynamic surfaces

- Dialogs, drawers, popovers, side panels, and command palettes should keep a stable outer shell.
- Scroll content internally when results change instead of resizing the entire overlay.
- Avoid scroll traps: users must be able to reach the start, end, close control, and primary action.
- Keep headers, filters, summaries, and action bars pinned only when it helps orientation and does not obscure focus.

### Prevent avoidable layout shift

- Reserve space for images, skeletons, async content, banners, and result counts when their size is predictable.
- Prefer skeletons or stable placeholders for content blocks over spinners that leave blank space.
- Do not insert promotional, unrelated, or surprise content into focused flows after the user begins interacting.

## Implementation checklist

For every responsive UI change, check:

- smallest supported width,
- largest expected width,
- 200% zoom,
- long labels and translated-like copy,
- empty, one-item, many-item, and loading states,
- sticky headers/footers and focus visibility,
- nested scroll containers,
- light and dark themes,
- keyboard navigation order.

## Review prompts

Ask these before accepting a layout:

- What remains reachable when the component is only 320px wide?
- Does the fallback appear before the layout looks broken?
- Is the component responding to viewport width or actual container width?
- Does changing results resize the shell while the user is typing?
- Are summaries, filters, close controls, and primary actions still discoverable?

## Output format

When proposing a fix, include:

- the intended responsive states,
- the collapse order,
- the stable regions versus scrollable regions,
- the exact verification cases.

## Anti-patterns

- Viewport-only breakpoints for components that also live in drawers, cards, split panes, or modals.
- Letting navigation, tables, cards, or action rows wrap into ambiguous half-states.
- Hiding core actions without an equally discoverable fallback.
- Fixing overflow by clipping content or disabling scroll without preserving task continuity.
