# Roadmap — Epic 7: CSS coverage (selectors, generated content, tables)

Broadens CSS support: richer selectors, `::before`/`::after` generated content, and table
layout. Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **E7-M1** | Selector expansion: attribute selectors (`[a]`, `[a=v]`, `~=`, `\|=`, `^=`, `$=`, `*=`, case `i`), structural pseudo-classes (`:first-child`, `:last-child`, `:only-child`, `:nth-child(An+B)`, `:nth-of-type`, `:root`, `:empty`, `:not(simple)`), and the sibling combinators `+` / `~`. Wired into both `querySelector` (js) and the cascade matcher (style). | `css`, `style`, `js` | The new selectors parse and match correctly in the cascade and querySelector (tested + visual) |
| **E7-M2** | Generated content: `::before` / `::after` pseudo-elements with the `content` property (strings, `attr()`, basic), laid out as inline/box children of the originating element, styled by the matching pseudo-element rule. | `css`, `style`, `layout` | `div::before { content: "x" }` inserts a styled generated box before the element's content (tested + visual) |
| **E7-M3** | Table layout: `display: table` / `table-row` / `table-cell` (+ `inline-table`, the anonymous table-fixup basics), a simple fixed/auto column-width table algorithm, `border-collapse: separate` with `border-spacing`, cell `colspan`/`rowspan` (basic). | `style`, `layout` | A `<table>` (or display:table) lays out rows/cells in a grid of columns with borders (tested + visual) |

## Non-goals (deferred)

- `:hover`/`:focus`/`:active` and other interaction pseudo-classes (no live input in a
  one-shot renderer — they simply never match), `:nth-last-child`, `:nth-of-type(An+B)`
  edge cases beyond the common forms, `:is()`/`:where()`/`:has()`, `::marker`/`::first-line`/
  `::first-letter`, `::selection`.
- `content` beyond strings + `attr()` (no `counter()`, `url()` images in content, quotes).
- `border-collapse: collapse` (only `separate` for M3), table captions/col/colgroup
  advanced sizing, `table-layout: fixed` precision, vertical-align in cells beyond top,
  percentage/auto table-width subtleties.
- CSS transitions/animations (no timeline).
