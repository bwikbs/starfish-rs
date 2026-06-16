# Roadmap — Epic 44: color round 3 (named / system / color())

Rounds out the color parser (`crates/css/src/color.rs`: `#hex`, rgb/hsl/oklch,
`color-mix`, relative `from`) with the FULL named-color set, CSS system colors,
and the `color()` function.

Same per-milestone pipeline. Additive: the existing ~16 named colors + existing
functions keep their values (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E44-M1** | **Full named colors**: extend `named()` from the ~16 basic colors to the complete CSS named-color list (~148, incl. `rebeccapurple`, `transparent`). | `css` | `color: rebeccapurple` / `tomato` / `darkslategray` resolve to their exact RGB; the existing basic colors are unchanged (tested) | ☐ |
| **E44-M2** | **System colors**: `Canvas`/`CanvasText`/`LinkText`/`VisitedText`/`ActiveText`/`ButtonFace`/`ButtonText`/`ButtonBorder`/`Field`/`FieldText`/`GrayText`/`Highlight`/`HighlightText`/`Mark`/`MarkText` resolve to sensible light-theme RGB (case-insensitive). | `css` | `color: CanvasText` → near-black, `background: Canvas` → white, `GrayText` → grey (tested) | ☐ |
| **E44-M3** | **`color()` function**: `color(srgb r g b [/ a])`, `color(srgb-linear …)`, `color(display-p3 …)` (and `hwb()` if missing) parse to an sRGB `Rgba` (out-of-gamut clamped); channels are 0..1 numbers or percentages. | `css` | `color(srgb 1 0 0)` = red; `color(display-p3 0 1 0)` ≈ green (clamped); `color(srgb 1 1 1 / .5)` = 50% white (tested) | ☐ |

## Non-goals (deferred)

- Wide-gamut fidelity for `display-p3`/`rec2020`/`a98-rgb`/`prophoto-rgb` beyond a
  best-effort matrix→sRGB (or treating the channels as sRGB) with gamut clamp.
- `color-contrast()` selection, `lab`/`lch`/`oklab` already shipped (E26),
  `light-dark()`, and `@media (prefers-color-scheme)` driving system colors.
- Dark-theme system color values / `forced-colors` mode.
