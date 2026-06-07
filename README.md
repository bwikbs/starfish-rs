# starfish-rs

A from-scratch reimplementation of the [Starfish](https://github.com/Samsung/lightweight-web-engine) lightweight web engine, written in Rust.

**Goal (milestone 1 target):** render a static HTML page to a PNG — the classic browser rendering pipeline, no JavaScript yet.

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

**Epics 1–11 complete** — the engine fetches a page by URL, runs its JavaScript, and
renders the resulting HTML+CSS+SVG (grid, transforms, complex-script text shaping, bidi)
to PNG end to end, with shape/cascade/style caches keeping the hot paths cheap.
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

839 tests across the crates; `cargo clippy --all-targets -- -D warnings` is clean.
Roadmaps: [Epic 1](docs/ROADMAP.md), [Epic 2](docs/ROADMAP-epic2.md),
[Epic 3](docs/ROADMAP-epic3.md), [Epic 4](docs/ROADMAP-epic4.md),
[Epic 5](docs/ROADMAP-epic5.md), [Epic 6](docs/ROADMAP-epic6.md),
[Epic 7](docs/ROADMAP-epic7.md), [Epic 8](docs/ROADMAP-epic8.md),
[Epic 9](docs/ROADMAP-epic9.md), [Epic 10](docs/ROADMAP-epic10.md),
[Epic 11](docs/ROADMAP-epic11.md). Per-milestone design notes in
[docs/design/](docs/design/); rendered examples in [docs/examples/](docs/examples/).

## Approach

Built incrementally, one milestone at a time. Each milestone runs through a fixed pipeline of
specialized agents — **design → analysis → implementation → review → verification** — and lands as
its own commit. Third-party crates differ from the original C++ engine's (e.g. `tiny-skia` instead
of cairo/GL); the JavaScript engine (Escargot upstream) is deferred and abstracted behind an
interface for now.

## License

MIT — see [LICENSE](LICENSE).
