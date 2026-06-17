# Roadmap — Epic 68: `border-image`

`border-image` is entirely absent (no `border_image*` anywhere). It draws a
9-sliced source image into a box's border region: 4 corners + 4 edges (+ an
optional center fill). The painter already supports a scaled sub-image blit
(`PaintCmd::ImageBlit { dest, src, src_crop, .. }`, used by object-fit), so each
of the 9 slices is one ImageBlit (`src_crop` = slice region, `dest` = border
region).

Computed state is a single BOXED struct `border_image: Option<Box<BorderImage>>`
(8 bytes on `ComputedStyle`) to avoid the recursive style/layout stack-depth
limit. Default `None` → no border-image → byte-identical to today.

Same per-milestone pipeline. Additive: no `border-image` → byte-identical
(golden + existing border tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E68-M1** | **`border-image-source: url()` + `border-image-slice`**: boxed `BorderImage` field; parse the source (url) and `border-image-slice: <number>{1,4}` (unitless = px in the image, `%` of the image size). Paint the 4 corner slices stretched into the border-width corners and the 4 edge slices stretched along each edge (no center fill). | `style`, `paint` | a box with `border:16px solid; border-image-source:url(frame.png); border-image-slice:16` paints the image's 4 corners + 4 edges into the border, replacing the solid color (tested + visual) | ☐ |
| **E68-M2** | **`border-image-repeat` + `fill` + `border-image-width`**: `border-image-repeat: stretch \| repeat \| round` tiles the edge slices instead of stretching (`round` rescales to a whole count; `space` ≈ repeat MVP); the `fill` keyword on `border-image-slice` paints the center slice into the content box; `border-image-width: <len>\|<number>\|<%>\|auto` overrides the destination border thickness. | `style`, `paint` | `border-image-repeat:repeat` tiles a patterned edge; `border-image-slice:16 fill` fills the center (tested + visual) | ☐ |
| **E68-M3** | **`border-image` shorthand + `border-image-outset` + gradient source**: parse the `border-image` shorthand (`source slice / width / outset repeat`); `border-image-outset` extends the painted border-image area outward beyond the border box; `border-image-source` accepts a CSS `<gradient>` (reuse the gradient rasterization as the slice source). | `style`, `paint` | the `border-image` shorthand sets all parts; a `linear-gradient()` source paints a gradient frame; outset pushes it outward (tested + visual) | ☐ |

## Non-goals (deferred)

- `border-image-repeat: space` exact gap distribution (treated as `repeat`).
- Multiple/`image-set()` sources and SVG-element border-image sources.
- Interaction with `border-radius` clipping of the border-image (painted on the
  rectangular border box; rounding of the image area is a non-goal).
- Sub-pixel `round` tiling exactness beyond a whole-count rescale.
