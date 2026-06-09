# Roadmap — Epic 21: filters & compositing

CSS `filter`, blend modes (`mix-blend-mode`/`background-blend-mode`),
`mask-image`, and `backdrop-filter` — the offscreen-layer compositing effects.
All build on the existing layer model: `opacity` and `transform` already render an
element's subtree into an offscreen pixmap and composite it back (`PushLayer`/
`PushTransform`). These features extend that — apply a filter to the layer, blend
it with a non-`source-over` mode, mask its alpha, or filter the backdrop behind it.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive,
so pages not using it must stay byte-identical (existing tests + the golden PNG
unchanged).

Current state (reference): the painter composites offscreen layers for `opacity`
(`PushLayer`) and 2D `transform` (`PushTransform`); `tiny-skia` provides a box-blur
(used by `box-shadow`/`text-shadow`), `BlendMode` (used by canvas `clearRect`), and
per-pixel pixmap access. There is **no** CSS `filter`, no `mix-blend-mode`/
`background-blend-mode` (everything composites `source-over`), no `mask-image`, and
no `backdrop-filter`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E21-M1** | **CSS `filter`**: parse a `filter` function list — `blur(<len>)`, `brightness(<n%>)`, `contrast()`, `grayscale()`, `sepia()`, `invert()`, `saturate()`, `hue-rotate(<angle>)`, `opacity()`, `drop-shadow(<x> <y> <blur> <color>)` (+ `none`); applied to the element's offscreen layer (render subtree → apply the filter chain to the layer pixmap → composite). Blur reuses the box-blur; the color filters are a per-pixel color-matrix/transfer pass; drop-shadow = a blurred, offset, recolored copy of the layer's alpha painted under it. | `css`, `style`, `paint` | A `filter: blur()` / `grayscale()` / `drop-shadow()` element renders the effect; a chain composes; a no-filter page is byte-identical (tested + visual) | ✅ |
| **E21-M2** | **Blend modes**: `mix-blend-mode` (`multiply`/`screen`/`overlay`/`darken`/`lighten`/`color-dodge`/`difference`/`exclusion`/… the common separable set) — composite an element's layer onto the backdrop with the chosen `tiny-skia` `BlendMode` instead of `source-over`; `background-blend-mode` — blend an element's stacked background layers (and color) with each other using the mode. | `css`, `style`, `paint` | An element with `mix-blend-mode: multiply` darkens against what's behind it; stacked backgrounds blend per `background-blend-mode` (tested + visual) | ☐ |
| **E21-M3** | **Masking & backdrop-filter**: `mask-image` (a `linear-gradient`/`radial-gradient`/`url()` image used as an alpha or luminance mask multiplying the element's coverage) + `mask-mode`/`-repeat`/`-size` basics; `backdrop-filter` (a `blur()`/color filter applied to the painted backdrop region behind a translucent element, captured before the element composites). | `css`, `style`, `paint` | A gradient `mask-image` fades an element out; a `backdrop-filter: blur()` blurs what shows through a translucent box (tested + visual) | ☐ |

## Non-goals (deferred)

- SVG `filter` elements (`<filter>`/`feGaussianBlur`/`feColorMatrix`/…), `filter:
  url(#id)` references, and `clip-path` (a separate concern from `mask`).
- Non-separable blend modes' exact spec (`hue`/`saturation`/`color`/`luminosity`
  use the full HSL non-separable formulas — approximate or defer); `isolation`
  and stacking-context isolation subtleties beyond the layer already created.
- `mask-image` with multiple mask layers, `mask-composite`, `mask-clip`/`-origin`,
  `-webkit-mask` prefix nuances, SVG `<mask>` references, and luminance-vs-alpha
  edge cases beyond the common gradient/image alpha mask.
- `backdrop-filter` exact backdrop-root semantics, nested backdrop interactions,
  and performance (each backdrop-filter snapshots + filters a region).
- `filter` animation interpolation specifics beyond what Epic 17's interpolation
  already covers; `filter` on inline/split boxes precision; `color-interpolation-
  filters`.
- High-precision color science (filters operate in straight sRGB, not linear-light).
