# Roadmap — Epic 67: `text-wrap` line balancing

`text-wrap: balance | pretty` parses into `TextWrap` (`crates/style/src/computed.rs`)
but the greedy line breaker in `crates/layout/src/inline.rs` ignores it (comment:
"balance/pretty are stored but behave as wrap (MVP)"). This epic makes `balance`
equalize line widths (the classic even-ragged heading effect) and `pretty` avoid
a too-short last line (orphan), and wires the `text-wrap-mode`/`text-wrap-style`
longhands.

Same per-milestone pipeline. Additive: `text-wrap: wrap` (the default) keeps the
exact greedy behavior → byte-identical (golden + existing inline/wrap tests
unchanged). Balancing is scoped to horizontal, non-float-narrowed lines (the
heading case); float-affected or vertical blocks fall back to greedy.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E67-M1** | **`text-wrap: balance`**: after the greedy break yields L lines, binary-search the smallest effective wrap width that still fits in L lines and re-break at it, so the lines become near-equal width (no long-first / lone-last). Capped at a small line count (UA limit ~10); above the cap, or when floats narrow the band, fall back to greedy. | `layout` | a 3-line `text-wrap:balance` heading breaks into near-equal-width lines instead of greedy's long-then-short (tested + visual) | ✅ |
| **E67-M2** | **`text-wrap: pretty`**: greedy wrap, but avoid an orphan last line — if the last line is a single short word (or far shorter than the others), nudge the wrap so the last line carries ≥ 2 words / a fairer share. MVP: apply the balance search only to the final 1–2 lines. | `layout` | a paragraph whose greedy wrap leaves one word on the last line instead pulls a word down so the last line isn't a lone orphan (tested + visual) | ✅ |
| **E67-M3** | **`text-wrap-mode` + `text-wrap-style` longhands**: parse `text-wrap-mode: wrap \| nowrap` and `text-wrap-style: auto \| balance \| stable \| pretty`; `text-wrap` is their shorthand. `text-wrap-mode: nowrap` suppresses wrapping (maps to the existing nowrap path); `stable` behaves as `auto` (= wrap, MVP). | `style` | `text-wrap-mode: nowrap` stops wrapping; `text-wrap: balance` still sets style=balance via the shorthand (tested) | ☐ |

## Non-goals (deferred)

- The full Knuth–Plass optimal paragraph algorithm; balance uses a width
  binary-search over the existing greedy breaker (line-count-preserving), not
  global badness minimization.
- Balancing across floats / shape-outside-narrowed bands and vertical writing
  modes (those fall back to greedy).
- `pretty`'s full last-4-lines optimization and hyphenation interaction; MVP
  only avoids a lone-word orphan on the last line.
- `text-wrap-style: stable` incremental-relayout stability guarantees (treated
  as `auto`).
