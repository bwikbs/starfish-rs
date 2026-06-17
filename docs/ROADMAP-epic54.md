# Roadmap — Epic 54: z-index & stacking contexts

Fixes a real correctness gap: the painter currently emits positioned boxes in
TREE order only (`build_display_list`'s in-flow → float → positioned passes,
`display.rs`), ignoring `z-index`. This epic adds `z-index`, stacking-context
establishment, and the CSS 2.1 stacking order.

Same per-milestone pipeline. Additive: a page with no `z-index` and no
context-establishing properties paints in the same tree order as today
(byte-identical; golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E54-M1** | **`z-index` + positioned sort**: parse `z-index: auto \| <integer>` (NOT inherited); the painter sorts the positioned bucket by `(z-index, tree-order)` so a higher z-index positioned box paints on top and a negative one behind (stable for ties = tree order). | `style`, `paint` | two overlapping `position:absolute` boxes paint in z-index order regardless of source order; equal/auto keeps tree order (byte-identical) (tested + visual) | ✅ |
| **E54-M2** | **Stacking contexts**: an element establishes a stacking context when it is positioned with `z-index != auto`, or has `opacity < 1` / a `transform` / a `filter` / `isolation: isolate` / `mix-blend-mode`. A context's positioned descendants are sorted+painted WITHIN it (a child's z-index can't escape its parent context). | `style`, `paint` | a high-z child inside a low-z positioned parent does NOT paint above a sibling of the parent (it's confined to the parent's context) (tested + visual) | ☐ |
| **E54-M3** | **CSS 2.1 stacking order**: within a stacking context paint in the 7 layers — backgrounds/borders, negative-z children, in-flow blocks, floats, in-flow inlines, z:auto/0 positioned, positive-z children — so a `z-index:-1` child paints behind its parent's in-flow content. | `paint` | a `z-index:-1` positioned child renders behind the parent's text/background; a `z-index:1` one in front (tested + visual) | ☐ |

## Non-goals (deferred)

- `isolation`/opacity already force offscreen layers (E21/E32); M2 reuses that —
  no second compositing pass. `will-change`-established contexts beyond the
  listed triggers.
- Exact interleaving of multiple same-z positioned boxes vs floats beyond the
  7-layer model, and stacking across shadow-tree boundaries.
- `z-index` on flex/grid items (treated as positioned-like only when actually
  positioned; flex/grid item painting order is left as tree order MVP).
