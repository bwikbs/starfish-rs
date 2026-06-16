# Roadmap — Epic 47: backgrounds & masking round 2

Fills the background/mask gaps: `background-clip`/`background-origin` (the
background analog of E32's `mask-clip`/`mask-origin`), `background-attachment` +
`background-clip: text`, and multi-layer masks + the `mask` shorthand (mask is
currently a single `Option<MaskSpec>`).

Same per-milestone pipeline. Additive: the default border-box backgrounds + a
single mask render byte-identically (golden + existing tests unchanged).
ComputedStyle is at a stack limit — keep new fields small/boxed (FULL cargo test).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E47-M1** | **`background-clip` + `background-origin`**: per-layer geometry box (border/padding/content-box) — origin positions the image, clip bounds the painted area (mirrors E32 mask-clip/origin). | `css`, `style`, `paint` | `background-clip:content-box` clips the background to the content box (padding shows the page behind); `background-origin:content-box` offsets the image; default border-box byte-identical (tested + visual) | ✅ |
| **E47-M2** | **`background-attachment` + `background-clip: text`**: parse `background-attachment: scroll\|local\|fixed` (fixed positions the background against the viewport); `background-clip: text` paints the background clipped to the element's text glyphs (`-webkit-background-clip:text` alias). | `css`, `style`, `paint` | `background-clip:text; color:transparent` shows a gradient through the text shape; `background-attachment` parses (tested + visual) | ☐ |
| **E47-M3** | **Multi-layer masks + `mask` shorthand**: `mask-image` comma-list → a `Vec<MaskSpec>` (each layer composited), and the `mask`/`-webkit-mask` shorthand parsing image+position+size+repeat. | `css`, `style`, `paint` | two comma-separated `mask-image` layers both apply; the `mask:` shorthand sets image+position (tested + visual) | ☐ |

## Non-goals (deferred)

- `mask-composite` (add/subtract/intersect/exclude) and `mask-mode` per-layer
  beyond the existing default — layers composite by intersection/source-over MVP.
- `background-attachment: fixed` true viewport-fixed paint with scrolling (one-shot
  scroll=0, so fixed ≈ positioned against the viewport origin); `local` == scroll.
- `background-clip: border-area`, and text-clip with `background-attachment:fixed`.
