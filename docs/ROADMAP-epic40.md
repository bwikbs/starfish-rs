# Roadmap — Epic 40: tables round 2

Deepens the table model (E7-M3: display:table/row/cell, occupancy grid +
colspan/rowspan, separate-border model with border-spacing) with the collapsed
border model, fixed table layout, caption placement, and column groups.

Same per-milestone pipeline. Additive: tables not using these features render
byte-identically (golden + existing table tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E40-M1** | **`border-collapse: collapse`**: in the collapsed model there is no `border-spacing` (cells abut) and adjacent cell/table borders share a single edge (MVP: zero inter-cell spacing + cells drawn edge-to-edge; full border-conflict resolution simplified to "wider/table border wins" or just overlapping draw). | `layout` | a `border-collapse:collapse` table has no gaps between cells (border-spacing ignored) where the separate model would; separate is byte-identical (tested + visual) | ✅ |
| **E40-M2** | **`table-layout: fixed`**: column widths are taken from the first row's cell widths / explicit `<col>`/cell `width` (content does not widen columns); remaining table width distributed across auto columns. | `style`, `layout` | a `table-layout:fixed` table with explicit first-row widths keeps those column widths regardless of cell content length (tested + visual) | ☑ |
| **E40-M3** | **`<caption>` + `<col>`/`<colgroup>`**: a `<caption>` lays out above (or below for `caption-side:bottom`) the table box; `<col>`/`<colgroup>` `width` (and `span`) set column widths. | `style`, `layout` | a `<caption>` renders above the table; `<col width>` sets a column's width (tested + visual) | ☐ |

## Non-goals (deferred)

- Full collapsed-border conflict resolution (style/width/color precedence per
  CSS 2.1 §17.6.2) and per-edge half-border geometry; MVP collapses spacing and
  draws cells edge-to-edge.
- `table-layout:fixed` with `<colgroup span>` spanning + percentage column widths
  beyond a basic distribution; `visibility:collapse` on rows/columns.
- `caption-side: inline-start/inline-end`, multiple captions, and table
  `border-spacing` interaction with collapsed borders.
