---
name: writing-interface-copy
description: Use when writing or reviewing user-facing copy - headlines, labels, errors, empty states, settings, toggles, confirmations, button text, and the copy of a landing or marketing page - for voice consistency, plain wording, translation safety, and copy that reads as generated (the same device in every section, figures or quotes nobody supplied).
uses:
  - name: refining-typography
    source: audacioustux/agents
  - name: building-accessible-interfaces
    source: audacioustux/agents
---

# Writing interface copy

Clear and brief beats clever; consistent beats varied. The best error message is the interaction redesigned so it cannot happen.

How copy renders (scale, capitalization via text-transform, truncation) belongs to `refining-typography`. Error markup and live-region announcements belong to `building-accessible-interfaces`. This skill owns the words themselves.

## Recon the existing voice

Before writing or reviewing, read the copy nearby. Note the product's terminology, localization conventions, and any content style guide already in force.

A deliberate brand voice is not a defect. Raise a departure from plain language only when it creates inconsistency, ambiguity, translation risk, or a tone the stakes don't support.

## One voice, flexible tone

The product has one voice, and its existing copy establishes it. A local edit does not get to invent a new one. Keep terms consistent: if it's "Archive" in the menu, it isn't "Move to storage" in the toast. Tone flexes with the stakes:

| Context | Tone |
| --- | --- |
| Success, onboarding, empty states | Warm, can be light |
| Routine actions, settings | Neutral, minimal |
| Errors, destructive confirmations | Calm, plain, zero playfulness |
| Data loss, security | Serious, explicit |

## Address the reader directly

Write "you", not "the user". In errors, "we" reads as deflection: prefer "Unable to load content" over "We're having trouble loading this content". An established first-person voice can stay in low-stakes copy that still reads clearly. Hold one perspective throughout a flow.

## Plain words, translation-safe sentences

Choose words a tired reader gets on the first pass, and delete every word that does no work. No idioms, no colloquialisms, no humor that won't translate.

The one rule worth memorizing exactly: never assemble a sentence from fragments around a variable, such as `"You have " + n + " new messages"`. Word order and pluralization rules change per language, so a fragment-built sentence breaks in translation even when it reads fine in the source. Use one complete templated string with proper pluralization handling, not a concatenation.

## Headlines make a claim the page keeps

A headline says what this product does for this reader, plainly enough that a first-time reader knows what the page offers before reading further. Swapping a competitor's name in is a quick prompt, not the test: a line that stays true of every product in the category ("The future of invoicing") usually names no job at all, while a line a rival could also truthfully use is still clear when it names the job ("Invoices that chase late payers for you"). Shared capability is not vagueness. Recommend a rewrite only when the headline and the copy under it together leave the reader unsure what this product does, which is the ambiguity that justifies raising a brand line at all. The page under it then has to keep the claim, since a headline promising speed above a six-step form reads as a broken promise. A deliberate brand line can stay when a specific line beside it does this work.

## Devices are a budget

A heading in two fragments ("Small oven. Big mornings."), a contrast ("X, not Y"), a run of refusals ("No X. No Y."), a dry line closing a paragraph, a list of three, an eyebrow over a section: none of these is a defect, and each works once, where it lands hardest. The defect is the default. Generated copy uses the same device in every section and the same warm, wry voice on every product, so a bakery and a CI tool sound alike; a reader cannot name what is wrong, only that it reads as copy. Before writing, decide where each device is spent; when reviewing, count them: a device in section after section is a tic, whatever it is.

The fix is a fact or a decision, never a synonym. Swapping the word a list bans for its neighbour moves the tic rather than removing it, and a rule applied everywhere breeds its replacement: "not X but Y" removed becomes "rather than" everywhere, "be specific" becomes a figure nobody measured, "end strong" becomes a quip on every paragraph. After an edit, look for the habit that took the removed one's place, and let a sentence that a rule made worse break the rule.

Two checks reach what a reading of one page cannot. When several pages were written in one session, read their first headings side by side: a shared shape or cadence is the session's tic, not the brand's voice. And give a reader who has not seen the brief only the page's text, asking what this is, who it is for, what it costs, and which three lines could sit on any site; rewrite those lines from the facts.

## Specifics are true or absent

