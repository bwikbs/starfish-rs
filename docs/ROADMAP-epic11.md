# Roadmap — Epic 11: performance (caching, incremental relayout)

The renderer is a one-shot pipeline (URL → fetch → parse → script → cascade →
layout → paint → PNG), but several stages do redundant work: text is **shaped
twice** for every run (once by the measurer during line-breaking, once by the
painter), the cascade re-resolves identical rule sets per element, and any
forced reflow during JS (`getComputedStyle`, re-render after a DOM mutation)
recomputes everything from scratch. Epic 11 makes the hot paths cheap and
re-layout incremental, with benchmarks proving each win.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Correctness is the gate:
every milestone must leave the rendered output (and all tests) **identical** —
caching/incrementalism may only change timing, never pixels.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E11-M1** | Shape/measure cache: memoize `FontDb::shape(text, q)` (now the dominant cost — rustybuzz runs in BOTH the measurer and the painter for the same run). A keyed cache (text + resolved face + size + style + spacing) returns a shared `Rc<[ShapedGlyph]>`; `advance_width` and `draw_glyph_run` hit it. Add a microbenchmark showing the speedup on a text-heavy page; assert output is byte-identical (golden render unchanged). | `paint` | The same run is shaped once, not per measure+paint; a text-heavy render is measurably faster; the golden PNG and all tests are unchanged (tested + benchmark) | ✅ |
| **E11-M2** | Style sharing / cascade cache: avoid re-running the full selector cascade for elements that share the same rule set (a style-sharing cache keyed by tag/class/attr signature, à la Servo), and/or memoize matched-declaration lists per rule-set. Computed styles are shared via `Rc` where identical. | `style` | Elements with identical styling reuse one computed style; a list-heavy page cascades faster; output + tests unchanged (tested + benchmark) | ✅ |
| **E11-M3** | Dirty tracking + incremental relayout: mark nodes/subtrees dirty on DOM mutation (from JS) so a re-layout after script quiescence (or a forced reflow via `getComputedStyle`/`offsetWidth`) only recomputes the dirty subtrees, reusing clean cached box geometry. Bounded, correctness-first: when in doubt, recompute. | `layout`, `js` | A small mutation triggers a partial relayout (not a full one) while producing the same result as a full relayout (tested + benchmark) | ☐ |

## Non-goals (deferred)

- Multi-threaded / parallel layout or paint (single-threaded throughout).
- A retained display list / damage-rect repaint (we repaint the whole pixmap;
  incremental *paint* is out of scope — only layout/style/shape are cached).
- GPU acceleration, glyph atlas caching on the GPU.
- Persisting caches across renders / processes (caches live for one render).
- Speculative or predictive relayout; layout-on-idle; frame scheduling (there is
  no animation timeline — Epic 4's timers are virtual/bounded).
- Cache eviction policies beyond a simple bound (renders are finite; an
  unbounded per-render cache is acceptable if memory stays sane).
