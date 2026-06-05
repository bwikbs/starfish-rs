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

**Epics 1–3 complete** — the engine fetches a page by URL and renders HTML+CSS to
PNG end to end (no JavaScript). Render a local file, or a remote URL:

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

350 tests across the crates; `cargo clippy --all-targets -- -D warnings` is clean.
Roadmaps: [Epic 1](docs/ROADMAP.md), [Epic 2](docs/ROADMAP-epic2.md),
[Epic 3](docs/ROADMAP-epic3.md). Per-milestone design notes in
[docs/design/](docs/design/); rendered examples in [docs/examples/](docs/examples/).

## Approach

Built incrementally, one milestone at a time. Each milestone runs through a fixed pipeline of
specialized agents — **design → analysis → implementation → review → verification** — and lands as
its own commit. Third-party crates differ from the original C++ engine's (e.g. `tiny-skia` instead
of cairo/GL); the JavaScript engine (Escargot upstream) is deferred and abstracted behind an
interface for now.

## License

MIT — see [LICENSE](LICENSE).
