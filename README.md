# starfish-rs

A from-scratch reimplementation of the [Starfish](https://github.com/Samsung/lightweight-web-engine) lightweight web engine, written in Rust.

It fetches a page (file/http(s)/data), parses HTML+CSS, runs JavaScript (Boa), and renders the result to a PNG — the classic browser rendering pipeline, built up over 53 milestone epics:

```
HTML  ──parse──▶  DOM  ──┐
                         ├─▶  Styled tree  ──layout──▶  Box tree  ──paint──▶  PNG
CSS   ──parse──▶  CSSOM ─┘
```

## Workspace layout

| Crate | Responsibility |
|-------|----------------|
| `starfish-dom`    | DOM node tree, node types |
| `starfish-html`   | HTML tokenizer + tree builder → DOM |
| `starfish-css`    | CSS tokenizer + parser → stylesheets |
| `starfish-style`  | Cascade, specificity, computed values → styled tree |
| `starfish-layout` | Box tree, block/inline layout |
| `starfish-paint`  | Rasterize via `tiny-skia` → PNG |
| `starfish-cli`    | `starfish` binary: drives the pipeline |

## Status

**Epics 1–53 complete** — the engine fetches a page by URL, runs its JavaScript, and
renders the resulting HTML+CSS+SVG to PNG end to end, with shape/cascade/style/layout
caches keeping the hot paths cheap. The supported surface spans, among much else:

- **Layout** — block/inline flow, floats, positioning (rel/abs/fixed/sticky), flexbox,
  CSS Grid (incl. subgrid, masonry, `minmax()`/`fit-content()`/intrinsic tracks,
  `repeat(auto-fill/auto-fit)`, dense flow), tables (collapsed borders, `table-layout:
  fixed`, caption, `<col>`), multi-column, vertical writing modes, `display: contents`/
  `flow-root`, containment, `aspect-ratio`, box-sizing, min/max sizing.
- **CSS** — the full cascade with `@media`/`@container`/`@supports`/`@layer`/nesting,
  `calc()`+math functions (trig/`sqrt`/`pow`/…), `var()`/`env()`/`@property`, logical
  properties, modern color (`oklch`/`lab`/`color-mix`/relative color/`color()`/system
  colors/148 named), 2D+flattened-3D transforms, filters & blend/isolation, masking
  (multi-layer, clip/origin, `clip-path`), gradients (linear/radial/conic + repeating),
  `image-set()`/`cross-fade()`, `@keyframes`/`transition` animation (sampled at `--at`),
  selectors (`:is`/`:where`/`:has`/structural/state/validity/`:popover-open`),
  pseudo-elements (`::before`/`::after`/`::marker`/`::placeholder`/`::first-letter`),
  counters/`@counter-style`/`quotes`, scrolling + scrollbars, the form/UI properties
  (`accent-color`/`appearance`/`all`/…).
- **Text** — `rustybuzz` shaping (kerning/ligatures, Arabic joining, combining marks,
  per-cluster font fallback), bidi/RTL, `@font-face`, font features/variants/variable-font
  axes, justification/hyphenation/line-clamp, text-decoration (style/thickness/offset) and
  `text-emphasis`.
- **Content** — inline SVG (shapes/path/text/gradients/`<use>`/`<symbol>`/`<pattern>`/
  `<clipPath>`), `<img>` + responsive `srcset`/`<picture>`, `<canvas>` 2D, native form
  controls + `<progress>`/`<meter>`/`<fieldset>`, interactive `<details>`/`<dialog>`/popover,
  Shadow DOM + slots + `customElements`.
- **JS** — Boa runs `<script>`s against a broad DOM/web-API surface: events, timers,
  `fetch`/XHR, storage, geometry, `requestAnimationFrame`, observers (Mutation/Resize/
  Intersection), `structuredClone`/`queueMicrotask`/`AbortController`, CSSOM
  (`styleSheets`), `NodeIterator`/`TreeWalker`, `DOMParser`/`XMLSerializer`.

Render a local file, or a remote URL:

```sh
cargo run -p starfish-cli -- render docs/examples/demo.html -o out.png --width 640
cargo run -p starfish-cli -- render https://example.com -o out.png --width 800
```

![demo render](docs/examples/demo.png)

**Epic 1** (static pipeline): HTML→DOM, CSS parsing, the cascade, block + inline
flow layout, paint to PNG.
**Epic 2** (coverage + images): text-decoration, list markers, real inline-block,
float/clear, position (relative/absolute/fixed), flexbox, `<img>` (PNG/JPEG),
linear-gradient backgrounds, border-radius, box-shadow, opacity.
**Epic 3** (networking & resources): a `ResourceLoader` over `file://`, `http(s)://`
(ureq + rustls), and `data:` URLs; `<link rel=stylesheet>` loaded in document order;
remote/`data:` images; an in-memory fetch cache.
**Epic 4** (JavaScript): the Boa engine runs `<script>`s against DOM bindings
(document/Element/Node, querySelector, createElement, element.style), with events
(addEventListener/dispatchEvent, DOMContentLoaded/load) and bounded setTimeout/
setInterval; the run-to-quiescence DOM state is what gets rendered.
**Epic 5** (Grid & transforms): CSS Grid (`grid-template-columns/rows` with fr/repeat,
gap, line/area placement, alignment) and 2D `transform` (translate/scale/rotate/skew/
matrix + transform-origin, paint-time).
**Epic 6** (Text & typography): real font selection (fontdb system fonts + vendored
fallback, font-family/style/weight, per-face metrics), `@font-face` web fonts loaded over
the ResourceLoader, and bidi/RTL reordering + letter/word-spacing + text-transform +
white-space (pre/nowrap/pre-wrap/pre-line).
**Epic 7** (CSS coverage): selector expansion (attribute, structural pseudo-classes,
sibling combinators), `::before`/`::after` generated content (`content` string/`attr()`),
and table layout (`display:table`, colspan/rowspan, border-spacing).
**Epic 8** (JS web APIs): `innerHTML`/`outerHTML`/`getComputedStyle`/`cloneNode`/
`insertAdjacentHTML`, `fetch()` + sync `XMLHttpRequest` over the ResourceLoader, and
`localStorage`/`sessionStorage`/`JSON`/`dataset`/`URL`/`URLSearchParams`.
**Epic 9** (inline SVG): foreign-content parsing; `<svg>`/`viewBox`; shapes (rect/circle/
ellipse/line), `<path>` (full command set incl. arcs), `polygon`/`polyline`; `<text>`/
`<tspan>`, `<g>` + `transform`, and `linearGradient`/`radialGradient` fills.
**Epic 10** (complex-script shaping): real text shaping via `rustybuzz` — Latin kerning +
ligatures, Arabic joining forms + RTL bidi placement, combining-mark positioning, and
per-cluster font fallback (a cluster the primary face lacks is reshaped with a covering
face; vendored Noto Sans Arabic + Noto Sans Devanagari, OFL).
**Epic 11** (performance): pure-optimization caches, each byte-identical — a `rustybuzz`
shape cache (one shape per run, not per measure+paint), a per-element selector-match
cascade cache, and a DOM-version-keyed styled-tree memo so `getComputedStyle` rebuilds
only after a real DOM mutation.
**Epic 12** (incremental box-layout): a per-`layout()` `LayoutCache` memoizes the intrinsic
`measure_*` calls (table/grid/flex measure the same subtree 2–3× across the column/row/final
passes) by `(NodeId, MeasureKind, width)` — byte-identical, layout once per node not 2–3×.
**Epic 13** (CSS expansion): `box-sizing` + `min/max-width/height`; `calc()` + custom
properties (`--x`/`var()`); `@media` queries + viewport units (`vw`/`vh`/`vmin`/`vmax`);
`overflow:hidden`/`clip` clipping, `hsl()`/`hsla()` + `#rrggbbaa` color, and
`dashed`/`dotted`/`double` borders.
**Epic 14** (form controls): native `<input>` (text/checkbox/radio/color/range), `<textarea>`,
`<button>`, `<select>` rendering with values/labels/state; form-state pseudo-classes
(`:checked`/`:disabled`/`:enabled`/`:required`/`:read-only`), `disabled` greying, hidden inputs.
**Epic 15** (images & media): gif/webp/bmp decode, `object-fit`/`object-position`/`image-rendering`,
responsive `srcset`/`sizes` + `<picture>`, SVG-as-`<img>` (vector), `<video>`/`<audio>`
poster/placeholder, and broken-image `alt` text.
**Epic 16** (CSS coverage, round 2): `:is()`/`:where()`/`:has()` selectors + CSS counters
(`counter-reset`/`-increment`, `counter()`/`counters()`); `background-image: url(...)` with
`background-size`/`-position`/`-repeat` + multiple comma-separated layers; `radial-gradient`/
`conic-gradient`, `text-shadow`, `outline`; `text-overflow: ellipsis` and `position: sticky`.
**Epic 17** (animations & transitions): `@keyframes` + `animation-*` + easing
(`cubic-bezier`/`steps`) sampled as a static frame at a `--at <seconds>` clock, value
interpolation (opacity/color/length/transform/border-color/border-radius/box-shadow), full
timing (delay/iteration/direction/fill-mode), and JS-driven `transition`s (pre-script
snapshot + second cascade, from→to sampled at the clock).
**Epic 18** (layout, round 3): `aspect-ratio` (derive the missing axis), flexbox `gap`,
CSS multi-column (`column-count`/`-width`/`-gap`/`-rule`, greedy column balance), and
vertical writing modes (`writing-mode: vertical-rl`/`vertical-lr` + `text-orientation:
upright`/`sideways`, with block flow on the horizontal axis and lines running down).
**Epic 19** (JS DOM & web-API expansion): `classList`/`closest`/`matches`/modern insertion
(`append`/`before`/…), layout geometry (`getBoundingClientRect`/`offsetWidth`/… via
on-demand mid-script layout), and `requestAnimationFrame`/`MutationObserver`/`history`+
`location`/`matchMedia`.
**Epic 20** (`<canvas>` 2D): `getContext('2d')` + `CanvasRenderingContext2D` (rect/path
fills, `save`/`restore`, transforms, linear/radial gradients, `globalAlpha`, line
cap/join/dash, `clip`, `fillText`/`measureText`, `drawImage`) — the context records a
display list (in `dom`) that the painter replays into the canvas box via `tiny-skia`.
**Epic 21** (filters & compositing): CSS `filter` (blur/brightness/contrast/grayscale/
sepia/invert/saturate/hue-rotate/opacity/drop-shadow), blend modes (`mix-blend-mode`/
`background-blend-mode`), `mask-image` (alpha/luminance), and `backdrop-filter` — all via
the offscreen-layer model.
**Epic 22** (text & typography): `word-break`/`overflow-wrap`/`tab-size`/`white-space:
break-spaces` line-breaking, `text-align: justify`/`text-justify`/`text-indent`,
`-webkit-line-clamp`, and `hyphens: manual` (soft-hyphen breaking) — plus a fix for
the `transparent` color keyword in gradient stops.
**Epic 23** (HTML entities & parser robustness): full named + numeric character
references (`&copy;`/`&mdash;`/`&#x1F600;`/…) in text and attributes, implied/optional
end tags (auto-closing `<p>`/`<li>`/table cells/…), and RAWTEXT/RCDATA content modes
(`<script>`/`<style>`/`<textarea>`/`<title>`).
**Epic 24** (design-system functions): CSS math functions `min()`/`max()`/`clamp()`/
`round()`/`mod()`/`rem()`, and `attr()` typed values + `env()`.
**Epic 25** (container queries & logical properties): `@container` size queries +
`container-type`/`-name` + `cq*` units, and flow-relative `margin`/`padding`/`border`/
`inset`-`inline`/`-block` logical box properties.
**Epic 26** (modern color): `oklch()`/`oklab()`/`lab()`/`lch()`, `color-mix()`, and
relative color syntax (`rgb(from …)`).
**Epic 27** (media query expansion): `aspect-ratio`/`resolution`/`orientation` media
features and richer query combinators.
**Epic 28** (CSS Nesting): `&` nesting (explicit/implicit/multi-level) + nested `@media`.
**Epic 29** (selectors & pseudo-classes): type-indexed/last structural pseudos,
`:nth-child(of S)`, link/UI pseudo-classes.
**Epic 30** (`@property`): typed custom properties — parse + `syntax`-validated
`initial-value` + `var()` fallback.
**Epic 31** (layout deepening): `content-visibility`/`contain` containment, masonry
layout, and grid **subgrid**.
**Epic 32** (graphics effects): `clip-path` basic shapes, mask positioning
(`mask-position`/`-size`/`-repeat`/`-clip`/`-origin`), and `isolation: isolate` +
the non-separable blend modes.
**Epic 33** (Shadow DOM & Web Components): `attachShadow` + a composed-tree render
(declarative `<template shadowrootmode>`), `<slot>` distribution, scoped styling
(`:host`/`::slotted` + `<style>` encapsulation), and `customElements.define` upgrades.
**Epic 34** (display model): `display: contents` (children splice into the parent flow;
`slot{display:contents}`), `display: flow-root` (a clean BFC), and the `display`
two-value syntax.
**Epic 35** (pseudo-elements, round 2): `::marker`, `::placeholder`, `::first-letter`.
**Epic 36** (interactive elements): `<details>`/`<summary>` (open/closed + disclosure
marker), `<dialog>` + the `hidden` attribute, and the Popover API (`popover` +
`:popover-open` + `showPopover`/`hidePopover`/`togglePopover`).
**Epic 37** (scrolling & scrollbars): `overflow: scroll`/`auto` with a painted overlay
scrollbar, `scrollTop`/`scrollLeft` content offset, and `scrollbar-width`/`-color` +
`scroll-snap`/`scroll-behavior` parsing.
**Epic 38** (SVG, round 2): `<use>`/`<symbol>`/`<defs>` instancing, `<pattern>` fills,
and `<clipPath>` + the `clip-path` attribute.
**Epic 39** (forms, round 2): `<progress>`/`<meter>` bars, `<fieldset>`/`<legend>`/
`<datalist>`, and the validity pseudo-classes (`:valid`/`:invalid`/`:in-range`/
`:out-of-range`/`:optional`).
**Epic 40** (tables, round 2): `border-collapse: collapse`, `table-layout: fixed`,
`<caption>` placement, and `<col>`/`<colgroup>` widths.
**Epic 41** (text decoration & emphasis, round 3): `text-decoration-color`/`-style`
(`solid`/`double`/`dotted`/`dashed`/`wavy`)/`-thickness` + `text-underline-offset`, and
`text-emphasis`.
**Epic 42** (CSS values, round 3): trig/`sqrt`/`pow`/`hypot`/`abs`/`sign` math functions
+ `pi`/`e`, `env()` everywhere `var()` works, and `@counter-style`.
**Epic 43** (JS, round 3): `ResizeObserver`, `IntersectionObserver`, and
`structuredClone`/`queueMicrotask`/`AbortController`.
**Epic 44** (color, round 3): the full ~148 named colors, CSS system colors, and the
`color()` function + `hwb()`.
**Epic 45** (transforms, round 2): individual `translate`/`rotate`/`scale` properties,
flattened 3D functions (`rotateX`/`rotateY`/`translate3d`/`matrix3d`/…), and
`perspective` + `backface-visibility`.
**Epic 46** (fonts, round 3): `font-feature-settings`/`font-kerning`, the `font-variant-*`
longhands, and variable-font `font-variation-settings`.
**Epic 47** (backgrounds & masking, round 2): `background-clip`/`-origin`,
`background-attachment` + `background-clip: text`, and multi-layer masks + the `mask`
shorthand.
**Epic 48** (images & gradients, round 3): `repeating-linear`/`-radial`/`-conic-gradient`,
`image-set()`, and `cross-fade()`.
**Epic 49** (CSS parser & value robustness, round 2): CSS escape decoding (`\25AA` →
the codepoint, in strings + idents), gradient color-stop lengths (`px`/`em` + double
position), and axis-aware `<position>` keyword parsing.
**Epic 50** (CSS Grid, round 3): `minmax()`, `min-content`/`max-content`/`fit-content()`
tracks, `repeat(auto-fill/auto-fit)`, and `grid-auto-flow: dense`.
**Epic 51** (form-control & UI styling): `accent-color`, `appearance: none`/`auto`, and
`caret-color`/`pointer-events`/`all`.
**Epic 52** (CSSOM & DOM traversal): `document.styleSheets`/`cssRules`, `NodeIterator`/
`TreeWalker`, and `DOMParser`/`XMLSerializer`.
**Epic 53** (lists & generated content, round 2): `list-style-image`, `content: url()`/
`none`, and `quotes` + `open-quote`/`close-quote`.

1740 tests across the crates; `cargo clippy --all-targets -- -D warnings` is clean.
Roadmaps: [Epic 1](docs/ROADMAP.md), [Epic 2](docs/ROADMAP-epic2.md),
[Epic 3](docs/ROADMAP-epic3.md), [Epic 4](docs/ROADMAP-epic4.md),
[Epic 5](docs/ROADMAP-epic5.md), [Epic 6](docs/ROADMAP-epic6.md),
[Epic 7](docs/ROADMAP-epic7.md), [Epic 8](docs/ROADMAP-epic8.md),
[Epic 9](docs/ROADMAP-epic9.md), [Epic 10](docs/ROADMAP-epic10.md),
[Epic 11](docs/ROADMAP-epic11.md), [Epic 12](docs/ROADMAP-epic12.md),
[Epic 13](docs/ROADMAP-epic13.md), [Epic 14](docs/ROADMAP-epic14.md),
[Epic 15](docs/ROADMAP-epic15.md), [Epic 16](docs/ROADMAP-epic16.md),
[Epic 17](docs/ROADMAP-epic17.md), [Epic 18](docs/ROADMAP-epic18.md),
[Epic 19](docs/ROADMAP-epic19.md), [Epic 20](docs/ROADMAP-epic20.md),
[Epic 21](docs/ROADMAP-epic21.md), [Epic 22](docs/ROADMAP-epic22.md),
[Epic 23](docs/ROADMAP-epic23.md), [Epic 24](docs/ROADMAP-epic24.md),
[Epic 25](docs/ROADMAP-epic25.md), [Epic 26](docs/ROADMAP-epic26.md),
[Epic 27](docs/ROADMAP-epic27.md), [Epic 28](docs/ROADMAP-epic28.md),
[Epic 29](docs/ROADMAP-epic29.md), [Epic 30](docs/ROADMAP-epic30.md),
[Epic 31](docs/ROADMAP-epic31.md), [Epic 32](docs/ROADMAP-epic32.md),
[Epic 33](docs/ROADMAP-epic33.md), [Epic 34](docs/ROADMAP-epic34.md),
[Epic 35](docs/ROADMAP-epic35.md), [Epic 36](docs/ROADMAP-epic36.md),
[Epic 37](docs/ROADMAP-epic37.md), [Epic 38](docs/ROADMAP-epic38.md),
[Epic 39](docs/ROADMAP-epic39.md), [Epic 40](docs/ROADMAP-epic40.md),
[Epic 41](docs/ROADMAP-epic41.md), [Epic 42](docs/ROADMAP-epic42.md),
[Epic 43](docs/ROADMAP-epic43.md), [Epic 44](docs/ROADMAP-epic44.md),
[Epic 45](docs/ROADMAP-epic45.md), [Epic 46](docs/ROADMAP-epic46.md),
[Epic 47](docs/ROADMAP-epic47.md), [Epic 48](docs/ROADMAP-epic48.md),
[Epic 49](docs/ROADMAP-epic49.md), [Epic 50](docs/ROADMAP-epic50.md),
[Epic 51](docs/ROADMAP-epic51.md), [Epic 52](docs/ROADMAP-epic52.md),
[Epic 53](docs/ROADMAP-epic53.md). Per-milestone design notes in
[docs/design/](docs/design/); rendered examples in [docs/examples/](docs/examples/).

## Approach

Built incrementally, one milestone at a time. Each milestone runs through a fixed pipeline of
specialized agents — **design → analysis → implementation → review → verification** — and lands as
its own commit, every feature additive (pages not using it render byte-identically; the golden PNG
+ existing tests stay green). Third-party crates differ from the original C++ engine's (e.g.
`tiny-skia` instead of cairo/GL, the Boa JS engine instead of Escargot).

## License

MIT — see [LICENSE](LICENSE).
