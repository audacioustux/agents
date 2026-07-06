---
name: repo-ui-conventions
description: Use when building, modifying, or reviewing repository-owned frontend UI. Enforces predictable, calm, accessible, responsive, theme-safe interfaces without depending on a specific framework, language, component library, or visual style.
---

# Repo UI Conventions

## Role

Act as the first-pass and final-pass guardian for repository-owned user interfaces.

Prioritize:

1. the repository's explicit UI conventions,
2. existing components, tokens, themes, and styling patterns,
3. universal accessibility, usability, and web-quality standards,
4. broad UX principles only when they clarify a concrete product decision.

Do not invent a new design system, visual style, framework pattern, or styling stack unless the user explicitly asks for one.

## When to use

Use for:

- new screens, components, forms, dialogs, drawers, navigation, headers, filters, search, checkout, account, onboarding, dashboard, admin, or settings UI
- visual/UX reviews of code or screenshots
- dark/light theme checks
- PR review comments involving UI behavior, styling drift, accessibility, or layout resilience
- converting loose product requirements into implementation-ready UI rules

## Core rules

### Predictability

- Use familiar application and commerce patterns unless the product need clearly justifies a different pattern.
- Keep terminology consistent across UI copy, docs, URLs, and state names.
- Put the most important item first or last in short lists, nav groups, menus, and action rows.
- Keep primary paths focused; collapse or defer secondary actions before they crowd the main task.

### Calmness under pressure

- UIs must remain usable under loading, empty, error, partial-data, long-content, narrow-width, and high-zoom states.
- A component should not jump, resize, wrap into half-broken states, or hide core actions when data changes.
- Overlays, drawers, command palettes, and dialogs should keep a stable shell and scroll their contents internally.
- Empty states need a clear message, a clear next action when one exists, and balanced placement.

### Accessibility by default

- All interactive elements must be keyboard reachable and visibly focusable.
- Icon-only controls must have accessible names.
- Labels stay adjacent and programmatically associated with controls.
- Do not use color alone to communicate state.
- Normal text must meet WCAG AA contrast at minimum; muted text must remain readable.
- Motion must support comprehension and respect reduced-motion settings.

### Theme safety

- Light and dark themes are both first-class.
- Every new surface, border, text style, icon color, overlay, disabled state, hover state, focus state, and selected state must be checked in both themes.
- Prefer semantic theme variables, design tokens, or existing theme bridges over hardcoded raw colors.
- Do not introduce a light-only color into dark mode without a mapped dark equivalent.

### Styling discipline

- Use the repository's existing styling system.
- If the repo prefers utility-first styling, use utilities for one-off layout/spacing and extract repeated combinations into shared composition.
- If the repo uses CSS modules, vanilla CSS, styled components, design tokens, or a UI library, follow that system instead.
- Avoid arbitrary values when existing spacing, color, typography, radius, or shadow tokens will do.
- Remove unused scaffold CSS rather than letting a second styling system grow.

## Implementation stance

When changing UI code:

- make the smallest coherent change that improves the user experience;
- preserve existing component APIs unless there is a clear usability or maintainability reason to change them;
- keep implementation details close to the component unless reuse, theming, or consistency requires extraction;
- prefer semantic markup and native controls before custom ARIA;
- include loading, empty, error, disabled, active, selected, focus, hover, and long-content states when relevant.

## Review output format

When reviewing, group findings by severity:

- **Blocking** — users cannot complete the task, keyboard/screen-reader users are blocked, data loss risk, unusable contrast, broken layout, or primary action missing.
- **High** — confusing flow, unstable overlay, poor responsive fallback, missing state, invalid semantics, or theme mismatch likely to reach production.
- **Medium** — styling drift, inconsistent spacing/copy, weak empty state, minor keyboard/focus issue, or fragile implementation.
- **Low** — polish, naming, documentation, or cleanup.

For each finding, include:

1. what is wrong,
2. why it matters to users,
3. the smallest practical fix,
4. how to verify it.

## Anti-patterns

- Introducing a second visual language when the repository already has tokens, components, or conventions.
- Optimizing for novelty over predictable task completion.
- Treating screenshots as proof without checking loading, empty, error, long-content, narrow-width, and theme states.
- Hiding primary actions behind unclear overflow, icon-only, or responsive fallbacks.

## Final checklist

Before closing UI work, verify:

- light and dark themes look intentional,
- contrast is acceptable for normal and muted text,
- primary actions remain reachable at constrained widths,
- fallbacks appear before layouts look broken,
- dialogs and drawers stay stable under changing content,
- focus states and icon-only controls are accessible,
- loading, empty, error, and success states exist where needed,
- new shared styling improves reuse or consistency rather than adding drift.
