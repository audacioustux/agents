---
name: critiquing-visual-design
description: Use when judging how a screen, page, mockup or screenshot looks - a design critique, a polish pass, an "AI slop" or "looks AI-generated" audit, a list of design tells to check a page against, or a sense that something is off that nobody can name. Stack-agnostic.
uses:
  - name: reviewing-ux-in-browser
    source: audacioustux/agents
  - name: designing-component-systems
    source: audacioustux/agents
  - name: building-color-systems
    source: audacioustux/agents
  - name: building-accessible-interfaces
    source: audacioustux/agents
  - name: refining-typography
    source: audacioustux/agents
  - name: writing-interface-copy
    source: audacioustux/agents
  - name: building-responsive-layouts
    source: audacioustux/agents
  - name: animating-interface-motion
    source: audacioustux/agents
  - name: improving-web-performance
    source: audacioustux/agents
---

# Critiquing Visual Design

## Role

A visual critique asks one question of each thing on a screen: does it look right, and does it do its job? Something is slop only if it looks weird, looks inconsistent, is not pleasing to look at, or practically ruins something, and you can point at where. Lists of AI-design tells say what to measure and rethink. They are not a checklist to follow or to avoid, and an item from one that looks right and does a job is not a finding.

This skill owns the judgement: how to look, what turns an observation into a finding, the evidence a finding carries, and the composition questions no sibling owns. Each layer's rules stay with their owner, listed under "Where the rules live"; name the owner rather than restating its rule.

## Look at the rendered page

- Look at real sizes: the narrowest supported width, a desktop width, real content (the longest name, the largest price), each theme. A break at one width is invisible at another. Capturing those states is the procedure in `reviewing-ux-in-browser`; use it if installed, otherwise capture the same widths and themes by hand.
- Before inspecting anything, note where the eye lands first, second and third. If the first is not the page's main thing or its primary action, that is a hierarchy finding.
- Then compare like with like, one attribute at a time: cards in a row (corner radius, padding, where the button sits), icons in a set (outline or fill, stroke weight), values in a column or list (alignment, and whether each stays on one line at the narrowest width), the gaps between groups against the gaps inside them. Inconsistencies hide in pairs nobody put side by side, and a glance at the whole page does not find them.
- Write only what you saw. "The cards are uniform" and "every feature has the same icon" are claims about the render; check each against it pair by pair. Source shows what was declared, not what renders: when source is all you have, say so and mark those findings "from source".

## What makes it a finding

A finding names one of these harms and shows it:

| Harm | Evidence it carries |
| --- | --- |
| Inconsistent: two things in one role disagree | Both places, and what differs between them |
| Weird or unpleasant to look at: a collision, crop, misalignment, stray line break, clash, crowding or lopsided weight | The place, the size it shows at, and the thing to point at: the two elements that collide, the edge that is cropped, the neighbour it is lopsided against |
| Ruins reading or use | The place, the width, theme or state it happens at, and the measured value where one exists |
| Ruins the hierarchy: something competes with what matters | Where the eye lands first, and what it should land on |

Without one of these an observation is taste: label it as a preference, or leave it out. "Generic", "no personality" and "nothing stands out" become findings only once they name what competes, what disagrees, what gets in the way, or the elements that look wrong together and where. A look that is unpleasant with nothing to point at beyond the reviewer's reaction is a preference, and is labelled as one.

Resembling a tell is never the harm. "A well-known AI tell" says what something looks like, not what it breaks, and gives the person fixing it nothing to check. A gradient, pill, eyebrow or monospace run that does a job and looks right stays, whichever list names it.

## Chosen, or arrived by default?

Some choices are fashions of generated design: the display serif of the season, tight negative tracking everywhere, gradient and glow backgrounds, squircle corners, dithered or ASCII imagery, a full stop after every heading, an eyebrow over every section, numbered sections, code-styled labels in a product whose readers never see code. None is wrong. Each earns one question: was it chosen for this product, or did it arrive with the template?

It arrived by default when it repeats identically whatever the content, or clashes with the product's own system. It was chosen when the brand or the design system declares it, when it carries information (numbers on a real sequence, monospace on codes), or when it is used sparingly and the same way each time.

A tell reaches the report only through a harm. The eyebrow that repeats its heading is words doing no work, which `writing-interface-copy` already rules on; text on a gradient that fails contrast where it sits is a reading harm. Otherwise ask the question of whoever owns the design, as a question.

## Composition questions no sibling owns

