---
name: component-system-design
description: Use when building or reviewing UI components, component APIs, design tokens, theming, slots/composition, controlled and uncontrolled state, reusable styling, documentation, and component library boundaries. Stack-agnostic.
---

# Component System Design

## Role

Build reusable UI pieces that are accessible, composable, theme-aware, testable, and easy to adopt without locking the project into a particular framework or visual style.

## When to use

Use for:

- new components, primitives, blocks, patterns, templates, or shared UI modules
- refactoring repeated UI into shared components
- component API design, naming, state ownership, theming, tokens, styling composition, or documentation
- reviewing whether a component is too bespoke, too configurable, inaccessible, or theme-fragile

## Component taxonomy

Use these distinctions when deciding what to extract:

- **Primitive** — low-level behavior or semantic building block, such as Button, Input, Dialog, Tooltip, Tabs.
- **Component** — reusable product UI with a clear job, such as SearchBox, PriceField, AccountMenu.
- **Block** — larger composition of components for a product area, such as CheckoutSummary or UserTableToolbar.
- **Template** — layout scaffold with slots for page-specific content.

Do not prematurely extract a component just because code repeats once. Extract when reuse improves consistency, accessibility, theme safety, or maintenance.

## API principles

### Composition over configuration

Prefer composable children, slots, or named regions over large prop matrices.

Good APIs make common cases easy and unusual cases possible without breaking accessibility.

Avoid:

- boolean-prop explosions,
- style escape hatches as the normal customization path,
- APIs that hide required labels or semantics,
- “variant” names that encode a visual style but not a user intent,
- passing full rendered content into props when children/slots would be clearer.

### State ownership

Decide state ownership explicitly:

- local state for isolated UI state,
- controlled state when the parent owns value and transitions,
- uncontrolled state for simple forms and progressive enhancement,
- URL state for shareable filters, pagination, tabs, and search when appropriate,
- server/cache state for remote data,
- global state only when many distant parts of the app need the same client state.

Do not bury important state transitions in styling-only hooks or side effects.

### Accessibility cannot be optional

- Components with interactive behavior must ship keyboard and focus behavior.
- Icon-only variants require accessible names.
- Form components must expose label, description, error, disabled, required, and invalid states.
- Dialog-like components must define focus entry, focus restoration, dismissal, and scroll behavior.

## Theming and styling

- Use semantic tokens or existing theme variables for surface, text, border, focus, accent, danger, success, warning, muted, disabled, and overlay states.
- Every component must define light and dark behavior, even when the implementation inherits it.
- Avoid raw hex values, arbitrary spacing, and custom CSS that bypasses the design system.
- Prefer the repo's styling system. Utility-first, CSS modules, vanilla CSS, CSS-in-JS, design tokens, or UI-library styles are all acceptable when they are already the project convention.
- If the same styling combination appears repeatedly, extract a shared composition rather than copying drift.

## Component states to cover

For reusable components, consider:

- default,
- hover,
- active/pressed,
- focus-visible,
- selected/current,
- disabled,
- loading/pending,
- invalid/error,
- empty,
- success,
- long text,
- narrow container,
- high-contrast/dark mode,
- reduced motion.

Not every component needs every state, but missing relevant states should be deliberate.

## Documentation expectations

A shared component should document:

- purpose and when not to use it,
- required accessible labels or content,
- state ownership model,
- theming assumptions,
- responsive behavior,
- examples for common and edge cases,
- known constraints.

## Review checklist

- Is this component doing one clear job?
- Does the API encourage accessible usage by default?
- Can the component be themed without one-off overrides?
- Does it adapt to narrow containers and long content?
- Are repeated patterns extracted at the right level?
- Are escape hatches rare, named, and documented?
- Is styling reuse improving consistency rather than hiding bespoke decisions?

## Anti-patterns

- Extracting abstractions after one repetition or before the accessibility contract is known.
- Boolean-prop matrices, style escape hatches, and variants that encode visuals instead of intent.
- Components that own state the product flow needs to control.
- Reusable UI that bypasses repository tokens, themes, or established primitives.
