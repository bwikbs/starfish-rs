# Roadmap — Epic 31: layout deepening (subgrid, masonry, containment)

Pushes the grid/containment layer past the Epic 5 baseline: **subgrid** (a nested
grid adopting its parent's tracks), **masonry** layout (the experimental
`grid-template-*: masonry` packing), and **containment** (`contain` +
`content-visibility`, which let a subtree be skipped or isolated). All build on
the existing grid track resolver (crates/layout `grid.rs`).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a page using only the existing grid/block layout must render byte-identically
(existing tests + the golden PNG unchanged).

Current state (reference): CSS Grid (E5) resolves `grid-template-columns/rows`
(px/%/fr/auto, `repeat()`), places items by line/area, and aligns via
`justify`/`align`-*. Tracks are a `Vec<TrackSize>` { Px, Percent, Fr, Auto }.
There is **no** `subgrid` (the keyword is dropped → the track list is empty →
auto), no `masonry`, and `contain`/`content-visibility` are unknown properties
(ignored — a subtree always lays out and paints).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E31-M1** | **Containment**: `content-visibility: hidden` (skip the subtree's layout + paint, treating its size as 0) / `auto` (treated as visible here); `contain: size` (the element's intrinsic size doesn't depend on its contents — used 0 unless an explicit size is set) and `contain: layout`/`paint` (accepted, establishing an independent formatting context / clip, no extra visual change beyond existing overflow clipping). Implemented first as the self-contained, single-element case. | `css`, `style`, `layout` | `content-visibility: hidden` makes a block contribute 0 height and paints nothing; `contain: size` on an auto-height block with no explicit height collapses it to 0 (tested + visual) | ☐ |
| **E31-M2** | **Masonry**: `grid-template-rows: masonry` (or `-columns`) — the non-masonry axis defines tracks normally; on the masonry axis items are placed into whichever track has the smallest running size so far (shortest-column packing), flowing in source order. `gap` applies between packed items. Self-contained within `layout_grid`. | `css`, `style`, `layout` | with `grid-template-columns: repeat(3, 1fr); grid-template-rows: masonry`, items pack into the 3 columns by shortest-column-first (a tall first item shifts later items to the other columns) (tested + visual) | ☐ |
| **E31-M3** | **Subgrid**: `grid-template-columns: subgrid` / `grid-template-rows: subgrid` on a grid item that is itself a grid. The subgridded axis adopts the parent grid's track sizes spanned by the item's placement (instead of defining its own tracks), so the child's items align to the parent's grid lines. One level deep; the gap is inherited from the parent on the subgridded axis. (Hardest — needs parent→child track threading, hence last.) | `css`, `style`, `layout` | a `display:grid; grid-template-columns: subgrid` child spanning 3 parent columns lays its own items out on those 3 parent column tracks (their widths), not equal auto tracks (tested + visual) | ☐ |

## Non-goals (deferred)

- Subgrid more than one level deep, subgrid on both axes simultaneously beyond
  the MVP track-adoption, named grid lines inherited through subgrid, and the
  subgrid `gap` override (`gap` on the subgrid that differs from the parent).
- `masonry` with explicit item placement / spanning, `masonry-auto-flow`,
  `align-tracks`/`justify-tracks`, and the masonry track sizing that depends on
  the natural item heights' interaction with `fr` on the masonry axis.
- `content-visibility: auto`'s real visibility-based skipping (it would need a
  viewport-intersection model); it is treated as always-visible. No
  `contain-intrinsic-size` placeholder sizing.
- `contain: strict`/`content`/`inline-size` shorthands' full isolation
  semantics (paint containment beyond clip, style containment, counter/quote
  scoping); only the size-contribution + clip effects are modeled.
- `container-type` already establishes containment for queries (E25); this epic
  is the `contain`/`content-visibility` properties, not query containers.
