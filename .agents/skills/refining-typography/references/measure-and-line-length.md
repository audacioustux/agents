> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# Measure and line-length units

Lookup for capping long-form text at 60-75 characters per line. Any unit works as long as a cap exists and the rendered line lands in range; this table just maps the common choices to each other.

## Unit behavior

| Unit | Behavior |
| --- | --- |
| `px` | Fixed regardless of font size |
| `em` | Scales with the current element's font size |
| `rem` | Scales with the root font size |
| `ch` | Width of the `0` character in the current font; measures characters directly |
| `%` on a container | Relative to the parent's width, not font size |

## Rough pixel equivalents at a 16px body size

The 60-75 character range lands roughly between 560px and 680px at a 16px body size, though the exact figure depends on the font's average character width. Recheck it whenever the body font size or the typeface itself changes: a wider or narrower face shifts how many characters fit in a given pixel width.

| Target measure | Approximate width at 16px body |
| --- | --- |
| 60 characters | ~560px |
| 65 characters (`65ch`) | ~600-620px |
| 75 characters | ~680px |

## Practical guidance

- `ch` is the most direct unit because it ties the cap to actual character width, but it is sensitive to the specific font's average glyph width; a condensed face fits more characters in the same `ch` value than a wide one.
- A fixed pixel or rem max-width is just as valid as `ch`, as long as someone checks the rendered line length against the 60-75 character target for the actual body font in use.
- Recheck the cap after any font-family or base font-size change project-wide. A measure that was correct for one typeface can drift out of range for another at the same nominal width.
