---
name: diagramming-with-mermaid
description: "Use when creating, reviewing, or fixing a Mermaid diagram embedded in Markdown documentation — choosing a diagram type, writing or debugging a mermaid fence, or reviewing a doc PR that touches one."
---
# Mermaid Diagrams

## Role

Covers writing, reviewing, and fixing a Mermaid diagram once you're using one: choosing the right diagram type, structuring it so it stays readable, and avoiding known parse pitfalls. This skill does not decide whether Mermaid is required over other formats — that's the consuming repo's call. If the current repo has its own diagram-conventions doc, treat it as canonical and this skill as a portable baseline for the mechanics; re-read the repo doc if its guidance differs, since Mermaid's supported types evolve and any such table is a point-in-time snapshot.

## Source and placement

- Write every diagram as a fenced `mermaid` block inside the Markdown document that explains it — not in a separate diagram source file.
- Keep the diagram adjacent to the claims it illustrates, so review and updates happen together.

## Choosing a diagram type

Pick the smallest Mermaid diagram type that matches the claim:

- **Flowchart** (`flowchart LR`/`TB`) — architecture, topology, process, data, or decision flow.
- **Sequence diagram** (`sequenceDiagram`) — ordered interactions: requests, replies, retries, handoffs.
- **State diagram** (`stateDiagram-v2`) — finite states and allowed transitions, only when correctness depends on which transitions are legal.
- **Entity-relationship diagram** (`erDiagram`) — persisted entities and cardinality, not a schema dump.

Mermaid supports many more types (class, Gantt, pie, git graph, C4, block, etc.). Load `references/diagram-type-table.md` for the full catalog with links and starter syntax before reaching for anything outside the four above — and verify the target Markdown host's bundled Mermaid version actually supports a less common or experimental (`-beta`) type before relying on it; the table is a snapshot, not a support guarantee.

Split a dense architecture map into an overview and focused companion diagrams rather than making a reader trace crossing edges.

## Flowcharts and topologies

- One dominant reading direction: left-to-right for handoffs/data flow, top-to-bottom for layered systems.
- Define stable node IDs separately from reader-facing labels; connect IDs, not labels.
- Solid arrow for the primary relationship; dashed arrow only when its meaning (optional path, async notification) is stated in prose or a legend.
- Label edges with the action (`persist event`, `authorize request`), never a vague relationship (`uses`, `connects`).
- Subgraphs represent visible containment/ownership only, nested at most one level unless a second level is essential. Connect external edges to nodes inside a subgraph; a subgraph ID may itself be an edge endpoint (`groupA --> groupB`) only for a single summary edge between whole clusters.
- Push qualifications and implementation detail into surrounding prose, not into node labels. Use `%%` comments only for non-obvious source organization or a necessary layout decision.

## Sequence diagrams

- Explicit `participant` list, messages in temporal order.
- Model only interactions that matter to the documented behavior; split an exceptional path into its own diagram when it obscures the main path.
- Solid arrows for requests, dashed arrows for replies, consistently.
- State retries, failures, and async work in labels or nearby prose — never imply a guarantee the system doesn't provide.

## State diagrams

- Use only when correctness depends on which transitions are allowed — not for a loose workflow with no transition rules to preserve.
- Name states as stable conditions, label transitions with the triggering event, show terminal states explicitly (`[*]`).

## Entity-relationship diagrams

- `erDiagram` for persisted data relationships; show cardinality and domain-accurate relationship names.
- Include only fields needed for identity, ownership, constraints, or a documented query — exhaustive field lists belong in the schema/reference doc, not the diagram.

## Titling

Let an adjacent Markdown heading name the diagram; don't repeat the heading's wording inside the fence. Only when there's no diagram-specific heading directly above the fence, give it a name via Mermaid frontmatter (`---\ntitle: ...\n---`) instead of inventing a heading just to hold it. Never put the title in both places.

## Styling

Default Mermaid rendering is preferred. Style only when color or shape conveys a documented, reusable semantic distinction — never as the sole carrier of meaning. Keep the palette small and contrast legible. Styling never compensates for unclear topology, density, or vague labels; avoid experimental types and per-diagram config unless the target renderer is known to support them.

## Common Mermaid pitfalls

Check these before treating a fence as done — each has caused a real parse failure or silently wrong render:

- A semicolon inside label/message text ends the statement early (`C->>K: deliver; cache it` fails). Use a comma, en dash, or split into two edges.
- `**bold**` and other Markdown emphasis do not render inside default node labels — the asterisks show literally. Use structure (short label, dedicated node, prose heading) instead.
- `\n` does not break a line inside a quoted label. Use `<br/>` inside `["..."]`/`{"..."}`-style labels.
- Quote any label containing `(`, `)`, `#`, `;`, or a line break. A bare middot/em dash alone doesn't need quoting, but quote the whole label once it has other special characters.
- A double colon (`::`) inside a `stateDiagram-v2` transition label breaks parsing even when quoted (collides with `:::`/`::` class-assignment syntax). Flowchart labels don't have this problem. Write `Poll.Ready` or `Poll is Ready`, not `Poll::Ready`.
- `~~~` is the layout-only invisible link (`A ~~~ B`) — for forcing visual order without implying a relationship. Never use a dashed/plain edge for this; a rendered arrow always claims a relationship to explain.
- A subgraph is a container, not a node, for most edges — except the single summary edge between two subgraph IDs (`groupA --> groupB`). Reserve that for "this whole cluster relates to that whole cluster"; connect to an inner node for anything more specific.

## Review checklist

Before merging a diagram change, verify:

- The diagram is an inline fenced `mermaid` block in the Markdown document — not a separate diagram source file.
- The selected type matches the claim being made.
- Directions, cardinalities, and labels match current behavior and constraints.
- The reader can scan one dominant flow without following crossing edges.
- Subgraphs represent real containment/ownership, with concise labels.
- Colors, shapes, and arrows carry consistent, documented meaning.
- The fence renders without a parser error (spot-check with `mermaid-cli` or a Mermaid-aware preview).

**REFERENCE:** Load `references/diagram-type-table.md` for the full type-by-purpose catalog (including experimental/`-beta` types) when the four common types above don't fit the claim, or when checking whether a less common type is supported upstream.
