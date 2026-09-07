---
name: refining-typography
description: Use when implementing or reviewing type scale, line-height, letter-spacing, measure, wrapping, truncation, tabular numbers, font loading, weight/size floors, punctuation, or mixed-direction text rendering. Stack-agnostic.
uses:
  - name: building-accessible-interfaces
    source: audacioustux/agents
  - name: building-color-systems
    source: audacioustux/agents
  - name: writing-interface-copy
    source: audacioustux/agents
  - name: improving-web-performance
    source: audacioustux/agents
  - name: building-responsive-layouts
    source: audacioustux/agents
---

# Refining Typography

## Role

Typography is mostly restraint: a sensible scale, comfortable spacing, deliberate wrapping. A label, a table cell, a headline, and an article paragraph do not share one rule set. This skill owns how text renders, wraps, and behaves at real content lengths.

Boundaries with siblings: semantic heading structure and whether contrast is required belong to `building-accessible-interfaces`; pick the heading element from document structure first. The words themselves belong to `writing-interface-copy`; keep copy natural-case here regardless. Building the color ramp and measuring a rendered pair belong to `building-color-systems`. Font loading performance belongs to `improving-web-performance`. Whether the surrounding layout has room for the text, and which direction it flows, belong to `building-responsive-layouts`.

Review by reading the rendered page, not the source: bad wrapping, widows, and truncation only show at real content lengths. Write fixes in the project's existing styling system.

## Scale and hierarchy

Define a small set of sizes and deviate from it as little as possible. Name sizes by role (`body-sm`, `heading`, `display`) once more than one person touches the system. Map heading levels to descending scale steps so a subordinate heading never overpowers its parent; adjacent levels may share a size toward the small end as long as weight or letter-spacing keeps them distinct.

## Line-height and letter-spacing

Headings tight, around `1.1`. Body copy `1.5`–`1.6`, unitless so it scales with font size. Anything wrapping to three or more lines needs at least `1.4`, even in a height-constrained row.

Display text set near `1.0` clips descenders (the parts of `g`, `y`, `p` below the baseline), worst in italics, which extend further. Bump line-height above `1.0` or reserve space below on any display treatment, and check italic and upright separately.

Large headings often read better with slightly negative letter-spacing; small uppercase labels need a little positive spacing; body copy at reading sizes needs neither.

## Measure, wrapping, truncation

Cap long-form text at 60–75 characters per line; any unit works if the rendered line lands in range (`references/measure-and-line-length.md` has the equivalents).

Four wrapping jobs: balance headings evenly across lines; keep a lone short word off a description's final line; let unbreakable strings (links, IDs) break inside their container instead of escaping it; keep labels and badges on one line where a break looks broken.

Balancing is capped by the engine because the algorithm is expensive: roughly 6 lines in Chromium and 10 in Firefox. Past that it is silently ignored, so applying it to body copy declares an intent that never runs. Orphan-prevention has no such limit and is the safer default for short-to-medium text. Skip both in genuinely long-form text, where default wrapping is fine and cheaper.

Truncate a single line with an ellipsis plus hidden overflow and no-wrap; truncate several with a line-clamp. Truncation hides content, so keep the full value reachable in a tooltip or expanded view when it matters.

## Numerals, features, real faces

Apply tabular numerals to any changing value (timers, counters, prices, numeric table columns) so digits stop shifting the layout on update. Leave static and decorative numbers, phone numbers, and version strings proportional; they never reflow, and tabular spacing reads as loose.

Expect glyph shapes to change, not just spacing. Narrow digits, `1` most visibly, are redrawn to fill an equal advance width. That is the feature working, but it is a visible change to the numerals, so confirm it in the project's own face rather than assuming it is invisible.

Prefer the CSS property over the raw OpenType feature tag when one exists: a weight property over a variation-settings tag, a numeric-variant property over a feature-settings tag. Properties keep working when a fallback font renders; raw tags silently do nothing. Reserve tags for custom axes with no property (`references/opentype-and-variable-fonts.md`).

Load the faces the design actually uses instead of a family that forces the browser to synthesize bold, italic, or small caps: synthesis distorts the real letterforms. Verify every emphasis form across the fallback stack before disabling synthesis outright; disabling it erases a missing form instead of reporting it.

## Weight and size floors

Below 18px, stay at weight 400 or heavier; thinner weights are display-only (28px+) and disappear at text sizes. Start long-form body around 16px; UI text can go to roughly 14px for inputs and menus, rarely below 12px for captions. Mobile text inputs still need 16px, since iOS Safari zooms the page below that (`references/ios-input-zoom-recipes.md`).

Forcing grayscale smoothing thins strokes. On macOS it is a common house style and can read as crisper, but it overrides a rendering choice the user's system made and costs perceived contrast, which is why it belongs with the weight floor rather than in a reset. Apply it to a whole document only when the type is already at or above the floor above, and never as a way to make a thin weight look intentional. If text reads too heavy, change weight or color, which every platform honours.

## Copy, punctuation, direction

Store copy in natural case, style with `text-transform`, so a redesign never means rewriting strings. Use curly quotes in prose (straight quotes in code), an en dash for ranges, a single ellipsis character instead of three periods, a non-breaking space to hold a value and its unit together, and a soft hyphen where a long word may break.

Pull underline position and thickness from the font's own metrics; the browser default sits inconsistently and can cut through descenders.

Set `lang` so pronunciation, quotes, and hyphenation resolve correctly, and `dir` at the boundary where direction changes. Wrap a mixed-direction value, such as a phone number or ID inside RTL text, in `<bdi>` so its order stays correct.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| Orphan word alone on a paragraph's last line | Enable orphan-prevention wrap on that element |
| Lopsided two-line heading | Enable balanced wrap on the heading |
| Digits shift the layout as a counter or price updates | Apply tabular numerals to that value |
| Descenders clipped on large display text, worse in italics | Raise line-height above 1.0 or reserve space below |
| Child heading renders larger or bolder than its parent section | Remap that section to descending scale steps |
| Long ID, link, or filename escapes its container | Add break-word wrapping to that element |
| Input font zooms the page on iOS Safari | Render the input at 16px via one of the zoom recipes |
| Thin or light weight used on sub-18px UI text | Raise to weight 400+, reserve thin weights for display sizes |
| Card or list description at heading line-height wraps to 3+ lines | Raise line-height to at least 1.4 |
| Hyphen used for a date or number range | Replace with an en dash |
| Mixed-direction value reorders inside RTL text | Wrap the value in `<bdi>` |
