# Roadmap — Epic 16: CSS coverage, round 2

A second broad CSS pass: the logical-OR/relational selectors, CSS counters, real
background images (url + sizing/positioning/repeat + multiple layers), radial &
conic gradients, text-shadow, outline, text-overflow, and `position: sticky`.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Where a feature is purely
additive (a new selector form, a new property), pages not using it must stay
byte-identical (existing tests + the golden PNG unchanged).

Current state (reference): selectors support type/id/class/attribute, structural
+ state pseudo-classes, `:not(simple)`, and `+`/`~`/descendant/child combinators —
but NOT `:is()`/`:where()`/`:has()`. `content` supports strings + `attr()` but not
`counter()`. Backgrounds are a single `Background::{Color, Gradient(LinearGradient)}`
(one linear-gradient, no url images, no layers, no radial/conic). No `text-shadow`,
`outline`, `text-overflow`, or `position: sticky`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E16-M1** | **Selectors + counters**: `:is(<list>)` / `:where(<list>)` (matches if any selector in the list matches; `:where` has 0 specificity) and `:has(<relative-list>)` (matches if a descendant/sibling matches — the relational pseudo); CSS **counters** — `counter-reset` / `counter-increment` tracked through the tree, and `counter(name[,style])` / `counters(name,sep[,style])` in `content` (incl. `::before`/`::after` + `list-style-type` numbering basics). | `css`, `style` | `:is()`/`:where()`/`:has()` match + cascade with the right specificity; an ordered counter renders `1. 2. 3.` via `counter()` in generated content (tested + visual) | ☐ |
| **E16-M2** | **Background images & layers**: `background-image: url(...)` (raster, via the image pipeline) painted under the content; `background-size` (`auto`/`cover`/`contain`/lengths), `background-position`, `background-repeat` (`repeat`/`no-repeat`/`repeat-x`/`-y`); **multiple background layers** (comma-separated images/gradients, painted back-to-front) — restructure `Background` into a layer list. | `style`, `paint` | A `url()` background tiles/sizes/positions correctly, multiple layers stack, gradients still work (tested + visual) | ☐ |
| **E16-M3** | **Gradients & shadows**: `radial-gradient(...)` and `conic-gradient(...)` as background images (reusing the tiny-skia radial/SVG infra); `text-shadow` (offset + blur + color, painted under the glyphs); `outline` (`outline-width`/`-style`/`-color`/`-offset`, drawn outside the border box, not affecting layout). | `css`, `style`, `paint` | A radial/conic-gradient background renders, text has a drop shadow, an element shows an outline (tested + visual) | ☐ |
| **E16-M4** | **Overflow text & sticky**: `text-overflow: ellipsis` (a single-line clipped run ending in `…` when it overflows, with `overflow: hidden`/`clip` + `white-space: nowrap`); `position: sticky` (sticks within its containing block per `top`/`bottom`/`left`/`right` — in a one-shot render with no scroll it resolves to its in-flow position, sticking only if already scrolled past; document the no-scroll behavior). | `style`, `layout`, `paint` | An overflowing nowrap line shows `…`; a sticky element lays out at its sticky/in-flow position (tested + visual) | ☐ |

## Non-goals (deferred)

- `:nth-last-child`/`:nth-of-type(An+B)` edge cases, `:is()`/`:has()` performance
  (no fast-path indexing), forgiving vs strict parsing subtleties, `:has()` beyond
  descendant/`+`/`~`/child relative selectors.
- `counter-set`, `@counter-style`, named/styled counters beyond decimal/roman/alpha,
  CSS nesting, `counter()` outside `content`.
- `background-attachment: fixed`, `background-origin`/`-clip` (paint to the border
  box only), `background-blend-mode`, `image-set()`, `background` shorthand edge
  cases beyond the common forms.
- `repeating-linear/radial/conic-gradient`, gradient color-interpolation hints/
  color spaces, `conic-gradient` `from`/`at` full syntax beyond the common forms.
- Real scroll / scrollable overflow + a scroll offset for `sticky` (one-shot render
  has no viewport scroll); `position: sticky` table-header stickiness edge cases.
- `text-overflow` per-side / a custom string value / multi-line line-clamp (Epic
  candidate "typography"); `text-shadow` multiple comma layers beyond the first 1-2.
- `outline` following a non-rectangular shape (`outline` is drawn as a rectangle
  around the border box); `outline` on inline/split boxes precision.
