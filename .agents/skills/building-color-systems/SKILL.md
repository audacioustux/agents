---
name: building-color-systems
description: Use when constructing or auditing a color system - ramps, primitive and semantic color tokens, palette generation, color notation, gamut, or dark-mode palettes - before adding a new color, converting formats, or checking a rendered pair.
uses:
  - name: building-accessible-interfaces
    source: audacioustux/agents
  - name: designing-component-systems
    source: audacioustux/agents
  - name: refining-typography
    source: audacioustux/agents
---

# Building Color Systems

Color is one of the few interface concerns with an exact answer: never report a contrast value you did not measure, never estimate a color you could compute.

This skill owns system construction - ramps, the primitive/semantic tier split, notation and gamut, dark-mode palettes, measuring a rendered pair. Whether contrast is *required* belongs to `building-accessible-interfaces`; token consumption to `designing-component-systems`; text rendering to `refining-typography`.

## Match what the project already has

Reuse the project's tokens and notation before introducing a new one. A consistent hex system beats hex with a stray `oklch()` value dropped in to fix one case. For a genuinely new system, a perceptually uniform notation is the best default - its numbers behave the way the ramp rules below expect. Elsewhere, convert with a color library and emit the project's notation; never convert a value just because this skill loaded.

## A system is ramps, not a pile of colors

Most products need one neutral ramp, one accent ramp, and only the status ramps the product renders (danger, warning, success, info; add one only when a state ships). A ramp is not a gradient picked by eye: every step exists because a role consumes it - background, hover, border, fill, text. Do not generate a step nothing imports. See `references/ramp-roles.md`.

## Two tiers, one seam

**Primitives** name a value (`--blue-500`) and are never applied directly in a component. **Semantic tokens** name a job (`--color-text-secondary`), point at a primitive, and are the only tier components reference. That seam makes theming possible: dark mode, white-label, or increased contrast repoints the semantic tier, leaving primitives untouched. Without it, a later theme means auditing every usage to work out which meant "the accent" and which just wanted blue.

Never borrow a token because its value happens to look right today - a separator used as text color works until borders lighten, and the text drifts with them. A role with no token gets a new token, not a borrowed one.

## What makes a ramp well-formed

- Steps land evenly in *perceived* lightness, not whatever the format calls lightness - HSL's is not perceptual and bunches at one end.
- Hue stays constant end to end; a wandering hue reads as two colors blended.
- Vividness peaks mid-ramp, falling off at both ends; full vividness at the extremes gives a light step that glows and a dark step like ink.
- Steps sit denser at the light end - light surfaces need finer distinctions than dark ones.

Use a color library (`culori`, `colorjs.io`, `chroma.js`) to interpolate and measure; never build a ramp by eye. See `references/notation-and-gamut.md`.

## One color, one meaning

Use a color for exactly one purpose, treating any hue within 15 degrees as the same color. If the accent means interactive, that hue on static text tells users to click something that isn't; a neutral interactive element misleads too.

## Exactly one filled action per view

When a filled color encodes primary emphasis, one action gets it and peers stay neutral. Put the color on the background, not the label: a filled button reads as primary across the room, while accent-colored text on a neutral button reads as a link. Several colored backgrounds are fine encoding distinct states.

## Measure the rendered pair, then report

Measure a foreground against the background it actually renders on - including opacity or an image beneath it - not the page background. On failure, report the pair, its measured value, and the threshold missed, then leave the colors alone; change only when asked and remeasure. See `references/measuring-contrast.md`.

## Dark mode is not a mechanical inversion

Reversing the light palette is a starting point, not the output. Vividness usually needs to come down (confident on white reads as neon on near-black), the dark end needs more separation, and every pair needs rechecking - contrast is asymmetric, so a light pass can fail reversed.

Declare the document's color scheme so the browser themes what you do not paint: scrollbars, form controls, and the native select popup. Without it a dark page keeps light scrollbars and unreadable native menus, and no token audit catches it because no token is involved.

Elevation changes hands in dark mode. A shadow reads as depth by darkening what is behind it, and there is little left to darken on a near-black surface, so a raised element has to separate by getting lighter instead. Give the dark theme a small ladder of surface steps and let height select a step; keep the shadow for contact, not for the whole effect. A dark theme that ships the light theme's shadows has no depth at all, and the usual response - deepening the shadow further - cannot produce any.

## Gamut and interpolation

Generate ramps against sRGB unless display-restricted, and layer P3 as an enhancement: sRGB first, override inside a P3 media query, so every display gets something. A P3 color with no sRGB fallback fails outright.

Interpolation space is a look, not a correctness setting. An even, perceptually uniform space is the default for gradients, holding brightness steady. A polar space suits a two-hue gradient that goes grey in the middle. Plain sRGB darkens and mutes the midpoint, which is what most interfaces already have.

Polar spaces carry one hazard worth knowing before choosing one: hue is an angle, so interpolating it sweeps an arc and the gradient passes through hues neither stop declared. Blue to yellow can travel via green or via pink depending on which way round the wheel is shorter. A rectangular space interpolates in a straight line and cannot introduce a hue you did not pick, so prefer it whenever the surprise matters more than the extra chroma.

To move where a blend happens without inventing a colour, bias the midpoint between two stops rather than inserting a third one. A third stop pins a colour at a position and changes what the gradient contains; a midpoint hint only changes how fast one stop gives way to the next. Reach for the hint when a gradient is technically correct but weighted wrong, since adding a stop to fix a rate is what produces the extra bands nobody chose.

Perceived lightness must move in one direction across a gradient's stops. A stop that dips or rises against the run reads as a band at that position, and no interpolation space removes it, because the reversal is in the stops rather than the blend. Check lightness alone before reaching for a different colour space: an even space fixes how two stops meet, never a third stop placed out of order between them.

## Before you finish

| Symptom | Fix |
| --- | --- |
| Ramp built by varying HSL lightness | Rebuild against perceived lightness, constant hue |
| A separator token used as a text color | Add the missing role token; never borrow by value |
| `--blue-500` referenced directly in a component | Point a semantic token at it instead |
| `--color-primary` means the brand, `--color-text-primary` means body text | Reserve one word for the brand; `primary` means "most prominent in its group" |
| `prefers-color-scheme` sets some tokens, a class sets others | Pick one switching mechanism throughout |
| Contrast fixed by changing hue | Change lightness - the channel contrast responds to |
| P3 color declared with no sRGB fallback | Declare sRGB first, override only where P3 renders |
| Same saturation number copied across hues | Match the proportion of each hue's own peak vividness |
| Dark mode built by flipping light values mechanically | Reverse as a start, reduce vividness, widen the dark end, remeasure |
| A ramp step generated that no role consumes | Drop it; add it back only when a role needs it |
