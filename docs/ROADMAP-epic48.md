# Roadmap — Epic 48: images & gradients round 3

Rounds out CSS image values: the `repeating-*-gradient` functions, `image-set()`,
and `cross-fade()`. Builds on E16's gradient painting (linear/radial/conic) and
E15's `srcset` density selection.

Same per-milestone pipeline. Additive: non-repeating gradients + plain images
render byte-identically (golden + existing tests unchanged). ComputedStyle is at
a stack limit — keep additions small.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E48-M1** | **`repeating-linear/radial/conic-gradient`**: a `repeating` flag on the gradient values; parse the `repeating-*` function names; the painter tiles the stop pattern (linear/radial via tiny-skia `SpreadMode::Repeat`; conic by repeating the wedge fan over the period). | `css`, `style`, `paint` | `repeating-linear-gradient(45deg,#000 0,#000 10px,#fff 10px,#fff 20px)` paints repeating stripes; non-repeating byte-identical (tested + visual) | ✅ |
| **E48-M2** | **`image-set()`**: `image-set(url(a) 1x, url(b) 2x)` (and `type()`) picks the candidate for the device pixel ratio (one-shot dpr=1 → the 1x candidate); usable as a `background-image` / `<img>`-ish image value. | `css`, `style`, `paint` | `background-image: image-set(url(a.png) 1x, url(b.png) 2x)` resolves the 1x url at dpr 1 (tested) | ☐ |
| **E48-M3** | **`cross-fade()`**: `cross-fade(<image> <p>, <image>)` blends two images/gradients by the percentage (paint both into a layer, weight by the fade). | `css`, `style`, `paint` | `cross-fade(linear-gradient(...) 50%, linear-gradient(...))` blends the two gradients 50/50 (tested + visual) | ☐ |

## Non-goals (deferred)

- `image-set()` `type()` format negotiation beyond picking by resolution, and
  resolution units other than `x`/`dppx`; the actual >1 dpr selection (one-shot
  is dpr 1).
- `cross-fade()` with >2 images / omitted percentages summing logic beyond the
  2-image MVP; `element()` / `paint()` image sources.
- `repeating-conic-gradient` exactness at the wedge seam (flat-shaded fan, like
  the base conic).
