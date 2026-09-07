> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# Measuring contrast

This file covers measuring a rendered pair and, on request, changing it. Whether a given pair is required to pass a contrast threshold at all is a `building-accessible-interfaces` decision, not this skill's.

Contrast is measured between a foreground (text, an icon, a UI element) and the background it actually renders against - usually the nearest ancestor that paints one. Identify that background first; measuring against the page background when the element sits on a card gives the wrong answer.

**Report, do not repaint.** When a check fails, report the pair, its measured value, and the threshold it misses, then leave the colors unchanged - they are a design decision. Apply a fix only when asked.

## APCA thresholds

APCA (Accessible Perceptual Contrast Algorithm) models perceived contrast more accurately than the WCAG 2 ratio, so it is the better instrument for judging how a pair will actually read. It does not set the pass mark: `building-accessible-interfaces` holds WCAG AA as the baseline every pair must clear, and this table refines decisions above it. Lc (Lightness Contrast) measures perceived contrast between foreground and background; these levels simplify APCA's full font-size and weight lookup table:

| Content type | Minimum | Preferred |
| --- | --- | --- |
| Body text (columns or blocks) | Lc 75 | Lc 90 |
| Non-body text (labels, headlines) | Lc 60 | Lc 75 |
| Large text (≥36px) | Lc 45 | Lc 60 |
| UI components | Lc 30 | n/a |

Lc 30 is also APCA's minimum for disabled and placeholder text. The floor for a non-text element to be discernible at all is Lc 15.

Lc is signed: positive means dark text on a light background, negative means light text on a dark background. Compare the absolute value against the threshold.

## WCAG 2 thresholds

The baseline. Its luminance ratio is both too strict and too lenient depending on the pair, but it is what `building-accessible-interfaces` and `following-repo-ui-conventions` require, and it carries the legal standing APCA does not. A pair that fails AA fails, whatever its Lc.

| Content type | AA | AAA |
| --- | --- | --- |
| Normal text (<24px / <18.5px bold) | 4.5:1 | 7:1 |
| Large text (≥24px / ≥18.5px bold) | 3:1 | 4.5:1 |
| UI components and graphical objects | 3:1 | n/a |

WCAG defines large text in points: 18pt is roughly `24px`, 14pt bold is roughly `18.5px`.

WCAG is the gate and APCA is the tiebreaker for anything above it. Report both when they disagree: a pair that clears AA but sits below its Lc minimum is legible on paper and hard to read in practice, and that is worth saying.

## Fixing a failing pair, on request

Change lightness first - it is the channel contrast actually responds to. Hue and saturation move the measured value far less, so fixing contrast by changing hue is wasted effort.

Move the foreground away from the background in perceived lightness, holding hue and saturation, then remeasure:

```css
/* Failing: text too close to its background in lightness (Lc ~50) */
color: #7d93b0;
background: #eef2f7;

/* Fixed: darker text, same hue (Lc ~90) */
color: #2b3a4f;
background: #eef2f7;
```

Two constraints on the fix:

- **Mid-lightness backgrounds cap what is achievable.** On a background near 75% perceived lightness, even pure black text reaches only about Lc 60. Body text needs a background near one extreme, so a mid-range background is usually the thing that has to change.
- **Pushing lightness can push a color out of gamut.** Reduce saturation as needed to keep it renderable.

Always remeasure after changing a value - never assume a fix landed.

## Quick approximations

Useful for a first pass only; verify by measuring before reporting a result.

For body text targeting |Lc| >= 75:

- Light background (above ~90% perceived lightness): foreground below ~35%.
- Dark background (below ~25% perceived lightness): foreground above ~90%.

The gap is asymmetric because APCA is polarity-aware - mirrored pairs do not score identically, which is why a pair passing in light mode can fail in dark.

**Light or dark background?** The crossover is around 73% perceived lightness. Above it, dark text scores better; at or below it, light text scores higher. That threshold is higher than intuition suggests - between roughly 60% and 73%, the background already looks light, yet white text still measures meaningfully better than black.

## What to check

- **Every pair, in every appearance.** A pair passing in light mode can fail in dark; the palettes are not mirror images of each other.
- **Translucent surfaces.** A color on a blurred or overlaid header shifts with whatever scrolls behind it. Test against the lightest and darkest content it can sit over, or make the surface opaque enough that the shift cannot break the pair.
- **Computed colors.** Color-mix and relative-color functions, and opacity modifiers, all resolve at render time - measure the rendered result, not the declaration.
- **Text over images.** There is no single background color. Measure the worst region, or guarantee one with a scrim.
