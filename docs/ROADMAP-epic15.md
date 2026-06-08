# Roadmap — Epic 15: images & responsive media

Broadens raster-image support. Today `<img>` decodes PNG/JPEG only (the `image`
crate is built with just those features), is stretched to fill its box by a manual
nearest-neighbour blit (no `object-fit`, no `image-rendering`), and there is no
responsive selection (`srcset`/`<picture>`) nor `.svg`-as-image nor media posters.
Epic 15 adds more formats, correct fitting/scaling, responsive source selection,
SVG-as-image, and `<video>`/`<audio>` poster/placeholder rendering.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Pages whose images don't use
the new features must stay byte-identical (existing tests + golden PNG unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E15-M1** | **Formats + fit/scale**: decode `gif` (first frame), `webp`, `bmp` (enable the `image` crate features); `object-fit` (`fill` (default) / `contain` / `cover` / `none` / `scale-down`) + `object-position` deciding how the decoded image maps into its content box; `image-rendering` (`auto`/`smooth` = bilinear vs `pixelated`/`crisp-edges` = nearest) controlling the blit's sampling. | `css`, `style`, `paint` | A non-square image with `object-fit: contain`/`cover` fits/crops correctly, `object-position` shifts it, `image-rendering: pixelated` keeps hard edges; gif/webp/bmp decode (tested + visual) | ☐ |
| **E15-M2** | **Responsive images**: `<img srcset>` density (`2x`) + width (`640w`) descriptors with `sizes`, and `<picture>` / `<source>` (`media`, `type`, `srcset`) selecting the best candidate against the render viewport (width + a fixed device-pixel-ratio); the chosen URL feeds the existing image pipeline. | `html`, `style`, `layout`, `paint` | Given `srcset`/`sizes` or a `<picture>` with `media`, the engine picks + renders the right source for the viewport (tested) | ☐ |
| **E15-M3** | **SVG-as-image + media**: `<img src="*.svg">` rendered through the existing inline-SVG painter into a pixmap (intrinsic size from `width`/`height`/`viewBox`); `<video poster>` renders the poster image (else a placeholder box), `<audio>`/posterless `<video>` render a simple placeholder; broken-image `alt` text shown for undecodable images. | `paint`, `layout` | An `<img>` of an SVG renders its shapes; `<video poster>` shows the poster; a broken image shows its `alt` (tested + visual) | ☐ |

## Non-goals (deferred)

- Animated GIF/WebP playback (only the first frame); APNG; AVIF/JXL.
- Actual video/audio decoding or playback, media controls UI, `<track>` captions.
- `<canvas>` (a separate epic); CSS `background-image` beyond the existing single
  linear-gradient (the `url()` background is a CSS-coverage item, not here).
- Color management / ICC profiles, EXIF orientation, HDR, wide-gamut.
- `srcset` with full `sizes` media-condition lists beyond the common forms; lazy
  loading / `loading=lazy` / `decoding`; `fetchpriority`.
- `image-set()` in CSS; `<map>`/`<area>` image maps; `ismap`/`usemap`.
- `object-fit` interaction with intrinsic aspect-ratio edge cases beyond the
  standard five keywords; `object-view-box`.
