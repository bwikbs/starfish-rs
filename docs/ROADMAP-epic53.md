# Roadmap — Epic 53: lists & generated content round 2 (final epic)

The 20th epic of the autonomous grind. Rounds out list markers and the `content`
property: `list-style-image`, `content: url()` (replaced generated content), and
`quotes` + `open-quote`/`close-quote`.

Same per-milestone pipeline. Additive: pages not using these render
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E53-M1** | **`list-style-image`**: `list-style-image: url(...)` uses the image as the list marker (decoded like a background/`<img>`); falls back to the `list-style-type` marker when the image is absent/none. `list-style` shorthand accepts the image. | `style`, `layout`, `paint` | a `<ul style="list-style-image:url(dot.png)">` paints the image as each item marker (tested + visual) | ✅ |
| **E53-M2** | **`content: url()` + `content: none`**: `content` on `::before`/`::after` may be a `url(...)` → an image replaced box; `content: none`/`normal` suppresses the pseudo (already partly). A `content` list mixing strings/`url()`/`counter()`/`attr()` renders in order. | `style`, `layout`, `paint` | `::before{content:url(icon.png)}` paints the image before the element; mixed `content:"[" attr renders in order (tested + visual) | ✅ |
| **E53-M3** | **`quotes` + `open-quote`/`close-quote`**: `quotes: "«" "»" …` defines quote pairs (nesting level); `content: open-quote`/`close-quote`/`no-open-quote`/`no-close-quote` in generated content emits the right mark for the nesting depth; `<q>` UA `::before`/`::after` use them. | `css`, `style`, `layout` | `q{quotes:'«' '»'} q::before{content:open-quote}` puts « before; nested quotes use the next level (tested + visual) | ☐ |

## Non-goals (deferred)

- `list-style-image` sizing/position niceties beyond a default-sized marker; SVG
  list images via `<svg>` (raster/url only).
- `content` with `<image>` gradients / `image-set()` as generated content,
  `content` alt text (`content: url() / "alt"`) beyond parsing, and `content` on
  regular (non-pseudo) elements replacing children.
- `quotes: auto` language-aware quote marks, and `<q>` automatic quoting without
  an explicit `content: open-quote` (MVP wires the UA `q` rule).
