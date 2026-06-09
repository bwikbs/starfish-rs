# Roadmap — Epic 18: layout expansion, round 3

A third layout pass covering the box-model + flow features still missing:
`aspect-ratio`, flexbox `gap`, CSS **multi-column** layout, and **vertical
writing modes** (`writing-mode` + `text-orientation`). These round out the common
modern-layout surface the engine doesn't yet handle.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive,
so pages not using it must stay byte-identical (existing tests + the golden PNG
unchanged).

Current state (reference): block/inline/flex/grid/table/float/position layout with
`box-sizing`, `min/max`, `calc`, `@media`, overflow clipping. **Grid** supports
`gap`/`row-gap`/`column-gap`, but **flexbox does NOT** (flex items pack with no
gap). There is no `aspect-ratio` (a box with one auto dimension never derives the
other from a ratio). No multi-column (`column-*` properties are dropped). All text
is horizontal: `writing-mode` is unsupported (no vertical flow, no
`text-orientation`); the inline axis is always horizontal-tb.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E18-M1** | **aspect-ratio + flex gap**: `aspect-ratio: <w>/<h>` (and `<number>`) — when a box has a definite size on one axis and `auto` on the other (and no other constraint), derive the other from the ratio; applies to block boxes and replaced/flex items, respecting `min/max`. Flexbox `gap`/`row-gap`/`column-gap` — fixed spacing between flex items along the main axis (and between wrapped lines on the cross axis), mirroring the existing grid gap. | `style`, `layout` | A box with `aspect-ratio:16/9` + a definite width lays out at the derived height; flex items with `gap:10px` are spaced 10px apart (tested + visual) | ☐ |
| **E18-M2** | **Multi-column layout**: `column-count` / `column-width` (and the resolved used column count/width per CSS), `column-gap`, `column-rule` (`-width`/`-style`/`-color`, drawn in the gaps); fragment a block container's in-flow content into N balanced-ish columns laid out left-to-right within the element's content box (height grows to the tallest column). MVP: block-level children flowed column by column (no mid-element break optimization, no `break-inside`/`break-before` control beyond keeping a block whole). | `style`, `layout`, `paint` | A multi-column container splits its blocks across `column-count` columns with gaps + a rule between them (tested + visual) | ☐ |
| **E18-M3** | **Vertical writing modes**: `writing-mode` (`horizontal-tb` default, `vertical-rl`, `vertical-lr`) so the block axis runs horizontally and the inline axis vertically — block boxes stack left/right, lines run top-to-bottom, text advances vertically; `text-orientation` (`mixed`/`upright`/`sideways`) controlling glyph orientation in vertical text. MVP: block flow + inline text in the vertical axis with `text-orientation: sideways` (rotated runs) and `upright` (stacked CJK-style); logical-vs-physical kept minimal (map to physical at layout time). | `style`, `layout`, `paint` | A `writing-mode: vertical-rl` block stacks its lines top-to-bottom advancing right-to-left, with `text-orientation` controlling glyph rotation (tested + visual) | ☐ |

## Non-goals (deferred)

- `aspect-ratio` interaction with intrinsic-aspect replaced content beyond the
  basic "derive the missing axis" case; `aspect-ratio` on grid/table items'
  complex sizing; the `auto <ratio>` two-value form's full fallback rules.
- Flexbox `gap` percentage resolution edge cases; `gap` on block/inline contexts
  (only flex + the existing grid).
- Multi-column: `column-span: all`, `break-before`/`break-after`/`break-inside`
  full fragmentation, balancing to exactly-equal heights, column-fill `auto` vs
  `balance` precision, spanning floats, nested multicol, widows/orphans.
- `writing-mode`: `sideways-lr`/`sideways-rl` as distinct from `vertical-*`,
  full logical properties (`margin-block`/`inline-*`, `inset-block`), bidi in
  vertical text, vertical metrics (true vertical font advances / `vert` GSUB),
  ruby, and `text-combine-upright`.
- `direction: rtl` interaction with vertical modes beyond the basic mapping.
