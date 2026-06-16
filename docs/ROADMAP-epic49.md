# Roadmap — Epic 49: CSS parser & value robustness round 2

Closes real gaps surfaced during the E34–E48 grind: CSS escape sequences aren't
decoded (`\25AA` in `@counter-style` symbols / strings came out as garbage),
gradient color stops only accept `%`/`auto` positions (not `px`/`em`), and
`<position>` keyword parsing mishandles a lone named axis. All in `crates/css`
tokenizer + `crates/style` value parsers.

Same per-milestone pipeline. Additive: values not exercising these stay
byte-identical (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E49-M1** | **CSS escape decoding**: `\<1-6 hex>` (+ optional trailing space) → the codepoint, and `\<char>` → that char literally, in string tokens AND identifiers (the tokenizer currently only handles `\" \' \\`). | `css` | `content:"\25B6"` / `@counter-style{symbols:"\25AA"}` decode to ▶/▪; an identifier `\.foo` escapes the dot; un-escaped strings byte-identical (tested + visual) | ✅ |
| **E49-M2** | **Gradient stop lengths**: a color stop position may be a `<length>` (`px`/`em`/`rem`) or `%`; support the double-position stop form (`#000 10px 20px`); positions resolve against the gradient extent. | `style` | `linear-gradient(#000 0, #000 10px, #fff 10px, #fff 20px)` makes a 10px-period stripe; percent-only gradients byte-identical (tested + visual) | ✅ |
| **E49-M3** | **`<position>` keyword parsing**: `background-position`/`mask-position`/gradient `at` / `object-position` accept the full 1–4 value `<position>` grammar (`left`/`right`/`top`/`bottom`/`center` on the correct axis, two-keyword any order, edge+offset). | `css`, `style` | `background-position: bottom right` puts the image bottom-right (not both on x); `center`/`top` resolve on the right axis (tested + visual) | ✅ |

## Non-goals (deferred)

- `\` newline line-continuation inside strings (rare), and escapes in `url()`
  unquoted bodies beyond the basic cases.
- Gradient stop positions as `calc()` (only plain lengths/percentages), and
  color-stop `<angle>` positions for conic beyond the existing handling.
- 3-value `<position>` (edge + offset + edge) exotic combos beyond the common
  4-value grammar; `<position>` for `transform-origin` z.
