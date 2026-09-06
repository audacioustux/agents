> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# Color notation, conversion, and gamut

Every rule in the main skill is stated perceptually and holds regardless of the notation a value is written in. This file covers picking a notation, converting between them, and what happens at the edge of a display's gamut.

## Choosing a notation

| Notation | Good for | Weakness |
| --- | --- | --- |
| Hex | Universal support, compact, what design tools hand you | Opaque - no channel is readable or editable by hand |
| `rgb()` | Same reach as hex, readable alpha | Channels do not correspond to anything a designer thinks in |
| `hsl()` | Channels look like design controls | Lightness is not perceptual and hue drifts across a ramp; a ramp built by varying `hsl()` lightness bunches at one end and shifts hue |
| A perceptually uniform notation (e.g. `oklch()`) | Perceptually even lightness, stable hue, predictable ramps | Newer baseline - a legacy browser matrix needs a fallback |

Match whatever the project already uses; notation choice is not itself a defect. For a genuinely new color system, a perceptually uniform notation is the best default, because even lightness steps stay even and a fixed hue stays fixed.

```css
oklch(L C H)          /* lightness 0-1, chroma 0-~0.4, hue 0-360 */
oklch(L C H / alpha)  /* alpha uses a slash, never a comma */
```

## Converting

Convert when the user asks, when a migration is already in scope, or when the project is standardizing on one notation and a value is the straggler. Never convert an isolated value in a project that deliberately uses something else, and never convert just because this skill loaded.

When conversion is in scope, change the values and nothing else:

- Leave CSS-level keywords alone: `currentColor`, `inherit`, `transparent`, `initial`, `unset`.
- Leave gradient interpolation methods alone; convert only the color stops inside them.
- Leave colors in third-party configs that expect a specific format.
- Preserve comments and formatting.

Bulk conversion is a migration, not cleanup - it shifts every rendered color by a rounding margin and touches files nobody asked about. Treat it as the task, not a side effect of something else.

## Gamut

Every sRGB color exists in the wider gamut most modern displays now support (commonly called P3), but not the reverse - the wider gamut covers roughly 50% more colors, which matters only for the most saturated values. A color at 60% of maximum vividness looks the same in both.

A color more vivid than the display can render gets clipped, and clipping is not graceful - it flattens neighbouring ramp steps into one rendered color, so the top of a ramp can lose its distinctions on a narrower-gamut screen. Maximum vividness varies by hue and lightness; cyans top out far lower than reds and purples, so a clipping ramp clips at some steps and not others.

The fix is to reduce vividness while holding hue and lightness constant. Generate ramps against sRGB unless the product is display-restricted, and add the wider gamut as a layered enhancement:

```css
.accent {
  background: #3b82f6;
}

@media (color-gamut: p3) {
  .accent {
    background: oklch(0.62 0.24 259);
  }
}
```

Order matters: the sRGB value comes first so every display gets something, and the wider-gamut rule overrides only where it will actually render. A wide-gamut color with no sRGB fallback does not degrade - it simply fails on displays that cannot show it.

For a browser matrix predating support for a newer notation, the same layering works with a feature query instead of a media query:

```css
.accent {
  background: #3b82f6;
}

@supports (color: oklch(0 0 0)) {
  .accent {
    background: oklch(0.62 0.19 259);
  }
}
```

Check the project's actual browser matrix before adding this layer - on a modern baseline it is dead weight.

## Computed color functions

Color-mixing and relative-color functions (deriving one color from another, adjusting a single channel of an existing color) all resolve at render time. Their output cannot be contrast-checked from the declaration; measure the rendered result instead. Keep generated values out of the token layer where possible, since a color defined by several chained derivations is not inspectable in a design tool.