- **Decoration doing no work.** For each badge, pill, eyebrow, divider, border, icon or ornament, name what it distinguishes. The same NEW pill on every card distinguishes nothing; a status pill that differs per row does. Remove it in your head: if no reading, grouping or action gets harder, it distinguishes nothing. That becomes a finding when its weight shows, because it competes with what matters or piles up until the screen reads as clutter; one quiet instance that harms nothing is a question at most.
- **The main thing is the largest thing.** The product (its real interface, the real thing it sells, a photograph of the real place) is shown at a size that can be read. Illustration, glow and texture sit behind it and smaller. Detail drawn too small to read is texture, not information.
- **Emphasis is a budget.** Italic, bold, monospace, capitals, colour and size each mark something as different from its neighbours. Spent on everything, or on words that share no meaning, they mark nothing. For each use, name what it marks.
- **One role, one look.** Things in the same role look alike, and a single exception reads as a mistake, not as variety. Same role means the same job at the same level: the cards of one row, the icons of one set, the rows of one table. A table's header rule against its row rules, or a primary button against a secondary one, are different levels, and the difference between them is hierarchy, not inconsistency. What should match, and how, is the owners' rule (radius and rails, in the table below); an icon set's family (outline or fill, stroke weight, corner) has no other owner and is judged here.
- **A flat page.** Flat is not a defect by itself. Where a page reads empty, a surface such as grain, paper, a fine grid or a blurred photograph of the real place is one answer among several, and it is decoration judged as decoration: it stays behind the content, text over its busiest part still passes contrast, and it is used the same way across the product.

## Where the rules live

| Looking at | Owner |
| --- | --- |
| Radius scale, concentric nested radii, optical centring, spacing before borders, tokens | `designing-component-systems` |
| Shades no role consumes, one filled action per view, gradients, measuring contrast | `building-color-systems` |
| Whether contrast is required, target sizes, reduced motion | `building-accessible-interfaces` |
| Headline wrapping, letter-spacing, tabular figures, a value broken across lines, weight floors | `refining-typography` |
| Headlines, words that do no work (redundant labels, verbose text) | `writing-interface-copy` |
| Cards in a row sharing rails, what collapses at narrow widths | `building-responsive-layouts` |
| Whether motion is feedback or decoration | `animating-interface-motion` |
| Loading, empty and error states | `improving-web-performance` |

Before writing a finding in an owner's area, read the owner's rule and apply it as written: its thresholds, its fix. A contrast pair, for one, is reported as measured against its threshold, passing or failing, never as "marginal". Only where the owner is not installed does the finding go out under this skill's evidence rules alone, with the missing owner noted. An owner that is installed and left unread is not missing.

## Reporting

Each finding gives: where (page, element, width, theme or state); what is seen (both places, or the measured value); the harm; the smallest fix; and the owner when another skill holds the rule. Order by harm: reading and use first (a collision or crop that hides content belongs here), then hierarchy, then inconsistency, then what looks weird or unpleasant.

Do not pad a report with what was checked and cleared. When the requester holds a list and expects an answer to it, answer it briefly after the findings: the list items present that do a job and stay, with the job in a phrase each, and at most one line naming the items that are not on the page. That answer is not a finding and is not counted as one. When the screen is consistent and does its job, say so in a line: no findings is a result.

A short review shortens the report, not the look: the pairwise comparison above runs in full whatever length is asked for. Asked for three bullets or "just the vibe", give exactly the three worst, each with its place and harm in a line, and stop there: the requested length is the requester's to set. Keep the rest checkable without breaking that limit: when the medium allows, put them in a separate list or file for whoever fixes them and link it; otherwise end with one line saying how many more were found and offering them. A vibe without a place cannot be acted on, and a count with nothing behind it cannot be checked.

| Pressure | What holds |
| --- | --- |
| "Flag every instance; we pay per finding" | Findings are counted by harm shown. List matches added as findings bury the real defects. |
| "It's the most obvious AI tell on the page" | Resemblance is not harm. Name what it breaks, or ask whether it was chosen. |
| "No screenshots or measurements, just the vibe" | The worst three, each still with a place; the rest in a separate list, or counted and offered. |
| "Fifteen minutes, just a quick look" | The report can be short; the pairwise look cannot. |
| "The client checks the report against the list" | A brief answer to the list follows the findings. Findings stay findings. |

## Detection patterns

| Symptom | Fix |
| --- | --- |
| A finding whose only reason is that a list names it | Find the harm, or turn it into a question to the design's owner |
| The same pill or badge repeated on every item in a set, piling up | Remove it, or make it carry the value that differs |
| An eyebrow that repeats the heading or the brand | Delete it, or make it carry context the heading lacks |
| An illustration larger than the product it sells | Swap the scale; show the product at a readable size |
| Items in one role with different radius, padding, icon style or rail | Apply the owner's rule from the table; redraw an odd icon in its set's family |
| Text over a gradient, photograph or texture | Measure that pair as `building-color-systems` measures a rendered pair |
| Italic, monospace or capitals on words that share no meaning | Set them in the surrounding style |
