# Roadmap — Epic 41: text decoration & emphasis round 3

Extends `text-decoration-line` (E2-M1: underline/overline/line-through painted as
plain rects in `decorate` / `display.rs`) with the full text-decoration longhands
(`-color`, `-style`, `-thickness`, `text-underline-offset`) and `text-emphasis`
marks.

Same per-milestone pipeline. Additive: text using only `text-decoration:underline`
(current behavior) renders byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E41-M1** | **`text-decoration-color` + `-style`**: a decoration color (default = current `color`) and a line style `solid`/`double`/`dotted`/`dashed`/`wavy`; the `text-decoration` shorthand parses line+color+style. Painter draws the chosen style/color. | `css`, `style`, `paint` | `text-decoration: underline wavy red` draws a red wavy underline; `double`/`dotted`/`dashed` differ from `solid`; plain underline is byte-identical (tested + visual) | ✅ |
| **E41-M2** | **`text-decoration-thickness` + `text-underline-offset`**: explicit line thickness and underline vertical offset (`auto`/length), used by the painter instead of the derived defaults. | `css`, `style`, `paint` | `text-decoration-thickness:4px` thickens the line; text-underline-offset:6px lowers the underline (tested + visual) | ✅ |
| **E41-M3** | **`text-emphasis`**: `text-emphasis-style` (`dot`/`circle`/`filled`/`open`/a string) + `text-emphasis-color` + `text-emphasis-position` (over/under); the painter draws an emphasis mark centered over (or under) each base character. | `css`, `style`, `paint` | `text-emphasis: filled dot red` draws red dots above each character (tested + visual) | ☐ |

## Non-goals (deferred)

- `text-decoration-skip-ink`, decoration on inline-box descendants propagation
  nuance, and `text-underline-position`.
- `text-emphasis` skipping spaces/punctuation per spec, ruby-aware emphasis
  placement, and per-CJK mark defaults.
- `text-decoration` `spelling-error`/`grammar-error` line styles.
