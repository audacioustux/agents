> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# OpenType features and variable-font axes

Lookup tables for when a design calls for a specific numbered feature or axis. Check the CSS property first; these tags are for cases with no property equivalent.

## Static vs variable

A static font ships one weight and style per file: regular, medium, and bold are three separate files. A variable font packs a whole range into one file, so any value in range works, such as `font-weight: 589`.

Variable is not automatically better. At one or two weights, static files can be smaller. At several weights, multiple optical sizes, or custom axes, a variable font usually wins.

## Registered axes

Variable-font controls, each with a four-letter tag. A font only supports the axes its designer built in; check the font's own documentation before relying on one.

| Axis | Tag | Controls | CSS property |
| --- | --- | --- | --- |
| Weight | `wght` | Stroke thickness | `font-weight` |
| Optical size | `opsz` | Details and spacing tuned for the display size | `font-optical-sizing` |
| Width | `wdth` | Glyph width | `font-stretch` |
| Slant | `slnt` | Slant angle | none; use `font-variation-settings: "slnt" N` |
| Italic | `ital` | Switches to a true italic design | `font-style: italic` |
| Custom | e.g. `GRAD` (Roboto Flex) | Whatever the designer built | none; raw tag only |

Optical sizes predate variable fonts; many families still ship them as separate files rather than an `opsz` axis. A text-optical variant is sturdier and more spaced for reading sizes; a display variant is finer and tighter for large ones. Use the variant matching the size being set, not the one whose name sounds right.

## OpenType features

Features are extra built-in options that work the same on static and variable fonts, unlike axes. A font ships only the features its designer included; verify before depending on one.

| Tag | Feature | CSS property |
| --- | --- | --- |
| `tnum` | Tabular numbers: every digit the same width | `font-variant-numeric: tabular-nums` |
| `zero` | Slashed zero, distinct from capital O | `font-variant-numeric: slashed-zero` |
| `liga` | Standard ligatures, joining pairs like "fi" | `font-variant-ligatures: common-ligatures` |
| `smcp` | Small capitals | `font-variant-caps: small-caps` |
| `sups` / `subs` | Superscript / subscript glyphs | `font-variant-position: super` / `sub` |
| `ss01`-`ss20` | Stylistic sets, numbered slots | none; `font-feature-settings: "ss01" 1` |
| `cv01`-`cv99` | Character variants, numbered slots | none; `font-feature-settings: "cv01" 1` |

Stylistic sets and character variants have no fixed meaning: `ss01` in one font might switch to open-style digits, and `cv11` in another might swap in a single-story lowercase `a`. Check the specific font's own documentation for what each numbered slot does before using it.

## Reference example

```css
/* Common axes and features: use the property */
.heading {
  font-weight: 650;
  font-optical-sizing: auto;
}

.price {
  font-variant-numeric: tabular-nums;
}

/* Custom axis or numbered slot: no property exists */
.heading-grade {
  font-variation-settings: "GRAD" 80;
}

.logo {
  font-feature-settings: "ss01" 1;
}
```
