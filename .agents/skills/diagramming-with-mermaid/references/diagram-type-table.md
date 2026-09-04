# Mermaid Diagram Type Catalog

Point-in-time snapshot of upstream Mermaid diagram types and when to use each. Verify the target Markdown host's bundled Mermaid version supports a chosen type — especially anything marked experimental/`-beta` — before relying on it. Consult the [full syntax reference](https://mermaid.js.org/intro/syntax-reference.html) for status changes or any type not listed here; it is authoritative and this table drifts as Mermaid evolves.

| Type | Use it for | Start with |
|------|------------|------------|
| [Flowchart](https://mermaid.js.org/syntax/flowchart.html) | Architecture, topology, process, data, or decision flow | `flowchart LR` / `flowchart TB` |
| [Sequence diagram](https://mermaid.js.org/syntax/sequenceDiagram.html) | Ordered interactions, requests, replies, retries, handoffs | `sequenceDiagram` |
| [State diagram](https://mermaid.js.org/syntax/stateDiagram.html) | Finite states, allowed transitions, terminal states | `stateDiagram-v2` |
| [Entity-relationship diagram](https://mermaid.js.org/syntax/entityRelationshipDiagram.html) | Persisted entities, cardinality, required fields | `erDiagram` |
| [Class diagram](https://mermaid.js.org/syntax/classDiagram.html) | Object models, interfaces, inheritance | `classDiagram` |
| [Gantt chart](https://mermaid.js.org/syntax/gantt.html) | Schedules and delivery timelines | `gantt` |
| [Pie chart](https://mermaid.js.org/syntax/pie.html) | Simple proportion of a whole | `pie` |
| [Git graph](https://mermaid.js.org/syntax/gitgraph.html) | Branch, merge, and release history | `gitGraph` |
| [User journey](https://mermaid.js.org/syntax/userJourney.html) | End-to-end user experience with satisfaction scoring | `journey` |
| [Requirement diagram](https://mermaid.js.org/syntax/requirementDiagram.html) | Formal requirement-to-element traceability (SysML) | `requirementDiagram` |
| [Quadrant chart](https://mermaid.js.org/syntax/quadrantChart.html) | Plotting items against two independent axes | `quadrantChart` |
| [Kanban](https://mermaid.js.org/syntax/kanban.html) | Tasks moving through workflow columns | `kanban` |
| [Sankey diagram (experimental upstream)](https://mermaid.js.org/syntax/sankey.html) | Proportional flow volumes between stages | `sankey-beta` |
| [Mindmap (experimental upstream: icon integration only)](https://mermaid.js.org/syntax/mindmap.html) | Topic hierarchies and brainstorming | `mindmap` |
| [Timeline (experimental upstream: icon integration only)](https://mermaid.js.org/syntax/timeline.html) | Chronological events without swimlanes | `timeline` |
| [C4 diagram (experimental upstream)](https://mermaid.js.org/syntax/c4.html) | Formal C4 context/container/component/dynamic/deployment views | `C4Context` |
| [Block diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/block.html) | Compact block layouts with author-controlled placement and spans | `block-beta` |
| [Packet diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/packet.html) | Byte/bit-level protocol layouts | `packet-beta` |
| [Architecture diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/architecture.html) | Cloud/service infrastructure with icon-aware nodes and groups | `architecture-beta` |
| [XY chart (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/xyChart.html) | Bar/line charts for two numeric variables | `xychart-beta` |
| [Radar chart (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/radar.html) | Comparing entities across multiple dimensions | `radar-beta` |
| [Treemap (experimental upstream, new type)](https://mermaid.js.org/syntax/treemap.html) | Nested proportional hierarchy | `treemap-beta` |
| [Swimlane diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/swimlanes.html) | Process steps grouped by owning actor/team/system | `swimlane-beta` |
| [TreeView (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/treeView.html) | Directory/file-tree-style hierarchy | `treeView-beta` |
| [Venn diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/venn.html) | Overlapping-set relationships | `venn-beta` |
| [Ishikawa / fishbone diagram (experimental upstream, new type)](https://mermaid.js.org/syntax/ishikawa.html) | Root-cause analysis of one event | `ishikawa-beta` |
| [Wardley map (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/wardley.html) | Value-chain strategy mapping by visibility/evolution | `wardley-beta` |
| [Cynefin framework (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/cynefin.html) | Categorizing problems into complexity domains | `cynefin-beta` |
| [Event modeling diagram (new type)](https://mermaid.js.org/syntax/eventmodeling.html) | Information/event flow over time in swimlanes | `eventmodeling` |
| [Railroad diagram (experimental upstream, `-beta`)](https://mermaid.js.org/syntax/railroad.html) | Grammar/syntax diagrams (EBNF, ABNF, PEG) | `railroad-ebnf-beta` (or `-abnf-`/`-peg-`/plain `-beta`) |
| [ZenUML (alternate sequence-diagram renderer)](https://mermaid.js.org/syntax/zenuml.html) | Sequence diagrams in ZenUML's alternate syntax | `zenuml` |
