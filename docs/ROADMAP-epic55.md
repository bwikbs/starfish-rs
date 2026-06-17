# Roadmap — Epic 55: SVG round 3 (image / marker / mask + filter)

Extends inline SVG (E9 shapes/path/text/gradients; E38 use/symbol/pattern/clipPath,
`walk_svg` in `crates/paint/src/display.rs`) with `<image>`, `<marker>` (vertex
arrowheads), and `<mask>` + a basic `<filter>` (`feGaussianBlur`).

Same per-milestone pipeline. Additive: SVG not using these renders
byte-identically (golden + existing SVG tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E55-M1** | **`<image>`**: `<image href x y width height>` embeds a raster (or already-parsed SVG) image at the given rect (decoded via the resource/image store like an `<img>`); `preserveAspectRatio` default (fit). | `paint` | a `<svg><image href="pic.png" x y width height>` paints the decoded image inside the SVG at that rect (tested + visual) | ✅ |
| **E55-M2** | **`<marker>`**: `marker-start`/`marker-mid`/`marker-end` (and the `marker` shorthand) reference a `<marker>` def painted at a path/line/polyline's vertices, oriented by `orient="auto"` (tangent) — MVP at least start/end markers (e.g. arrowheads). | `paint` | a `<line marker-end="url(#arrow)">` paints the arrow marker at the line end vertex (tested + visual) | ✅ |
| **E55-M3** | **`<mask>` + basic `<filter>`**: `mask="url(#m)"` clips/fades a shape by the luminance/alpha of the `<mask>`'s content; `filter="url(#f)"` with an `feGaussianBlur` blurs the element. | `paint` | a shape with `mask=url(#m)` is masked by the mask content; `filter` with `feGaussianBlur stdDeviation=N` blurs it (tested + visual) | ☐ |

## Non-goals (deferred)

- `preserveAspectRatio` modes beyond the default `xMidYMid meet`; `<image>` of an
  SVG with its own complex viewBox transform chain beyond a basic fit.
- `<marker>` `orient` angle values / `markerUnits`/`refX`/`refY` fine-tuning
  beyond a reasonable default placement, and `marker-mid` on every interior vertex.
- `<filter>` primitives beyond `feGaussianBlur` (no feColorMatrix/feOffset/
  feMerge/feFlood chains), filter regions/`filterUnits`, and SVG `<mask>`
  `maskUnits`/`maskContentUnits` beyond the default.
