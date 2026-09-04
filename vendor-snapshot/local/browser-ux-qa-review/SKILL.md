---
name: browser-ux-qa-review
description: Use when a runnable app, preview URL, local server, or built page is available and the UI needs browser-based verification. Covers screenshots, responsive checks, theme checks, keyboard walkthroughs, console/network review, dynamic content, and final QA notes. Tool-agnostic; use whatever browser automation or DevTools tools are available.
---

# Browser UX QA Review

## Role

Verify UI behavior in a real browser rather than relying only on static code review.

Use available tools such as browser automation, DevTools, Playwright, screenshots, accessibility snapshots, console logs, network inspection, and performance traces. If no tools are available, provide a manual QA script the user can run.

## When to use

Use for:

- final UI review before closing work,
- visual regression-style checks,
- forms, drawers, dialogs, menus, filters, search, checkout, onboarding, account, and dynamic result pages,
- reports of broken layout, inaccessible controls, slow interactions, console errors, or network failures,
- validating light/dark themes and responsive behavior.

## Review procedure

### 1. Establish the task path

Identify the main user goal, required screens, primary action, and expected completion state.

Do not explore randomly first. Start with the intended task and note deviations.

### 2. Capture baseline states

Check:

- initial load,
- loading state,
- empty state,
- populated state,
- error or failed-network state where practical,
- success/completion state,
- authenticated/unauthenticated state where relevant.

### 3. Inspect visual and responsive behavior

At minimum, inspect:

- narrow mobile-sized viewport,
- tablet-ish width,
- desktop width,
- constrained container/panel width when relevant,
- light theme,
- dark theme,
- long labels or realistic content.

Look for wrapping, overflow, clipped controls, unstable shells, hidden primary actions, unreadable muted text, and fallbacks that arrive too late.

### 4. Walk the interaction path

Use mouse and keyboard:

- Tab through controls,
- activate with Enter/Space where appropriate,
- open and close overlays,
- type into inputs,
- submit invalid and valid forms,
- filter/search while observing layout stability,
- test Escape/back/cancel behavior,
- confirm focus restoration.

### 5. Check diagnostics

When tools allow, check:

- console errors and warnings,
- failed or suspicious network requests,
- slow requests on the critical path,
- excessive layout shifts,
- long tasks during interaction,
- accessibility tree names for icon-only controls,
- screenshot evidence for before/after.

### 6. Report with evidence

For each finding, include:

- where it occurs,
- exact reproduction steps,
- visible/user impact,
- suspected cause,
- suggested fix,
- verification step.

Use screenshots or concise textual observations when possible.

## Acceptance gate

Do not mark UI work complete until:

- no blocking console/runtime errors appear on the reviewed path,
- primary path works with keyboard,
- primary actions remain reachable at constrained widths,
- overlays remain stable under dynamic content,
- light and dark themes both look intentional,
- normal and muted text remain readable,
- icon-only controls have accessible names,
- loading, empty, and error states are intentional.

## Manual QA script template

When browser tools are unavailable, provide this to the user:

1. Open the page in light theme and complete the primary task.
2. Repeat in dark theme.
3. Resize to a narrow mobile width and repeat the primary task.
4. Tab through the page and activate controls without a mouse.
5. Submit invalid form data and check the first error focus/message.
6. Type/filter quickly in dynamic panels and watch whether the shell jumps.
7. Open DevTools and check console errors plus failed network requests.
8. Confirm loading, empty, error, and success states.

## Anti-patterns

- Declaring UI complete from code review alone when a runnable app or preview exists.
- Taking one desktop screenshot and skipping keyboard, responsive, theme, console, and network checks.
- Exploring randomly instead of following the user task path and expected completion state.
- Reporting subjective visual opinions without tying them to observable user impact.
