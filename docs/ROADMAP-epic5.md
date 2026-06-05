# Roadmap — Epic 5: CSS Grid & Transforms

Adds CSS Grid layout and 2D transforms. Same per-milestone agent pipeline (design →
analysis → implementation → verification → review), each landing as its own commit + push.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **E5-M1** | Grid core: `display:grid` (+ `inline-grid`); explicit tracks `grid-template-columns`/`grid-template-rows` with `px`/`%`/`fr`/`auto`/`repeat()`; `gap`/`row-gap`/`column-gap`; line-based item placement `grid-column`/`grid-row` (`N`, `N / M`, `span N`); row-major auto-placement of unplaced items into the explicit grid (+ implicit rows). | `style`, `layout` | A grid container sizes its tracks (incl. `fr` distribution) and places explicit + auto items at the right cells (tested + visual) |
| **E5-M2** | Grid alignment + named areas: `justify-items`/`align-items`/`justify-self`/`align-self` (start/end/center/stretch), `justify-content`/`align-content` (track distribution incl. space-*), `grid-template-areas` + `grid-area` name placement, `span` to a named/line. | `style`, `layout` | Items align within their cells, content distributes extra space, and `grid-template-areas` places named items (tested + visual) |
| **E5-M3** | 2D transforms: `transform` (`translate(X,Y)`/`translateX/Y`, `scale`/`scaleX/Y`, `rotate`, `skew`/`skewX/Y`, `matrix(a,b,c,d,e,f)`; multiple functions composed) + `transform-origin`. Painted via a matrix applied to the element's subtree (an offscreen layer drawn back with the matrix); transforms don't affect layout, only paint. | `style`, `paint` | A translated/scaled/rotated box paints transformed about its origin (tested + visual) |

## Non-goals (deferred)

- Grid: `minmax()`/`min-content`/`max-content`/`fit-content()` intrinsic precision (a
  pragmatic approximation is fine), subgrid, `grid-auto-flow: column`/`dense` beyond a
  simple form, `auto-fill`/`auto-fit` in `repeat()` (note if cheap), masonry, baseline
  alignment.
- Transforms: 3D (`translateZ`/`rotateX`/`perspective`/`matrix3d`), `transform-box`,
  transform affecting hit-testing/stacking subtleties beyond paint order, `will-change`,
  transform on table parts.
- Transitions/animations (no timeline in a one-shot renderer), `:hover`/interaction.
