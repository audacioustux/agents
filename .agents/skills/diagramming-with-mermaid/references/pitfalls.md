# Common Mermaid Pitfalls

Syntax traps that cause parse failures or silently wrong renders.

Check these before treating a fence as done — each has caused a real parse failure or silently wrong render:

- A semicolon inside label/message text ends the statement early (`C->>K: deliver; cache it` fails). Use a comma, en dash, or split into two edges.
- `**bold**` and other Markdown emphasis do not render inside default node labels — the asterisks show literally. Use structure (short label, dedicated node, prose heading) instead.
- `\n` does not break a line inside a quoted label. Use `<br/>` inside `["..."]`/`{"..."}`-style labels.
- Quote any label containing `(`, `)`, `#`, `;`, or a line break. A bare middot/em dash alone doesn't need quoting, but quote the whole label once it has other special characters.
- A double colon (`::`) inside a `stateDiagram-v2` transition label breaks parsing even when quoted (collides with `:::`/`::` class-assignment syntax). Flowchart labels don't have this problem. Write `Poll.Ready` or `Poll is Ready`, not `Poll::Ready`.
- `~~~` is the layout-only invisible link (`A ~~~ B`) — for forcing visual order without implying a relationship. Never use a dashed/plain edge for this; a rendered arrow always claims a relationship to explain.
- A subgraph is a container, not a node, for most edges — except the single summary edge between two subgraph IDs (`groupA --> groupB`). Reserve that for "this whole cluster relates to that whole cluster"; connect to an inner node for anything more specific.
