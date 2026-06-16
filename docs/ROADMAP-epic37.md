# Roadmap — Epic 37: scrolling & scrollbars

Turns the clip-only `overflow` model into a scroll model: `overflow:scroll`/`auto`
with a painted overlay scrollbar, a scroll offset (JS `scrollTop`/`scrollLeft`
that translates the clipped content), and scrollbar styling + scroll-snap. The
engine is one-shot, so the scroll position is whatever JS leaves at snapshot time.

Same per-milestone pipeline. Additive: a page with no scroll containers renders
byte-identically (overlay scrollbars + no layout/gutter change).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E37-M1** | **`overflow:scroll`/`auto` + scrollbar paint**: add the values (clip like `hidden`); paint an overlay vertical scrollbar (track + ratio-sized thumb) at the padding-box right edge — always for `scroll`, when overflowing for `auto`. No gutter (overlay) → layout byte-identical. | `style`, `paint` | a fixed-height `overflow:scroll` box clips its content and shows a track+thumb; `auto` shows it only when overflowing; `hidden` shows none (tested + visual) | ✅ |
| **E37-M2** | **Scroll offset**: JS `element.scrollTop`/`scrollLeft` getters/setters store a per-element scroll offset; the painter translates the box's clipped content by `-scrollTop`/`-scrollLeft` and positions the scrollbar thumb accordingly. | `dom`, `js`, `paint` | setting `scrollTop` in a script scrolls the clipped content (a lower line becomes visible) and moves the thumb down (tested + visual) | ✅ |
| **E37-M3** | **Scrollbar styling + scroll-snap**: `scrollbar-width` (`auto`/`thin`/`none`) and `scrollbar-color` (track+thumb) style the overlay scrollbar; parse `scroll-snap-type`/`scroll-snap-align` (+ `scroll-behavior`) — parsed/stored, snap geometry deferred. | `css`, `style`, `paint` | `scrollbar-width:none` hides the scrollbar; `scrollbar-color: red blue` recolors thumb/track; snap properties parse without error (tested) | ✅ |

## Non-goals (deferred)

- Horizontal scrollbar paint (vertical only in M1; `scrollLeft` still stored).
- Gutter reservation (`scrollbar-gutter`), classic vs overlay scrollbar layout
  effects, and scrollbar buttons/arrows.
- Actual scroll-snap geometry/positioning, `scroll-padding`/`scroll-margin`,
  scroll-driven animations, and smooth `scroll-behavior` animation.
- `::-webkit-scrollbar` pseudo-elements.
