> Adapted from [jakubkrehel/skills](https://github.com/jakubkrehel/skills) (MIT), point-in-time snapshot. Upstream is authoritative for changes.

# Ramp step to role mapping

What each step in a ramp is for, and the two common step-count conventions a project may already be using.

## Role table

| Role | 11-step convention (e.g. `50`-`950`) | 12-step convention (e.g. `1`-`12`) |
| --- | --- | --- |
| Page background | `50` | `1` |
| Subtle background | `50` | `2` |
| Component background | `100` | `3` |
| Component hover | `200` | `4` |
| Component active / selected | `200` | `5` |
| Subtle border | `200` | `6` |
| Border, separator | `300` | `7` |
| Strong border, focus ring | `400` | `8` |
| Solid fill | `500` | `9` |
| Solid fill hover | `600` | `10` |
| Low-contrast text | `700` | `11` |
| High-contrast text | `900` | `12` |

The two conventions differ in kind, not just in numbering:

- **The 12-step convention defines its steps by role.** The solid-fill step is the solid fill in every ramp and in every appearance. A separate dark-mode ramp reuses the same step numbers, so component CSS never has to change between appearances.
- **The 11-step convention defines its steps by lightness.** The lowest number is light, the highest is dark. The mapping above holds in light mode and inverts in dark mode - page background moves to the highest step, high-contrast text to the lowest. Components either swap step numbers per appearance or read a semantic token that swaps once.

Match whichever the project already uses. For a new system, the role-defined (12-step) model tends to survive a theme change better, because a role-defined step does not need remapping when the appearance flips. On an 11-step system, keep the numbering and put the role mapping in the semantic tier instead of renumbering the ramp.

The 11-step convention covers 12 roles with 11 steps, so two roles share a step in the table above. Where a design needs both roles visually distinguishable (for example, a hover state and a subtle border that must read apart), that is a real signal the ramp needs a twelfth step, not a mapping error.

## Per-ramp step counts

| Ramp | How many steps |
| --- | --- |
| Neutral | Full range - neutrals carry the most roles (backgrounds, borders, body text), so give them at least as many steps as the accent |
| Accent | Full range |
| Status (danger, warning, success, info) | Usually four roles - a background, a border, a solid fill, and text. Generate the full range only where the product styles a status component across the whole ramp |

## Auditing an existing palette against roles

1. Collect every color literal in the codebase - hex, `rgb()`, `hsl()`, `oklch()`, and the project's own utility-class prefixes. Check SVGs, chart configs, and email templates too; colors hide outside stylesheets.
2. Sort by perceived lightness within each hue family. Near-duplicates surface as neighbors.
3. Collapse colors closer than about one ramp step - they are one color that drifted. Keep whichever is used most; never average two colors together.
4. Assign each survivor a role from the table above. A color matching no role is either a missing token or a mistake - decide which and say so.
5. Count what is left. More than one ramp filling the same role means the palette outgrew its structure, not that the product needs more color.

Report the inventory before changing anything. Consolidating a palette changes rendered pixels nobody asked to have touched, so treat it as a proposal until accepted.
