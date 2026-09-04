---
name: web-quality-performance
description: Use when reviewing or improving frontend performance, Core Web Vitals, loading behavior, runtime responsiveness, visual stability, SEO basics, modern web best practices, security hygiene, and production readiness. Stack-agnostic.
---

# Web Quality and Performance

## Role

Improve real user experience by making pages fast to load, responsive to interact with, stable while rendering, understandable to search/crawlers where relevant, and resilient in production.

Use framework-specific optimizations only after identifying the generic web-quality problem.

## When to use

Use for:

- slow loading, large bundles, poor runtime responsiveness, layout shift, expensive rendering, heavy images, blocking fonts/scripts, network waterfalls, or long tasks
- Lighthouse/Core Web Vitals review
- SEO and metadata basics for public pages
- security and production-readiness hygiene visible from the frontend
- loading, skeleton, error, retry, cache, and optimistic-feedback improvements

## Quality model

Check four layers:

1. **Loading performance** — how quickly useful content appears.
2. **Interaction performance** — how quickly the UI responds after input.
3. **Visual stability** — whether content shifts unexpectedly.
4. **Operational resilience** — whether users get clear states when data, network, auth, or third-party services fail.

## Core Web Vitals targets

Use these as practical targets unless the product has stricter budgets:

- Largest Contentful Paint: good at 2.5s or faster.
- Interaction to Next Paint: good at 200ms or faster.
- Cumulative Layout Shift: good at 0.1 or lower.

## Performance practices

### Loading

- Prioritize critical content and remove unnecessary render blockers.
- Use appropriately sized images and modern formats when supported.
- Lazy-load below-the-fold content that is not needed for the initial task.
- Avoid font loading that hides or shifts critical text.
- Preload only high-confidence critical resources; excessive preloading competes with real work.
- Avoid shipping large unused JavaScript or CSS for the current route.

### Runtime responsiveness

- Keep input handlers small and predictable.
- Avoid unnecessary re-rendering or recomputation during typing, scrolling, dragging, and filtering.
- Debounce or defer expensive work when immediate precision is not required.
- Virtualize only when list size justifies it; do not add virtualization complexity for small lists.
- Move non-urgent work out of critical interaction paths.

### Visual stability

- Reserve dimensions for images, embeds, skeletons, ads, banners, and async result blocks.
- Avoid injecting content above the user's current task after interaction begins.
- Keep overlay shells stable while results update.

### Feedback and resilience

- Use skeletons for structured content and spinners only for short unknown waits.
- Provide retry behavior for recoverable errors.
- Distinguish empty from failed from unauthorized from still-loading states.
- Make optimistic updates reversible on failure.
- Keep the user's work recoverable in long flows.

## SEO and discoverability basics

For public pages:

- one meaningful page title,
- useful meta description where appropriate,
- logical headings,
- crawlable content for key information,
- canonical URL when duplicates exist,
- structured data only when accurate and maintained,
- meaningful link text,
- mobile-friendly layout,
- no accidental noindex/robots blockage.

Do not prioritize SEO over accessibility, correctness, or focused task completion.

## Security and best-practice hygiene

- Avoid exposing secrets in client code.
- Sanitize or safely render untrusted HTML.
- Avoid dangerous inline script/style patterns unless the project has a secure policy for them.
- Keep console output clean of debug noise and sensitive data.
- Handle third-party failures gracefully.
- Prefer secure transport and modern browser APIs.

## Review output format

For each issue:

- metric or user-visible symptom,
- likely cause,
- smallest fix,
- expected impact,
- how to verify.

When unsure, recommend measuring before rewriting.

## Anti-patterns

- Optimizing framework details before identifying the loading, interaction, stability, or resilience problem.
- Adding skeletons, lazy loading, or caching that hides broken states rather than improving user-perceived progress.
- Treating Lighthouse as the only proof instead of checking real task paths and runtime interactions.
- Shipping public pages without metadata, error states, or basic frontend security hygiene.
