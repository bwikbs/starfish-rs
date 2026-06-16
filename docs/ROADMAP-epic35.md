# Roadmap — Epic 35: pseudo-elements round 2

Extends the `::before`/`::after` machinery (E7-M2: per-element pseudo cascade →
`StyledTree` side tables → woven into the box tree) to the highlight/marker
pseudo-elements: `::marker` (list bullet/ordinal styling), `::placeholder`
(form-control placeholder text styling), and `::first-letter` (first-letter
styling / drop-cap-ish). `::first-line` and `::selection` are line/selection-state
dependent and deferred (non-goals).

Same per-milestone pipeline. Additive: a page using none of these renders
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E35-M1** | **`::marker`**: parse `::marker`; cascade a marker pseudo-style (inherits the list item's style); the synthesized list-item marker box uses it for `color`/font properties, and a `content` string on `::marker` replaces the bullet/ordinal text. | `css`, `style`, `layout` | `li::marker{color:red}` renders red bullets; `::marker{content:"» "}` replaces the marker glyph (tested + visual) | ✅ |
| **E35-M2** | **`::placeholder`**: parse `::placeholder`; cascade a placeholder pseudo-style; the form control's placeholder text is painted with it (color/font) instead of the hard-coded grey. | `css`, `style`, `layout` | `input::placeholder{color:#06c}` colors the placeholder text (tested + visual) | ✅ |
| **E35-M3** | **`::first-letter`**: parse `::first-letter`; cascade a first-letter pseudo-style; the first typographic letter of a block's first line is split into its own run styled by it (`color`/`font-size`/`font-weight`/`font-style`). | `css`, `style`, `layout` | `p::first-letter{font-size:2em;color:#c00}` enlarges + colors the first letter only (tested + visual) | ☐ |

## Non-goals (deferred)

- `::first-line` (line-box-dependent cascade) and `::selection`/`::target-text`
  (no selection state in a one-shot renderer).
- `::first-letter` `float`/drop-cap geometry, punctuation-inclusion rules, and
  multi-codepoint grapheme handling beyond the first `char`.
- `::marker` on non-list-item `display:list-item` boxes, and `::marker`
  animension/`content` with `counter()`.
- `::placeholder` on non-text controls; `:placeholder-shown` already exists (E29).
