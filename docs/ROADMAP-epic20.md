# Roadmap — Epic 20: `<canvas>` 2D

Implements `<canvas>` with a `CanvasRenderingContext2D`: scripts call the 2D
drawing API (`fillRect`, paths, gradients, text, images), and the result is
composited into the canvas element's box in the rendered page.

**Architecture (the key decision).** Canvas drawing happens during JS execution,
but the rasterizer (`tiny-skia`) lives in the `paint` crate, which depends on
`js` — so `js` cannot rasterize directly (a cycle, the same constraint hit in
Epic 19-M2). Therefore the 2D context **records a display list of canvas
operations** (a `Vec<CanvasOp>` stored in the `dom` crate, which sits below both
`js` and `paint`, keyed by the canvas node); at paint time the `paint` crate
**replays that op list into the canvas element's box** via `tiny-skia`. The
backing-store size comes from the canvas `width`/`height` content attributes
(default 300×150), scaled to the element's CSS box.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. A page with no `<canvas>`
(or a canvas never drawn to) must stay byte-identical (existing tests + the golden
PNG unchanged).

Current state (reference): `<canvas>` parses as an unknown element with no special
layout or paint (an empty inline box); there is no `HTMLCanvasElement`,
`getContext`, or `CanvasRenderingContext2D` in the JS realm; the `paint` crate has
a full tiny-skia path/fill/stroke/gradient/glyph/image toolkit (used for CSS, SVG,
text) that the canvas replay can reuse.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E20-M1** | **Canvas core + rect/path fills**: `<canvas width height>` as a replaced element sized by its attributes (default 300×150) / CSS box; `canvas.getContext('2d')` → a `CanvasRenderingContext2D`; state `fillStyle`/`strokeStyle` (color), `lineWidth`; `fillRect`/`strokeRect`/`clearRect`; path building `beginPath`/`moveTo`/`lineTo`/`closePath`/`rect`/`arc` + `fill`/`stroke`. The 2D context appends `CanvasOp`s to the node's op list (dom crate); the painter replays them into the canvas box (tiny-skia). | `dom`, `js`, `layout`, `paint` | A script that does `fillRect` + a filled/stroked `arc` path renders the shapes inside the canvas box (tested + visual) | ✅ |
| **E20-M2** | **State, transforms & gradients**: `save`/`restore` (a state stack of style+transform); `translate`/`scale`/`rotate`/`transform`/`setTransform` (a current transform matrix applied to ops); `createLinearGradient`/`createRadialGradient` + `addColorStop` as fill/stroke styles; `globalAlpha`; `lineCap`/`lineJoin`/`setLineDash`; `quadraticCurveTo`/`bezierCurveTo`; `clip`. | `dom`, `js`, `paint` | A transformed, gradient-filled path with save/restore and a clip renders correctly (tested + visual) | ☐ |
| **E20-M3** | **Text & images**: `fillText`/`strokeText` (+ `font`/`textAlign`/`textBaseline`); `measureText` → `{ width }` (font-free metric like the Epic 19-M2 geometry fallback, documented); `drawImage(img\|canvas, …)` (the 3/5/9-arg forms) compositing a decoded `<img>` or another canvas's op-list result. | `dom`, `js`, `paint` | `fillText` draws text and `drawImage` composites an image into the canvas (tested + visual) | ☐ |

## Non-goals (deferred)

- `getImageData`/`putImageData`/`createImageData` and any pixel read-back (the
  op-list-replayed-at-paint model has no mid-script raster); `ImageData`,
  `toDataURL`/`toBlob`.
- `globalCompositeOperation` beyond the default `source-over`; `CanvasPattern`
  (`createPattern`), shadows (`shadowBlur`/`shadowColor`/`shadowOffset*`),
  `filter`, `imageSmoothingEnabled` nuances.
- WebGL / `getContext('webgl')` (a separate concern; WebGL is enabled in the C++
  engine build but out of scope here), `OffscreenCanvas`, `ImageBitmap`.
- `measureText` advanced metrics (`actualBoundingBox*`, font ascent/descent) — only
  `width`, and approximate (no FontDb in the JS realm); `direction`, letter/word
  spacing on canvas text.
- `ellipse`, `arcTo` full tangent math, `isPointInPath`/`isPointInStroke`,
  `roundRect`, non-zero vs even-odd fill-rule edge cases beyond the common forms.
- Hit regions, focus rings, `drawFocusIfNeeded`, accessibility of canvas content.
- High-DPI backing-store scaling beyond the attribute size → CSS box mapping.
