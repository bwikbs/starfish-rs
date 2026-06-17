# Roadmap — Epic 73: SVG `<textPath>` (text along a path) — grind finale

`<textPath href="#p">` lays text along a referenced `<path>` instead of a straight
baseline. Absent today (`emit_svg_text` only does straight horizontal runs). This
is the thematic capstone of the path arc built this grind — it reuses the
arc-length path sampling from E69 motion-path (`flatten_path`/`sample_polyline` in
`crates/paint/src/display.rs`), the SVG path parser (`svg_path::parse_path_data`),
the per-character font advances, and per-glyph `PushTransform`+`GlyphRun`.

Same per-milestone pipeline. Additive: plain `<text>` (no `<textPath>` child)
renders byte-identically (golden + existing SVG text tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E73-M1** | **glyphs along the path**: a `<text>` with a `<textPath href="#p">` child resolves `#p` to a `<path>` element, parses its `d`, flattens to an arc-length polyline, and lays each character at its accumulated advance distance along the path, rotated to the path tangent (each glyph = `PushTransform(translate·rotate)` + single-char `GlyphRun`). | `paint` | `<textPath href="#arc">Hello</textPath>` paints the glyphs following the arc, each rotated to the curve (tested + visual) | ☐ |
| **E73-M2** | **`startOffset` + `text-anchor`**: `startOffset` (`<length>`/`<percentage>` of path length) shifts the run's start distance along the path; `text-anchor: middle`/`end` shifts the start by −½·textlen / −textlen (measured along the path), so the text centers/ends at the offset point. | `paint` | `startOffset:50%` + `text-anchor:middle` centers the text at the path midpoint (tested + visual) | ☐ |
| **E73-M3** | **robustness + `<tspan>`/letter-spacing**: glyphs whose distance exceeds the path length are dropped (not wrapped); a missing/empty `href` path → graceful no-render of the textPath (the rest of `<text>` unaffected); `<tspan>` inside `<textPath>` contributes its text; `letter-spacing` adds per-glyph advance along the path. | `paint` | text longer than the path truncates at the path end; a missing path renders nothing; letter-spacing spreads the glyphs (tested + visual) | ☐ |

## Non-goals (deferred)

- `side: right` (reversed-direction text), `spacing: auto` glyph-spacing
  adjustment, and `method: stretch` (glyph distortion along the path).
- Bidi/RTL reordering along the path and vertical writing-mode text paths.
- `<textPath>` referencing a basic shape (`<rect>`/`<circle>`) via `shape`/CSS;
  only an explicit `<path>` `href` is supported.
- Per-glyph `dx`/`dy`/`rotate` attribute lists on the `<text>`/`<tspan>`.
