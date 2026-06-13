# Roadmap — Epic 24: CSS design-system functions

The modern CSS value/at-rule functions used by design systems: the math
comparison functions (`min()`/`max()`/`clamp()`, `round()`/`mod()`/`rem()`),
feature/cascade at-rules (`@supports`, `@layer`), and color/environment value
functions (`color-mix()`, `env()`, expanded `attr()`).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive,
so pages not using it must stay byte-identical (existing tests + the golden PNG
unchanged).

Current state (reference): `calc()` is supported as a linear `px + percent` form
(E13-M2), with custom properties + `var()`; `@media` queries cascade (E13-M3);
`@supports`/`@layer` are dropped (unknown at-rules skipped); `attr()` works only
in `content` (E7-M2). There is no `min()`/`max()`/`clamp()` (they're dropped, so
a `width: clamp(...)` falls back to auto), no `color-mix()`, no `env()`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E24-M1** | **Math comparison functions**: `min(...)`/`max(...)`/`clamp(min, val, max)` over the existing `calc()` linear `px + percent` model — each argument is a calc-expression, evaluated against the containing-block basis at resolve time, then compared (min/max pick the extreme; clamp = max(min, min(val, max))); `min`/`max`/`clamp` nest with `calc()` and each other; basic `round(strategy?, a, b)`/`mod(a,b)`/`rem(a,b)` for pure numbers/lengths. Works anywhere a length/percentage is accepted (width/height/margin/padding/font-size/gap/…). | `css`, `style` | `width: clamp(200px, 50%, 600px)` resolves to the clamped value at different container widths; `min()`/`max()` pick the right extreme; nesting with `calc()` works (tested + visual) | ✅ |
| **E24-M2** | **`@supports` + `@layer`**: `@supports (prop: value)` / `@supports not (...)` / `and`/`or` — evaluate whether a declaration is supported (the property is known + the value parses) and include the block's rules only if so; `@layer name { … }` + the `@layer a, b;` ordering statement — cascade layers ordering (rules in an earlier-declared layer lose to later layers; unlayered wins over layered), interleaved at the right cascade priority. | `css`, `style` | A `@supports (display: grid)` block applies (grid is supported) while `@supports (display: nonsense)` doesn't; `@layer base, theme` makes a `theme`-layer rule win over a `base`-layer one regardless of source order (tested) | ✅ |
| **E24-M3** | **Color & environment functions**: `color-mix(in srgb, A p%, B q%)` — mix two colors in sRGB by weight (the common `in srgb` form); `env(name, fallback)` — environment variables (`safe-area-inset-*` etc) resolving to the fallback (or 0) since there's no real device chrome; expanded `attr()` — `attr(name type, fallback)` usable beyond `content` (e.g. `attr(data-w px)` in a width), with the type/unit + fallback. | `css`, `style` | `color-mix(in srgb, red, blue)` renders purple; `env(safe-area-inset-top, 10px)` resolves to 10px; `attr(data-w px)` sets a width from the attribute (tested + visual) | ✅ |

## Non-goals (deferred)

- `calc()` beyond the linear `px + percent` form (no nested unit algebra across
  arbitrary units, no `*`/`/` by non-scalars beyond the existing support);
  `min/max/clamp` mixing incompatible unit types beyond px/percent.
- `round()` all rounding strategies' edge cases, `mod`/`rem` sign subtleties,
  `sin`/`cos`/`tan`/`pow`/`sqrt`/`hypot`/`log`/`exp` trig-and-exponential
  functions, `abs()`/`sign()`.
- `@supports selector(...)`, `@supports font-tech()/font-format()`; `@layer` with
  nested layers, `@import ... layer(...)`, anonymous layers' full ordering, and
  revert-layer.
- `color-mix()` in color spaces other than `srgb` (no oklch/lab/hsl interpolation,
  no hue interpolation methods, no `none` components); relative color syntax
  (`rgb(from …)`), `light-dark()`, system colors.
- `env()` real values (insets are always the fallback/0 — no device safe areas);
  `attr()` advanced type grammar beyond a unit keyword + fallback.
