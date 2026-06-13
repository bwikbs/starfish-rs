# Roadmap — Epic 27: media query expansion

Rounds out `@media` beyond the `min-`/`max-` width/height + orientation subset:
the **range syntax** (`(width >= 400px)`, `(400px <= width < 800px)`), the
**user-preference features** (`prefers-color-scheme`, `prefers-reduced-motion`,
`prefers-contrast`, `pointer`, `hover`), and the remaining **dimensional
features** (`aspect-ratio`, `resolution`). User preferences come from a render
context (a `MediaPrefs` carried on the `Viewport`, set by CLI flags; defaults
match a typical desktop: light, no-preference, fine pointer, hover).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a page with no `@media` (or only the existing width/height/orientation queries)
must stay byte-identical (existing tests + the golden PNG unchanged).

Current state (reference): `@media` parses a comma list of conditions, each a
media type + `and`-joined features, optional `not` (E13-M3). Modeled features:
`min-width`/`max-width`/`min-height`/`max-height` (px only) + `orientation`
(crates/css `MediaFeature`); evaluated against `Viewport { width, height }` in
crates/style `media.rs`. There is **no** range syntax (`width >= …` → `Unknown`,
never matches), no preference features, no `aspect-ratio`/`resolution`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E27-M1** | **Range syntax**: parse `(<feature> <op> <value>)` and `(<value> <op> <feature> <op> <value>)` for `width`/`height` with `<`/`<=`/`>`/`>=`/`=` (e.g. `(width >= 400px)`, `(400px <= width < 800px)`), alongside the existing `min-`/`max-` prefix form. Model as a normalized range feature `{ axis, lower?, upper? }` evaluated against the viewport. `min-width: N` ≡ `width >= N`, `max-width: N` ≡ `width <= N`. | `css`, `style` | `@media (width >= 400px)` matches at 400px+ but not below; `@media (400px <= width < 800px)` matches in `[400, 800)`; the `min-`/`max-` forms are unchanged (tested) | ☐ |
| **E27-M2** | **User-preference features**: a `MediaPrefs` (color-scheme light/dark, reduced-motion, contrast, pointer fine/coarse/none, hover) on the render `Viewport`, defaulting to desktop (light / no-preference / fine / hover). Evaluate `prefers-color-scheme: dark\|light`, `prefers-reduced-motion: reduce\|no-preference`, `prefers-contrast: more\|less\|no-preference`, `pointer: fine\|coarse\|none`, `hover: hover\|none`. CLI flags (`--color-scheme dark`, `--reduced-motion`, …) set them so a render can target a preference. | `css`, `style`, `cli` | `@media (prefers-color-scheme: dark)` applies only when the render requests dark; `@media (pointer: coarse)` doesn't match the default fine pointer; default render byte-identical (tested + visual) | ☐ |
| **E27-M3** | **Dimensional features**: `aspect-ratio` as a range feature (`(min-aspect-ratio: 16/9)`, `(aspect-ratio >= 1/1)`) against the viewport's width/height ratio; `resolution` (`(min-resolution: 2dppx)`) against a render DPR (default 1, CLI-settable). Also `(orientation)` already exists — fold it under the same evaluator. | `css`, `style`, `cli` | `@media (min-aspect-ratio: 1/1)` matches a landscape viewport; `@media (min-resolution: 2dppx)` matches only at DPR≥2 (tested) | ☐ |

## Non-goals (deferred)

- `@media` boolean context / `(width)` existence test without a value;
  `<` vs `<=` distinction beyond the two width/height/aspect-ratio axes.
- Real device-derived preferences: `MediaPrefs` only changes via CLI flags, not
  any OS/browser signal; `light-dark()` still always resolves to its light value
  (it folds at parse time, before the viewport/prefs exist).
- `update`, `overflow-block`/`overflow-inline`, `scripting`, `color-gamut`,
  `forced-colors`, `inverted-colors`, `prefers-reduced-data`/`-transparency`.
- `resolution` units beyond `dppx`/`x` (no `dpi`/`dpcm` conversion); fractional
  `resolution` ranges' exact rounding.
- Container queries already cover the per-element case (E25); this epic is the
  viewport-level `@media` only.
