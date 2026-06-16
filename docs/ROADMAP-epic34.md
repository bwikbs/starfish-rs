# Roadmap — Epic 34: display model completeness

Fills the box-generation gaps left by the legacy display set: `display: contents`
(element generates no box; children promote into the parent flow — also the
correct UA value for `<slot>`, fixing block content slotted into an inline slot),
`display: flow-root` (a clean block formatting context / clearfix), and the CSS
`display` two-value syntax. Builds on the box-tree builder (`crates/layout`
`build_children`/`build_node`) and the float/BFC machinery (`block.rs`
`FloatContext`).

Same per-milestone pipeline. Additive: pages not using the new values render
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E34-M1** | **`display: contents`**: a `Display::Contents` value; the element generates no box but its children are spliced into the parent's box flow (inheritance still flows through the element's computed style). UA rule `slot { display: contents }` so slotted block content lays out instead of being trapped in an inline slot box. | `style`, `layout` | a `display:contents` wrapper around two blocks lays them out as direct siblings (no wrapper box); a shadow `<slot>` with block slotted content lays out correctly (tested + visual) | ✅ |
| **E34-M2** | **`display: flow-root`**: a `Display::FlowRoot` that lays out as a block container which always establishes a new block formatting context (contains its floats, no margin-collapse through its boundary). | `style`, `layout` | a `flow-root` box fully contains a tall floated child (its border-box grows to enclose the float) where a plain block would not (tested + visual) | ✅ |
| **E34-M3** | **`display` two-value syntax**: parse `display: <outer> <inner>` (e.g. `inline flow-root`, `block flow`) and map to the legacy single value; `display: flow`/`run-in`/unknown degrade gracefully. | `style` | `display: inline flow-root` computes to inline-block-like (atomic inline BFC), `block flow` to block, and a bogus value leaves the inherited/initial display (tested) | ✅ |

## Non-goals (deferred)

- `display: contents` on replaced elements / form controls (treated as their
  normal box), and `::before`/`::after` on a `contents` element.
- `run-in` actual run-in behavior (graceful fallback to block only).
- `display: list-item` as a second value combo; `inline list-item`.
