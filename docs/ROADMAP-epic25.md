# Roadmap — Epic 25: container queries & logical properties

The responsive-component layer that sits on top of Epic 24's design-system
functions: **container queries** (`@container` size queries + `cqw`/`cqh`/`cqi`/
`cqb` units), **CSS logical properties** (`margin-inline`/`padding-block`/
`inset-*`/`inline-size`/… mapped to physical sides through the element's
writing-mode + direction), and a **box-alignment round-out** (the `place-*`
shorthands + the remaining `justify-*`/`align-*` longhands and gap alignment).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
pages not using it must stay byte-identical (existing tests + the golden PNG
unchanged).

Current state (reference): writing-mode (horizontal-tb / vertical-rl / vertical-lr)
ships from E18, so an inline/block axis already exists at layout time; `direction`
is parsed for bidi (E6). `gap` works for grid/flex/multicol; `justify-content` +
`align-items` are honored by flex/grid. There is **no** `@container` (the at-rule
is skipped like any unknown one, so its rules never apply), **no** `cq*` units
(dropped → length falls back), and **no** logical properties (`margin-inline`,
`inset`, `inline-size`, etc. are unknown names, silently ignored). The physical
`margin`/`padding`/`width`/`top`… longhands are the only box-edge surface.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E25-M1** | **Container queries**: `container-type: inline-size \| size \| normal` + `container-name` (and the `container:` shorthand); `@container <name>? (<size-feature>)` blocks — `width`/`height`/`inline-size`/`block-size` `min-`/`max-`/`=` comparisons against the nearest ancestor query container, with `and`/`or`/`not` (reuse the `@media`/`@supports` condition grammar). Container-relative length units `cqw`/`cqh`/`cqi`/`cqb` (`cqmin`/`cqmax`) resolve against that container's content box. Layout evaluates queries after the container's size is known (the container establishes size containment on the queried axis), so a child's `@container` rules cascade from the resolved container dimensions. | `css`, `style`, `layout` | `.card { container-type: inline-size }` + `@container (min-width: 400px) { .title { font-size: 2rem } }` makes `.title` larger only when the card is ≥400px wide; `width: 50cqw` resolves to half the query container's inline size (tested + visual) | ✅ |
| **E25-M2** | **Logical properties**: the flow-relative box properties mapped to physical sides through `writing-mode` + `direction` — `margin-inline`/`margin-block` (+ `-start`/`-end`), the `padding-*` and `border-*-inline/block-*` equivalents, `inset`/`inset-inline`/`inset-block` (+ `-start`/`-end`), and the `inline-size`/`block-size` (+ `min-`/`max-`) sizing keywords; plus `text-align: start \| end`. Each resolves to the existing physical longhand for the element's writing-mode (horizontal-tb → inline=horizontal; vertical-* → inline=vertical), so layout stays physical. | `css`, `style` | In `horizontal-tb` `margin-inline: 10px 20px` sets left=10 right=20; under `writing-mode: vertical-rl` the same sets top/bottom; `inline-size: 200px` sets the horizontal width (h-tb) or height (vertical); `text-align: start` is left in LTR, right in RTL (tested) | ✅ |
| **E25-M3** | **Box-alignment round-out**: the `place-content`/`place-items`/`place-self` shorthands (expand to the align/justify pair) and the remaining longhands — `justify-items`/`justify-self`, `align-content`/`align-self` — honored by flex + grid, plus `align-content`/`justify-content` driving gap distribution (`space-between`/`-around`/`-evenly`) consistently across flex and grid. `place-*: center` etc. centers items on both axes. | `style`, `layout` | `place-items: center` centers grid/flex items on both axes; `justify-self: end` places one grid item at its cell's end; `align-content: space-between` distributes rows with the gap between them (tested + visual) | ✅ |

## Non-goals (deferred)

- `@container style(...)` style queries and `@container scroll-state(...)`;
  container queries on anything but the nearest matching ancestor (no skipping
  by name across multiple containers is fine, but no `@container` on the queried
  element itself — an element can't query its own size).
- Full size containment / `contain: layout size` semantics beyond what's needed
  to make the query container's queried-axis size well-defined; no `content-visibility`.
- `block-size` container queries when the block size depends on the queried
  content (circular) — treated as `0`/unknown (the spec's own restriction).
- Logical properties for non-box surfaces: `*-block`/`*-inline` on
  `overflow`, `overscroll`, `contain-intrinsic-size`, scroll-margin/padding;
  `caption-side`, float `inline-start`/`inline-end`.
- Sideways writing modes (`sideways-rl`/`sideways-lr`) and `text-orientation`'s
  effect on logical mapping (only horizontal-tb + vertical-rl/lr are mapped).
- `justify-tracks`/`align-tracks` (masonry), `place-*` `legacy`/`safe`/`unsafe`
  overflow-alignment keywords, baseline alignment (`first baseline`/`last baseline`).