A figure, a quote, a rating, a customer name or logo, a date, a price or an address comes from the source material or it is not on the page. Generated copy fills the gaps with plausible specifics, and that is the slop that looks like good copy: a review count, a batch number, a percentage nobody measured. Where the material lacks a fact, write around it ("Price on request") or leave the element out. An assumption the page cannot avoid (an opening time the brief omitted) is marked where it is made, as an attribute on the element and a line in the hand-off, so a reviewer finds it rather than trusting it. A placeholder, a bracketed note or a "sample" label never ships. A contact detail that is not the client's is never printed in a real format: a plausible phone number or street number points at a stranger's door.

## Verb-first buttons

A button label starts with a verb naming the action: "Send", "Save draft", "Delete project". Never a bare "Yes"/"No" or "OK!" on a consequential action; neither states what will happen.

A confirmation button repeats the consequence, so the dialog is answerable without reading the body: "Delete this project?" pairs with `Delete project` and `Cancel`, not `Yes` and `No`.

## Consistent flow vocabulary

A multi-step flow uses one vocabulary throughout: "Get started" to enter, "Continue" or "Next" (pick one) to advance, "Done" to finish. Alternating synonyms makes users wonder whether the buttons do different things.

## Links describe their destination

Link text must make sense out of context, because screen-reader users navigate by a list of the page's links. Write "Read the billing docs"; "Click here" fails this test. A bare "Learn more" breaks down once two appear on one page: suffix each one, as in "Learn more about exports".

## One capitalization policy

Pick title case or sentence case per element type, then apply it to every instance of that type. Sentence case is the safer default: calmer, no per-word rules to remember, and it localizes cleanly. "Save Changes" beside "Discard changes" reads as sloppiness, not style.

## Settings describe the ON state

Label a toggle for what happens when it's on. "Send read receipts" lets a reader infer the off state; the negative form ("Don't send read receipts") turns the toggle into a double negative.

Link straight to a referenced setting rather than describing the path to it: a "Notification settings" link, not "Go to Settings > Notifications > Email".

## Errors: an instruction beside the field

An error is an instruction, not a diagnosis, and it belongs beside the field that failed:

| Bad | Good |
| --- | --- |
| That password is too short | Choose a password with at least 8 characters |
| Invalid name | Use only letters for your name |
| Oops! Something went wrong. | Unable to save. Check your connection and try again. |

No blame, no "oops". Phrase hints positively ("Use only letters", not "Don't use numbers or symbols") and show them before the mistake happens where possible, not only after. A repeatedly firing error means the interaction needs redesigning, not rewording.

## Empty states point forward

An empty state says what this place is, how to fill it, and offers one clear next action. A search or filter empty state names the query and offers an exit: "No results for 'quarterly'. Clear filters." Never park persistent information in an empty state; it disappears once content exists.

## Placeholders are examples, not labels

A placeholder shows the expected format: `name@example.com`, `DD/MM/YYYY`. It vanishes on input, so it is never the only label; every field keeps a visible one.

## Detection table

| Symptom | Fix |
| --- | --- |
| A dialog whose buttons are Yes and No | Replace with the verb repeating the consequence: "Delete project" / "Cancel" |
| A toggle labelled with a negative ("Don't...") | Rewrite as the ON-state action |
| Copy concatenated around a count or name variable | Replace with one templated string with proper pluralization |
| Two "Learn more" links on one screen | Suffix each with its topic |
| Mixed title case and sentence case on the same element type | Pick sentence case and apply it everywhere for that type |
| An error that repeats "we" | Rewrite from the reader's perspective, drop the pronoun |
| An empty state holding a stat or notice found nowhere else | Move that content out of the empty state before it ships |
| A headline and the copy under it that together leave a first-time reader unsure what this product does (a line true of every product in the category is the usual sign) | Rewrite it to name the job, or add a line beside it that does |
| The same device in section after section: every heading in two fragments, every paragraph closing on a quip, "X, not Y" in each block | Keep the one that lands hardest; rewrite the rest from the facts, and check that no new device took the old one's place |
| A figure, quote, review, customer name or price that appears nowhere in the source material | Remove it or replace it with a fact; an unavoidable assumption is marked on its element and named in the hand-off |
