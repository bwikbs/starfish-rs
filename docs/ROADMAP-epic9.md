# Roadmap — Epic 9: inline SVG

Renders inline `<svg>` — shapes, paths, and text — into the page. Same per-milestone
agent pipeline (design → analysis → implementation → review → verification), each landing
as its own commit + push.

An `<svg>` element is treated as a replaced element sized by its `width`/`height`/`viewBox`;
its subtree is painted by a dedicated SVG painter (tiny-skia paths), not the normal CSS box
walk. The HTML parser needs minimal foreign-content handling so SVG markup (self-closing
shapes, camelCase tags/attrs) survives.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **E9-M1** | HTML foreign-content for `<svg>` (preserve case, self-closing shapes); `<svg>` as a replaced box sized by width/height + `viewBox` (scale/translate mapping); basic shapes `rect` (+ `rx`/`ry`), `circle`, `ellipse`, `line`; presentation attributes `fill`, `stroke`, `stroke-width`, `opacity`, `fill-opacity`/`stroke-opacity` (+ from style). | `html`, `style`, `layout`, `paint` | An inline `<svg>` with rect/circle/ellipse/line + fill/stroke renders at the right size/colors (tested + visual) |
| **E9-M2** | `<path d="…">` (commands M/m L/l H/h V/v C/c S/s Q/q T/t A/a Z) filled + stroked with the correct fill-rule (`nonzero`/`evenodd`); `polygon`/`polyline`; `stroke-linecap`/`linejoin` (basic), `stroke-dasharray` (optional). | `paint` (svg) | A `<path>`/`<polygon>` renders the correct outline filled + stroked (tested + visual) |
| **E9-M3** | SVG `<text>`/`<tspan>` (x/y/fill/font), `<g>` grouping + `transform` (translate/scale/rotate/matrix) on any element, `linearGradient`/`radialGradient` fills (`<defs>` + `url(#id)`), `<polyline>` markers optional. | `paint` (svg) | SVG text, a transformed group, and a gradient-filled shape render (tested + visual) |

## Non-goals (deferred)

- Standalone `.svg` documents loaded as images (only INLINE `<svg>` in HTML); `<use>`/
  `<symbol>`/`<image>`, clipping/masking, filters (blur/feGaussianBlur), patterns, `<switch>`,
  CSS animation/SMIL, `<foreignObject>`, markers (arrowheads) beyond basic, `pathLength`,
  `preserveAspectRatio` beyond the default `xMidYMid meet`.
- Full SVG presentation-attribute ↔ CSS cascade integration (presentation attributes +
  inline `style` are read; full selector matching on SVG elements is basic).
- Hit-testing / pointer events on SVG.
