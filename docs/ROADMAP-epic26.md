# Roadmap — Epic 26: modern CSS color

The CSS Color 4/5 surface beyond `rgb()/hsl()`: the perceptual color spaces
(`oklch()`/`oklab()`/`lab()`/`lch()`), wider `color-mix()` interpolation (in
oklch/oklab/hsl with hue methods), and relative color syntax (`rgb(from …)`,
`oklch(from …)`). Everything still resolves to an 8-bit sRGB [`Rgba`] at parse
time (the engine's only color representation), so these are conversions on top of
the existing pipeline — no new color storage, no layout impact.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a page using only `rgb()`/`hsl()`/`#hex`/named colors must stay byte-identical
(existing tests + the golden PNG unchanged).

Current state (reference): `parse_color` (crates/css `color.rs`) handles `#hex`
(3/4/6/8), `rgb()/rgba()` (comma form, int/percent channels + 0..1 alpha),
`hsl()/hsla()` (comma form), ~16 named colors, and `transparent`. E24-M3 added
`color-mix(in srgb, …)` (premultiplied sRGB). There is **no** oklch/oklab/lab/
lch, no `color-mix` in any space but srgb, no hue interpolation, and no relative
color syntax.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E26-M1** | **Perceptual color spaces**: parse `oklch(L C H [/ A])`, `oklab(L a b [/ A])`, `lab(L a b [/ A])`, `lch(L C H [/ A])` and convert to sRGB [`Rgba`]. OKLab/OKLCh via the standard OKLab matrices (→ linear sRGB → gamma); Lab/LCh via CIE Lab → XYZ (D50) → Bradford-adapted → linear sRGB → gamma. `L` accepts `%` or number, `H` in deg, alpha `%`/number; out-of-gamut results are clipped to [0,255] per channel (MVP, no gamut mapping). Wire into both `parse_color` (verbatim color tokens in gradients etc.) and the parser's function dispatch. | `css` | `oklch(0.7 0.15 30)` and `lab(50% 40 30)` render the expected sRGB color; an `rgb()` page stays byte-identical (tested + visual) | ☐ |
| **E26-M2** | **Wider `color-mix()` + hue interpolation**: extend `color-mix(in <space>, A p%, B q%)` to `oklch`/`oklab`/`lab`/`lch`/`hsl`/`srgb`. Polar spaces (oklch/lch/hsl) interpolate hue with the `shorter`/`longer`/`increasing`/`decreasing` hue methods (default `shorter`); rectangular spaces interpolate components linearly in that space. Mixing is premultiplied by alpha; the existing weight/percentage normalization (E24-M3) is reused. | `css` | `color-mix(in oklch, white, black)` differs from the srgb mix (perceptual midpoint); `color-mix(in oklch longer hue, red, red)` ≠ `red`; the srgb form is unchanged (tested) | ☐ |
| **E26-M3** | **Relative color syntax**: `rgb(from <color> <r> <g> <b> [/ <a>])` and the same `from` form for `hsl()`, `oklch()`, `oklab()`, `lab()`, `lch()`. The origin color is decomposed into that space's channels, exposed as the named components (`r`/`g`/`b`/`alpha`, `h`/`s`/`l`, `l`/`c`/`h`, …), and each output channel is an expression over them (a bare channel keyword, a number, a percentage, or `calc()` over the channel keywords). `light-dark(a, b)` resolves to its first argument (light scheme, the engine default). | `css` | `rgb(from red 0 g b)` drops the red channel (→ black, since g=b=0 for red); `oklch(from red calc(l * 0.5) c h)` darkens red; `light-dark(red, blue)` is red (tested) | ☐ |

## Non-goals (deferred)

- Gamut mapping (CSS Color 4 §13): out-of-sRGB results are naively per-channel
  clipped, not chroma-reduced to the gamut boundary.
- `color()` with explicit color-space profiles (`color(display-p3 …)`,
  `color(rec2020 …)`, `color(xyz …)`); wide-gamut output (always 8-bit sRGB).
- `none` color components and their carry-forward through interpolation;
  `currentColor`/system colors inside the modern functions.
- `color-contrast()`, `device-cmyk()`, and the `contrast-color()` function.
- In relative color, channel expressions beyond a bare channel keyword / number /
  percentage / `calc()` over channel keywords (no nested color functions in the
  channel slots, no `var()` inside the channel expression).
- `@property` typed custom properties and `light-dark()` actually following a
  `prefers-color-scheme` / `color-scheme` toggle (always the light/first value).
