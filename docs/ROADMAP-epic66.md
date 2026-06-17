# Roadmap — Epic 66: multi-column round 2

Multicol (E18-M2, `crates/layout/src/multicol.rs`) distributes block children
across N balanced columns, but: (1) `column-rule` is parsed (`column_rule_width/
style/color` on `ComputedStyle`) yet never painted; (2) `column-span: all` is not
supported; (3) `column-fill` has no control (always balances). This epic adds the
rule painting, spanning elements, and fill control.

Same per-milestone pipeline. Additive: a multicol container with no rule / no
spanner / default fill renders as today → byte-identical (golden + existing
multicol tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E66-M1** | **`column-rule` painting**: the painter detects a multicol box (`column-count`/`column-width` set) with `column-rule-style != None` and width > 0, recomputes the column geometry (same `resolve_columns` formula), and paints a vertical rule centered in each inter-column gap, spanning the content height, in the rule color/style/width. | `paint` | a 3-column box with `column-rule: 2px solid red` paints 2 red vertical lines between the columns (tested + visual) | ✅ |
| **E66-M2** | **`column-span: all`**: new small `ColumnSpan{None,All}` computed field; in `layout_multicol` a child with `column-span: all` interrupts the column flow — children before it columnize, the spanner spans the full content width, children after it columnize in a fresh column set below the spanner. | `style`, `layout` | an `<h2 style="column-span:all">` inside a multicol spans the full width with the body text columnized above/below it (tested + visual) | ✅ |
| **E66-M3** | **`column-fill: auto`**: new `ColumnFill{Balance,Auto}` field (initial `Balance` = current behavior). `column-fill: auto` fills each column up to the container's content height in order before starting the next (requires a definite height; if height is auto, behaves as balance). | `style`, `layout` | a fixed-height multicol with `column-fill: auto` packs the first column full before the second instead of balancing (tested + visual) | ✅ |

## Non-goals (deferred)

- `break-before`/`break-after`/`break-inside: avoid-column` fragmentation control
  and widow/orphan handling across columns.
- Nested multicol, multicol inside flex/grid items beyond the existing block
  layout path, and inline-level content columnization (block children only).
- `column-span: all` with the spanner itself being a float/abspos, and multiple
  interleaved spanners' precise balance of each inter-spanner column group beyond
  the basic greedy balance per group.
- Vertical writing-mode column progression.
