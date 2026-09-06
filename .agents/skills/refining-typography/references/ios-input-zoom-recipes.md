> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# iOS input zoom recipes

iOS Safari zooms the whole page when a focused input's computed text size is smaller than 16px. This is an accessibility feature, not a bug: Safari treats anything smaller as too hard to read while typing. The fix always holds the input's rendered size at 16px; the two recipes below differ in what that does to the surrounding design, not in correctness. Pick whichever the design calls for.

## Recipe 1: size the input up on mobile

The input renders at 16px on small screens and drops to the design's intended smaller size once the viewport crosses into a desktop breakpoint. Nothing to compensate for, but the mobile input no longer visually matches the desktop one.

```css
input {
  font-size: 16px;
}

@media (min-width: 640px) {
  input {
    font-size: 13px;
  }
}
```

Use this recipe when the design tolerates the input looking slightly different (larger) on mobile than on desktop.

## Recipe 2: keep 16px, scale the rendered text down

Keep `font-size: 16px` at all times so Safari never triggers the zoom, then use a transform to render the text at the intended smaller size. The design stays visually identical at every viewport, at the cost of two compensating calculations.

The transform shrinks the whole element box, not only the glyphs, so:

- Widen the element by the inverse of the scale factor so it still fills its container once shrunk.
- Divide `line-height` by the same scale factor so the intended leading survives the shrink.
- Pin the transform origin to the start edge (`left` in LTR, `right` in RTL) so the text doesn't drift toward the center as it shrinks.
- Let a wrapper element draw the input's visible surface: background, border, focus ring. Keep the input itself transparent. A border or background on the scaled element shrinks along with the text and ends up missing the intended hit area.

```css
/* Example: rendering 13px from a 16px font-size. Scale factor = 13 / 16 = 0.8125 */
.input-wrapper {
  display: flex;
  align-items: center;
  height: 2.5rem;
  border-radius: 10px;
  background: var(--color-surface);
  padding-inline: 0.625rem;
}

.input-wrapper input {
  height: 100%;
  width: calc(100% / 0.8125);
  transform-origin: left center;
  transform: scale(0.8125);
  background: transparent;
  border: none;
  outline: none;
  font-size: 16px;
  line-height: calc(1.125 / 0.8125);
}

@media (min-width: 640px) {
  .input-wrapper input {
    width: 100%;
    transform: none;
    font-size: 13px;
  }
}
```

Above the breakpoint where the zoom risk no longer applies, drop the transform entirely and set the real intended size directly; carrying the scale forward past that point only adds unnecessary calculation for no visual benefit.

Use this recipe when the design requires the input to look and measure identically at every viewport width.
