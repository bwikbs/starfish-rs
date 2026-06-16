# Roadmap — Epic 38: SVG round 2 (use / symbol / pattern / clipPath)

Extends the inline-SVG painter (E9: shapes/path/text/`<g>`/transforms/gradients,
`walk_svg` in `crates/paint/src/display.rs`) with reuse + fill/clip definitions:
`<use>`/`<symbol>`/`<defs>` instancing, `<pattern>` fills, and `<clipPath>` +
the `clip-path` attribute.

Same per-milestone pipeline. Additive: SVG not using these renders
byte-identically (golden + existing SVG tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E38-M1** | **`<use>`/`<symbol>`/`<defs>`**: `<use href="#id" x y>` instantiates the referenced element (translated by x/y), `<symbol>` is a non-rendered template instantiated only via `<use>` (renders its children), `<defs>` content is non-rendered (already skipped). Id lookup within the SVG subtree; recursion bounded against cycles. | `paint` | `<defs><circle id=c></defs><use href="#c" x=20>` paints the circle at the offset; `<symbol>`+`<use>` renders the symbol content; no SVG-reuse is byte-identical (tested + visual) | ✅ |
| **E38-M2** | **`<pattern>` fills**: `fill="url(#pat)"` tiles a `<pattern>`'s content across the filled shape's bounding box (userSpaceOnUse / objectBoundingBox units MVP), painted clipped to the shape. | `paint` | a `<rect fill="url(#dots)">` tiles the pattern's shapes across the rect (tested + visual) | ✅ |
| **E38-M3** | **`<clipPath>` + `clip-path`**: a shape/group with `clip-path="url(#cp)"` is clipped to the union of the `<clipPath>`'s child shapes. | `paint` | a `<rect clip-path="url(#cp)">` where `#cp` is a circle paints only inside the circle (tested + visual) | ✅ |

## Non-goals (deferred)

- `<marker>` (line markers), `<mask>` SVG masking, `<image>` inside SVG, nested
  external `<use>` (cross-document href), and `<pattern>` `patternTransform`.
- `clipPathUnits`/`patternContentUnits` beyond the default, `clip-rule`, and
  `<filter>` SVG filter primitives (CSS `filter` already exists from E21).
- `<switch>`/`<foreignObject>`, `<tspan>` advanced positioning, and SVG
  `<style>`/CSS-on-SVG selector matching beyond presentation attributes.
