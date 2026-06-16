# Roadmap — Epic 50: CSS Grid round 3 (minmax / intrinsic / auto-repeat)

Deepens grid track sizing (E5 grid core: px/%/fr/auto + fixed `repeat()`; E31
subgrid) with `minmax()`, intrinsic track keywords + `fit-content()`,
`repeat(auto-fill|auto-fit)`, and `grid-auto-flow: dense`.

Same per-milestone pipeline. Additive: grids using only the existing track types
lay out byte-identically (golden + existing grid tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E50-M1** | **`minmax(min, max)`**: a `TrackSize::MinMax(MinMaxSize, MinMaxSize)` (sub-sizes px/%/fr/auto, kept `Copy`); the track is floored at `min` and grows toward `max` (fr/auto max participates in free-space distribution, fixed max clamps). | `css`, `style`, `layout` | `grid-template-columns: minmax(100px, 1fr) 1fr` keeps the first column ≥100px while sharing the rest; minmax(50px,80px) clamps to that range (tested + visual) | ✅ |
| **E50-M2** | **Intrinsic tracks + `fit-content()`**: `min-content`/`max-content` track keywords (sized to the column's item content) and `fit-content(<len>)` (clamp the max-content to the length). | `css`, `style`, `layout` | a `max-content` column is as wide as its widest item; fit-content(60px) caps it at 60px (tested + visual) | ✅ |
| **E50-M3** | **`repeat(auto-fill/auto-fit)` + `grid-auto-flow: dense`**: `repeat(auto-fill, <track>)` generates as many tracks as fit the container; `auto-fit` collapses empty trailing tracks; `grid-auto-flow: dense` backfills earlier holes. | `css`, `style`, `layout` | `repeat(auto-fill, 100px)` in a 350px grid makes 3 columns; `dense` packs a later small item into an earlier gap (tested + visual) | ✅ |

## Non-goals (deferred)

- `minmax()`/`fit-content()` with `min-content`/`max-content` as the fr-flexible
  bound beyond a reasonable approximation; the full CSS grid track-sizing
  algorithm's growth-limit/free-space iteration (MVP single-pass distribution).
- `repeat(auto-fit)` exact collapsed-gutter geometry, and `auto-fill` with a
  mixed track list `repeat(auto-fill, A B)` beyond a single track pattern.
- Named grid lines / `grid-template-areas` interaction with auto-repeat.
