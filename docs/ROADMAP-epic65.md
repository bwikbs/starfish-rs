# Roadmap — Epic 65: `shape-outside` (float exclusion shapes)

Floats currently exclude inline content using their full rectangular margin box
(`FloatContext::left_offset`/`right_offset` in `crates/layout/src/float.rs`).
`shape-outside` lets a float's exclusion area follow a basic shape so text wraps
around a circle/ellipse/inset/polygon instead of the rectangle. The inline layout
already re-queries the float band at each line's y, so a per-y shape extent slots
straight into that model. The `ClipShape` parser (`parse_clip_path`, E32) is
reused for the shape value.

Same per-milestone pipeline. Additive: a float with no `shape-outside` uses its
rectangle exactly as today → byte-identical (golden + existing float tests).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E65-M1** | **`shape-outside: circle()`**: parse `shape-outside` (reuse the clip-path basic-shape parser) into a boxed `Option<Box<ClipShape>>` computed field; a floated element with `shape-outside: circle()` narrows its exclusion band per line to the circle's horizontal extent at that y (resolved in the float's margin box), so inline text hugs the curve. | `style`, `layout` | a left float `shape-outside:circle(50%)` lets following text step in toward the float near the circle's top/bottom and out at its middle (tested + visual) | ✅ |
| **E65-M2** | **`ellipse()` + `inset()`**: same per-y extent for `ellipse(rx ry)` (independent axes) and `inset(t r b l [round …])` (rect inset, ignoring corner rounding for the extent MVP). | `layout` | a float `shape-outside:ellipse(40px 80px)` wraps text to the ellipse; `inset(20px)` shrinks the exclusion rect by 20px on each side (tested + visual) | ✅ |
| **E65-M3** | **`polygon()` + `shape-margin`**: `polygon(x y, …)` extent per band (min/max x of the polygon edges crossing the band); `shape-margin: <len>` expands the shape's exclusion outward by that distance (MVP: inflate the per-band extent by shape-margin). | `style`, `layout` | a triangular `polygon()` float wraps text along the slanted edge; `shape-margin:10px` pushes text 10px further out (tested + visual) | ✅ |

## Non-goals (deferred)

- `shape-outside: <image>` (alpha-channel / `<gradient>` derived shapes) and the
  `shape-image-threshold` property.
- Exact corner-rounding of `inset(... round ...)` in the exclusion extent (the
  rounded corners are ignored; the inset rectangle is used).
- Shapes on non-floated elements (`shape-outside` only affects floats per spec);
  `shape-outside` interaction with writing-mode / vertical floats.
- Sub-pixel-accurate curve sampling: the per-band extent is evaluated at the
  band's y-range edges (a tight conservative bound), not integrated over the band.
