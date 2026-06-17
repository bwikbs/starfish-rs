# Roadmap — Epic 70: `clip-path` round 2

`clip-path` (E32) supports `inset()`/`circle()`/`ellipse()`/`polygon()` resolved
against the **border-box only**. This round adds the `path()` function (arbitrary
SVG path clip), the geometry-box reference keyword (`border-box`/`padding-box`/
`content-box`/`margin-box`), and the rect-family shapes `rect()`/`xywh()`.

Maps onto existing machinery: `svg_path::parse_path_data` parses path data, the
clip raster (`clip_shape_path` in `crates/paint/src/raster.rs`) builds the
tiny-skia clip path, and `PushClipPath { shape, border_box }` already brackets the
clipped subtree.

Same per-milestone pipeline. Additive: existing `clip-path` shapes resolve against
the border-box exactly as today → byte-identical (golden + E32 tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E70-M1** | **`clip-path: path()`**: add a `ClipShape::Path` variant carrying the parsed path ops; parse `path("<svg-path-data>")` (optional leading fill-rule `nonzero`/`evenodd`); the clip raster builds the tiny-skia path from the ops (path coords in border-box-local px). | `style`, `paint` | `clip-path:path('M0,0 L100,0 L50,100 Z')` clips an element to that triangle (tested + visual) | ✅ |
| **E70-M2** | **geometry-box reference**: parse an optional `<geometry-box>` (`border-box`/`padding-box`/`content-box`/`margin-box`) alongside the shape (`clip-path: circle(50%) padding-box`); resolve the shape + percentages against the chosen reference rect instead of always the border-box. Box-only `clip-path: padding-box` (no shape) clips to that box rect. | `style`, `paint` | `clip-path: inset(0) content-box` clips to the content box; `circle(50%) border-box` unchanged from M0 (tested + visual) | ✅ |
| **E70-M3** | **`rect()` + `xywh()`**: parse `rect(<top> <right> <bottom> <left> [round <r>])` (edge offsets from the reference box's top-left) and `xywh(<x> <y> <w> <h> [round <r>])` (a rect at x/y of size w×h); both produce an inset-equivalent clip. (Corner rounding parsed; MVP ignores the radius in the clip extent.) | `style`, `paint` | `clip-path: xywh(10px 10px 80px 40px)` clips to that rectangle (tested + visual) | ✅ |

## Non-goals (deferred)

- The CSS `shape()` function (responsive path with `from`/`by` + CSS units).
- `clip-path` transition/interpolation between shapes, and animating `path()`.
- Exact corner rounding of `rect()`/`xywh()`/`inset(... round ...)` in the clip
  extent (the radius is parsed but the clip uses the sharp rectangle).
- `fill-box`/`stroke-box`/`view-box` geometry boxes (SVG-context reference boxes).
