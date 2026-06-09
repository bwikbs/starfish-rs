# Roadmap — Epic 17: CSS animations & transitions

CSS `@keyframes` animations and `transition`s, rendered as a **static frame at a
chosen time**. The engine renders one-shot, so an animation is sampled at a
single instant: a new CLI flag `--at <seconds>` (default `0`) sets the global
animation clock, and the style pipeline resolves each animated property to its
interpolated value at that time. `@keyframes` animations run automatically on
load, so they're fully renderable one-shot; `transition`s only fire on a property
change (a JS style mutation), so they're sampled the same way once a change has
been recorded.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Where a feature is purely
additive, pages without animations/transitions must stay byte-identical (existing
tests + the golden PNG unchanged) — and a page rendered at `--at 0` with an
animation in its `from`/0% state must match the equivalent static page.

Current state (reference): `transform` (translate/scale/rotate/skew/matrix),
`opacity`, colors, and lengths are all computed statically. No `@keyframes` rule
is parsed (the CSS parser drops unknown at-rules), no `animation`/`transition`
properties exist, no easing functions, no value interpolation, and the CLI has no
time input. The render is a single frame with no clock.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E17-M1** | **Timing model + interpolation core**: parse `@keyframes name { 0%{…} 100%{…} }` (css); `animation-name`/`-duration`/`-timing-function`/`-delay`/`-iteration-count`/`-direction`/`-fill-mode` longhands + the `animation` shorthand (style); easing functions `linear`/`ease`/`ease-in`/`-out`/`-in-out`/`cubic-bezier(...)`/`steps(...)`; a global animation clock via a new CLI `--at <seconds>` flag threaded into style resolution; **value interpolation** for `opacity`, color (`color`/`background-color`), and lengths — at time `t` the active keyframe pair is found and the property lerped. | `css`, `style`, `cli` | An element with `@keyframes` animating opacity/color/a length renders its correctly-interpolated value at `--at t` (tested + visual); `--at 0` ≡ the `0%` frame; a no-animation page is byte-identical | ☐ |
| **E17-M2** | **Transform interpolation + full timing**: interpolate `transform` lists (componentwise translate/scale/rotate/skew when the function lists match; matrix fallback otherwise) so a `rotate`/`translate` animation tweens; honour `animation-delay` (incl. negative), `animation-iteration-count` (incl. `infinite` sampled at `t`), `animation-direction` (`normal`/`reverse`/`alternate`/`alternate-reverse`), `animation-fill-mode` (`none`/`forwards`/`backwards`/`both`), and per-keyframe `offset` stops (`0% 50% 100%`, multi-selector `0%,100%`). | `style`, `paint` | A `@keyframes` transform animation tweens its transform at `t`; delay/iteration/direction/fill-mode resolve to the right frame (tested + visual) | ☐ |
| **E17-M3** | **Transitions + property coverage**: `transition-property`/`-duration`/`-timing-function`/`-delay` + the `transition` shorthand; when a JS script mutates a transitioned property the engine records the from→to pair and samples the transition at `--at t` (documenting the one-shot model: with no change recorded a transition is inert); broaden interpolatable properties to `border-color`, `width`/`height`, `margin`/`padding`, and `box-shadow`/`border-radius` basics. | `css`, `style`, `js` | A JS class change that triggers a `transition` renders the mid-transition value at `t`; the extra properties interpolate (tested + visual) | ☐ |

## Non-goals (deferred)

- A real animation loop / multi-frame output / GIF or video export (one-shot
  renders a single sampled frame; `--at` picks the instant).
- `animation-composition`, `animation-timeline`/scroll-driven animations, the
  `@scroll-timeline`/`view()`/`scroll()` functions, `@property` typed custom
  properties, and CSS `@keyframes` `!important` overrides.
- `transition`s driven by real user interaction (`:hover`/`:focus` state changes
  with no JS) — there is no interaction in a one-shot render; only JS-recorded
  property changes are sampled.
- Interpolating discrete/non-animatable properties, `visibility` timing, mode
  switches mid-animation, and `auto`↔length interpolation (treated as discrete).
- Matrix-decomposition interpolation precision (Slerp on rotation, full
  Unmatrix); the transform tween is componentwise when function lists match and a
  midpoint matrix otherwise.
- Spring/`linear()` easing with control points, `steps()` jump edge variants
  beyond the common `jump-end`/`jump-start`/`start`/`end`.
- `will-change`, compositing hints, and any GPU-layer behavior.
