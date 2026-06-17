# Roadmap — Epic 59: animations round 3 (multiple / linear() / pseudo)

Extends the animation engine (E17: single `@keyframes`+`animation-*` sampled at
`--at`; `Easing` Linear/CubicBezier/Steps): multiple comma-separated animations,
the `linear()` easing + `steps()` jump keywords, and animations on
`::before`/`::after`.

Same per-milestone pipeline. Additive: a page with one (or no) animation samples
identically (golden + existing animation tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E59-M1** | **Multiple animations**: `animation: a 1s, b 2s` — `animation`/`animation-name`/`-duration`/… are comma-lists → `Vec<Animation>` on the style; `apply_animations` samples ALL of them (later ones win on a shared property). | `style` | `animation-name: spin, fade` with different durations both contribute at `--at`; a single animation is byte-identical (tested) | ✅ |
| **E59-M2** | **`linear()` + `steps()` jump keywords**: `linear(0, 0.25 25%, 1)` piecewise-linear easing; `steps(n, jump-start\|jump-end\|jump-both\|jump-none\|start\|end)`. | `style` | `linear()` interpolates through its points; `steps(4, jump-both)` differs from jump-end at the endpoints (tested) | ✅ |
| **E59-M3** | **Animations on pseudo-elements**: `::before`/`::after` `animation`/`transition` are sampled at `--at` like element animations (the pseudo's computed style gets its keyframe/transition override applied). | `style` | `p::before{animation:fade 1s}` samples the ::before opacity at --at (tested + visual) | ✅ |

## Non-goals (deferred)

- `animation-composition` (replace/add/accumulate) beyond parsing, and the
  `@keyframes`-`!important` ignore rule nuance.
- `linear()` with input-stops on both sides / extrapolation beyond [0,1]
  domain edge cases; `steps()` negative/zero count validation beyond a sane floor.
- Per-animation `animation-timeline`/scroll-driven (separate epic), and
  `transition` on pseudo beyond the single-property MVP.
