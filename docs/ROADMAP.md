# Roadmap

Target of the current epic: **render a static HTML page to PNG** (no JS).

Each milestone is delivered by a fixed agent pipeline and lands as one or more commits:

1. **Design** — produce a design note under `docs/design/Mx-*.md` (data model, API, edge cases).
2. **Analysis** — study the upstream Starfish C++ for reference + list risks/spec scope.
3. **Implementation** — write the crate code + unit tests.
4. **Review** — review the diff (correctness, simplicity, idiom).
5. **Verification** — `cargo build` / `cargo test` / golden output checks.

Then: commit + push.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **M0** | Workspace scaffold, repo, CI-less baseline | all | `cargo build` succeeds |
| **M1** | HTML tokenizer + tree builder → DOM | `dom`, `html` | Parses a representative HTML doc into a correct DOM tree (tested) |
| **M2** | CSS tokenizer + parser → stylesheet model | `css` | Parses selectors + declarations of a stylesheet (tested) |
| **M3** | Style resolution: cascade, specificity, computed values | `style` | Produces a styled tree with resolved properties (tested) |
| **M4** | Box generation + block/inline layout | `layout` | Computes a box tree with positions/sizes for a simple page (tested) |
| **M5** | Paint → `tiny-skia` → PNG | `paint`, `cli` | `starfish render in.html -o out.png` produces a correct raster (golden) |

## Scope boundaries (this epic)

- **In:** static HTML parsing, a useful CSS subset (display, box model, color, font basics,
  text), block + inline flow layout, text + box/background painting to PNG.
- **Out (deferred):** JavaScript (abstracted behind an interface), networking/fetch, incremental
  reflow, GPU rendering, advanced CSS (grid, flexbox, float, position), forms, events.

## Third-party choices (differ from upstream)

- Rendering: `tiny-skia` (raster) instead of cairo + GL.
- Text shaping/fonts: TBD per M5 design (likely `fontdue` or `cosmic-text`/`ttf-parser`).
- JS: deferred; trait-based stub instead of Escargot.
- HTML/CSS parsing: hand-rolled toward the WHATWG/CSS specs (utility crates allowed for plumbing).
