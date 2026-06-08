# Roadmap — Epic 13: CSS expansion

Broadens CSS feature coverage with the most-used properties and value syntax the
engine still lacks: the box-model sizing controls (`box-sizing`, min/max width/height),
the value-computation layer (`calc()`, custom properties + `var()`), viewport-conditional
features (`@media` queries + viewport units), and visual coverage (`overflow` clipping,
`hsl()` color, more border styles).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Where a milestone changes only
value resolution or adds opt-in properties, output stays identical for pages that don't
use the new feature — existing tests + the golden PNG must not regress.

Current model (for reference): `Length { Px, Percent, Auto }`; `em`/`rem` are resolved to
`Px` during the cascade (via `EmContext`), so only `Percent`/`Auto` reach layout, where
`block::resolve(len, cb_width)` turns them into used px. Colors: `#hex`, `rgb()/rgba()`,
~16 named. `@media` is currently tokenized but skipped (`skip_at_rule`).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E13-M1** | **Box-model sizing**: `box-sizing: content-box \| border-box` (border-box folds padding+border into the specified width/height); `min-width`/`max-width`/`min-height`/`max-height` (px/%/auto/none) clamping the used main/cross size in block, flex, grid, and replaced-element layout. | `css`, `style`, `layout` | `box-sizing: border-box` makes width include padding+border; min/max clamp the used size; existing geometry unchanged for pages not using them (tested + visual) | ✅ |
| **E13-M2** | **Value computation**: `calc()` over lengths/percentages (`+ - * /`, nesting, mixed px/%/em) anywhere a length is accepted, resolved against the containing block at layout time; CSS **custom properties** (`--name: value`, inherited) + `var(--name, fallback)` substitution during the cascade. | `css`, `style` | `width: calc(100% - 20px)` and a `var(--c)` color resolve correctly; cascade/inheritance of custom props works (tested + visual) | ✅ |
| **E13-M3** | **Viewport-conditional**: `@media` queries (`min-width`/`max-width`/`min-height`/`max-height`/`orientation`, `and`/`,`/`not`) evaluated against the render viewport, their rules applied only when matching; viewport-relative units `vw`/`vh`/`vmin`/`vmax` resolved against the viewport. Viewport size threaded into the cascade. | `css`, `style`, `paint` | A `@media (max-width: …)` block applies at the right viewport width and `width: 50vw` resolves; non-media pages unchanged (tested + visual) | ✅ |
| **E13-M4** | **Visual coverage**: `overflow: hidden \| clip \| visible` (hidden/clip clip painting to the box's content/padding box); `hsl()`/`hsla()` + 8-digit `#rrggbbaa` colors; `border-style` `dashed`/`dotted`/`double` (basic stroke patterns). | `css`, `style`, `layout`, `paint` | `overflow:hidden` clips overflowing content; `hsl()` and `#rrggbbaa` parse; dashed/dotted borders render (tested + visual) | ✅ |

**Epic 13 complete.** 914 workspace tests, clippy clean. CSS coverage broadened across four
milestones, every default byte-identical: box-sizing + min/max sizing (M1), calc() + custom
properties/var() (M2), @media queries + viewport units (M3), and overflow clipping + hsl()
color + dashed/dotted/double borders (M4).

## Non-goals (deferred)

- `@supports`, `@layer` cascade layers, `@container` container queries, `@scope`.
- `:is()`/`:where()`/`:has()` selectors (Epic 7 non-goals — still deferred).
- CSS transitions/animations/`@keyframes` (no timeline in a one-shot renderer).
- `overflow: scroll`/`auto` scrollbars + scroll offset (only `hidden`/`clip` clipping); `overflow-x`/`-y` independence beyond the shorthand.
- `conic-gradient`, `repeating-*-gradient`, multiple/`background-image` layering beyond the existing single linear-gradient; `filter`/`backdrop-filter`, `clip-path`, `mask`.
- `calc()` with `min()`/`max()`/`clamp()`/trig functions; `calc()` in non-length contexts (e.g. inside `rgb()`); type-checking edge cases beyond length/percentage.
- Container-relative units (`cq*`), `ch`/`ex` precision (may approximate), `lh`/`rlh`.
- Color spaces beyond sRGB (`lab()`, `oklch()`, `color()`), `color-mix()`, relative color syntax.
