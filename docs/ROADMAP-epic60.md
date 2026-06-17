# Roadmap — Epic 60: scroll round 2 (gutter / scroll-padding / snap geometry)

Extends scrolling (E37 overflow:scroll/auto + scrollbar + scrollTop + scroll-snap
PARSING): `scrollbar-gutter` space reservation, `scroll-padding`/`scroll-margin`,
and actual scroll-snap geometry (snap the offset to an aligned child).

Same per-milestone pipeline. Additive: pages without these render
byte-identically (golden + existing scroll tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E60-M1** | **`scrollbar-gutter`**: `auto` (default) reserves nothing; `stable` reserves a scrollbar-width gutter on the inline-end edge even when not overflowing; `stable both-edges` reserves on both — narrowing the content box accordingly. | `style`, `layout` | a `scrollbar-gutter:stable` box with `overflow:auto` reserves ~SCROLLBAR_WIDTH so its content is narrower than a default box; auto is byte-identical (tested + visual) | ✅ |
| **E60-M2** | **`scroll-padding` + `scroll-margin`**: the 4-side longhands + shorthands (lengths/percentages) parsed and stored on the style (used by snap in M3). | `css`, `style` | `scroll-padding: 10px 20px` and `scroll-margin-top: 8px` parse to the per-side values (tested) | ✅ |
| **E60-M3** | **scroll-snap geometry**: a scroll container with `scroll-snap-type` snaps its scroll offset so the nearest child with `scroll-snap-align` aligns to the snapport edge (start/center/end), honoring `scroll-padding`/`scroll-margin`. One-shot: snap the resolved scroll offset (default 0 → first snap target). | `layout`/`paint` | a vertical snap container snaps its content so the first `scroll-snap-align:start` child's top aligns to the scrollport top (tested + visual) | ☐ |

## Non-goals (deferred)

- `scrollbar-gutter` interaction with classic (non-overlay) scrollbars beyond the
  reservation; the gutter only applies when the box is a scroll container
  (overflow scroll/auto/hidden).
- Snapping to the NEAREST target relative to a non-zero current scroll position
  beyond the MVP (one-shot defaults to the first/closest snap target); `proximity`
  vs `mandatory` strictness nuance (treated as mandatory MVP).
- `scroll-snap-stop`, `scroll-padding`/`-margin` logical (block/inline) longhands
  beyond the physical sides.
