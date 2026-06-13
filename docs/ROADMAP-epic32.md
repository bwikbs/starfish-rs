# Roadmap — Epic 32: graphics effects (clip-path, masking, blend)

Extends the paint layer's compositing/clipping past the Epic 13/21 baseline:
**`clip-path`** basic shapes (`inset`/`circle`/`ellipse`/`polygon`), **mask
positioning** (`mask-position`/`-size`/`-repeat`), and **blend-mode rounding-out**
(isolation + remaining mix-blend-modes). All build on the existing display-list
clip + offscreen-layer machinery (crates/paint `display.rs`/`raster.rs`).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a page using no `clip-path`/mask-positioning must render byte-identically
(existing tests + the golden PNG unchanged).

Current state (reference): overflow clipping emits `PaintCmd::PushClip { rect,
radius }` (rounded-rect only) consumed by `raster.rs` (a `Mask` intersected with
the rounded-rect path). `mask-image` (gradient/url) exists from E21 but only at
the box's border-box extent (no mask position/size/repeat). `filter` (incl.
`drop-shadow`) and `mix-blend-mode`/`background-blend-mode` ship from E21. There
is **no** `clip-path` (property unknown → ignored).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E32-M1** | **`clip-path` basic shapes**: parse `inset(t r b l [round <radii>])`, `circle(<r> [at <x> <y>])`, `ellipse(<rx> <ry> [at <x> <y>])`, and `polygon(x1 y1, x2 y2, …)` into a `ClipShape`; emit a `PaintCmd::PushClipPath { path }` (a `tiny-skia` path built from the shape against the box's border-box) so the box's content + descendants are clipped to the shape. Percentages resolve against the box size; `circle` default radius is `closest-side`. | `css`, `style`, `paint` | a `clip-path: circle(40%)` box paints only inside the circle; `polygon(0 0, 100% 0, 50% 100%)` clips to a triangle; a no-clip-path box is byte-identical (tested + visual) | ✅ |
| **E32-M2** | **Mask positioning**: `mask-position`/`mask-size`/`mask-repeat` (mirroring the existing `background-*` positioning) so a `mask-image` can be placed/scaled/tiled rather than always filling the border box; plus `mask-clip`/`mask-origin` to the border/content/padding box. | `css`, `style`, `paint` | a `mask-image` with `mask-position: center` / `mask-size: 50%` / `mask-repeat: no-repeat` is placed accordingly (not stretched to fill), matching the equivalent `background` placement (tested + visual) | ☐ |
| **E32-M3** | **Blend + isolation round-out**: `isolation: isolate` (a new stacking context that confines `mix-blend-mode` to its subtree), the remaining separable + non-separable `mix-blend-mode` keywords (hue/saturation/color/luminosity if missing), and `background-blend-mode` across multiple background layers. | `css`, `style`, `paint` | `isolation: isolate` stops a child's `mix-blend-mode` from blending with content outside the isolated parent; the non-separable blend modes match their formula (tested) | ☐ |

## Non-goals (deferred)

- `clip-path` with `path()` (SVG path data), `shape()`, geometry-box references
  (`border-box`/`margin-box`/`view-box`) beyond the default border-box, and
  `fill-rule` for `polygon`/`path` (uses non-zero winding).
- `clip-path` animation/interpolation between shapes; `clip-path` on SVG
  elements (only CSS-box clipping).
- `mask-composite` (add/subtract/intersect/exclude across multiple mask layers),
  `mask-mode` (luminance vs alpha) beyond the current default, `mask-type`, and
  `-webkit-mask-box-image` / `mask-border`.
- `mix-blend-mode: plus-lighter`/`plus-darker`; blend in non-sRGB working spaces;
  `isolation` interaction with `will-change`/filters establishing contexts
  beyond what already does.
- `backdrop-filter` changes (already E21); `element()` / `paint()` (Houdini)
  image sources.
