# Roadmap — Epic 56: text round 4 (ruby / initial-letter / hanging-punctuation)

Adds East-Asian + fine typography: `<ruby>`/`<rt>`/`<rp>` annotations,
`initial-letter` drop caps, and `hanging-punctuation` + `text-wrap` parsing.

Same per-milestone pipeline. Additive: text not using these renders
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E56-M1** | **`<ruby>`/`<rt>`/`<rp>`**: render ruby as an inline-block stacking the `<rt>` annotation (smaller, ~0.5em) centered ABOVE the base text; `<rp>` parenthesis fallback is `display:none`. Base + annotation widths reconciled (the wider sets the column). | `style`, `layout` | `<ruby>漢<rt>kan</rt></ruby>` renders "kan" centered above 漢 as one inline unit; rp hidden (tested + visual) | ✅ |
| **E56-M2** | **`initial-letter`**: `initial-letter: <size> [<sink>]` makes a block's `::first-letter` (E35-M3) a drop cap sized to `size` lines and sunk `sink` lines into the text. | `style`, `layout` | `p::first-letter{initial-letter:3}` enlarges the first letter to ~3 lines tall as a drop cap (tested + visual) | ✅ |
| **E56-M3** | **`hanging-punctuation` + `text-wrap`**: parse `hanging-punctuation` (first/last/force-end/allow-end) — opening/closing punctuation hangs into the margin at line edges; parse `text-wrap: wrap\|nowrap\|balance\|pretty` (balance/pretty stored, behavior = normal MVP). | `style`, `layout` | `hanging-punctuation: first` pulls a leading quote into the start margin; `text-wrap` values parse (tested) | ☑ |

## Non-goals (deferred)

- Ruby with multiple `<rt>` per base / `<rbc>`/`<rtc>` complex ruby, ruby
  over-hang onto adjacent text, and `ruby-position: under`/`ruby-align`.
- `initial-letter` exact baseline-grid sinking geometry + float interaction
  (MVP: scale the first letter + reserve its line span, no precise grid snap).
- `text-wrap: balance`/`pretty` actual line-balancing (parsed only); CJK
  line-break (`line-break: strict/loose`) rules beyond the existing breaking.
