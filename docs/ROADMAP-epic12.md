# Roadmap — Epic 12: incremental box-layout (measure memoization)

Layout runs once per render, but inside that single pass the **same subtree is laid
out 2–3 times** for measurement: table and grid size their columns, then their rows,
then place cells/items for real — each of `table::measure_cell_width`,
`measure_cell_outer_height`, `grid::measure_item_width`, `measure_item_height`,
`flex::flex_base_main` computes an intrinsic scalar by re-running `layout_block` over
the item subtree and reading `max_content_right`/content height. The same node is
measured in the column pass, the row pass, and the final pass — redundant work that
grows with subtree depth (a nested table cell is re-laid-out 3× at every level).

Epic 12 memoizes those measurement calls. A scalar result is cached per
`(NodeId, MeasureKind, available-width)`, so a node measured again at the same width
returns the cached scalar instead of re-laying-out its subtree. PURE optimization:
byte-identical output (all tests + the golden PNG unchanged) — only timing changes.

The correctness foundation: every `measure_*` call invokes `layout_block` with a fresh
`(x=0, y=0, height=0)` containing block and an empty `FloatContext`, so the result
depends ONLY on the subtree (content + style + fonts, all frozen during one `layout()`
call) and the available width. `NodeId` is a stable proxy for the frozen styled
subtree; `width.to_bits()` captures the one width-conditioned input. So a cache hit
equals a fresh compute — provably byte-identical. Only the scalar is cached (never the
placed `Dimensions`), keeping the input set closed: the measuring callers consume only
the scalar, while final placement re-runs `layout_block` separately.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E12-M1** | A `LayoutCache` (`RefCell<HashMap<(NodeId, MeasureKind, u32), f32>>`) created per `layout()` call and threaded through block/inline/flex/grid/table (like the `TextMeasurer`/`ImageSource` already are). The five `measure_*` functions look up `(node, kind, width_bits)` and only run `layout_block` on a miss. A cfg(test) `MEASURE_UNCACHED_CALLS` counter proves the per-node multi-pass redundancy collapses (table/grid measure each node once, not 2–3×). | `layout` | The same node's intrinsic measurement runs once across the column/row/final passes; a table/grid page lays out with fewer `layout_block` measure calls; all tests + golden PNG unchanged (tested + counter) | ✅ |

**Epic 12 complete.** 841 workspace tests, clippy clean. A `LayoutCache` threaded through
block/inline/flex/grid/table memoizes the five intrinsic `measure_*` calls per
`(NodeId, MeasureKind, available-width)`, so a node measured across the column/row/final
passes is laid out once instead of 2–3×. Only `BoxStyleRef::Node` boxes are cached
(anonymous/generated boxes, which can share a parent NodeId, bypass the cache); only the
scalar is stored (placed `Dimensions` are never reused). Byte-identical: a table+grid page
renders bit-for-bit identically before and after, the golden PNG is unchanged, and the
178 layout tests pass.

## Non-goals (deliberately deferred — correctness-first)

- **Arbitrary subtree-layout memoization** (reusing a placed `LayoutBox` subtree). A
  general subtree's layout depends on preceding-sibling floats, margin collapsing
  across its boundary, percentage heights (containing-block height), abspos containing
  blocks, and its x/y offset — none width-only. Keying on all of those is not simply
  provable, and reuse needs a deep clone + O(n) re-translate whose cost rivals
  re-layout. Excluded. (The `measure_*` path is safe precisely because it is always
  called at `(0,0,0)` with empty floats, so those dependencies are constant.)
- **Cross-relayout caching keyed by `Document.mutation_version`** (the layout analog of
  E11-M3's styled-tree memo). Layout runs exactly once per render and there is no
  JS-driven reflow (the JS realm has no `FontDb` — fonts are built after scripts), so
  such a cache has no hit scenario today. Deferred until a re-layout trigger exists.
- **JS forced-reflow APIs** (`offsetWidth`/`offsetHeight`/`getBoundingClientRect`):
  would require a `FontDb` (and loaded `@font-face`s) available mid-script, which the
  current pre-script/post-script font ordering does not provide. Out of scope.
- Multi-threaded / parallel layout; incremental *paint* (damage rects).
