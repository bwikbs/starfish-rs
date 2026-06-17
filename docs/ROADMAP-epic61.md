# Roadmap — Epic 61: media & embedded content round 2

Extends media (E15-M3 `<video>`/`<audio>` poster + play triangle, `BoxKind::Media`)
and adds embedded-content placeholders: media controls chrome, `<iframe>`/
`<embed>`/`<object>` boxes, and `<video>` intrinsic aspect-ratio.

Same per-milestone pipeline. Additive: pages without these render
byte-identically (golden + existing media tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E61-M1** | **media controls chrome**: a `<video controls>`/`<audio controls>` paints a control bar along the bottom — a play triangle + a timeline track — over the poster/box. No `controls` attr → unchanged (poster + center play glyph only). | `layout`, `paint` | a `<video controls poster=…>` paints a bottom control bar (play + timeline); without controls it is unchanged (tested + visual) | ✅ |
| **E61-M2** | **`<iframe>`/`<embed>`/`<object>`**: render as a replaced placeholder box sized by `width`/`height` attrs (default 300×150) / CSS, with a border + the `src`/`data` label centered (can't load cross-document content). | `layout`, `paint` | an `<iframe src=… width=200 height=100>` paints a 200×100 bordered placeholder showing its src; `<embed>`/`<object>` similar (tested + visual) | ☐ |
| **E61-M3** | **`<video>` intrinsic aspect-ratio + poster fit**: a `<video width=320 height=180>` (or `aspect-ratio`) sizes the box to that ratio when one axis is auto; the poster is `object-fit: contain`-fitted into the box; `<track>` parses (ignored). | `layout`, `paint` | a `<video width=320 height=180>` with only width set derives height from the 16:9 ratio; the poster fits without distortion (tested + visual) | ☐ |

## Non-goals (deferred)

- Actual video/audio playback or frame decoding (poster/placeholder only); the
  control bar is static chrome (no interaction).
- `<iframe>` loading/rendering the referenced document (cross-document render is
  out of scope), `srcdoc` rendering, and sandbox semantics.
- `<object>` with a nested fallback subtree rendering its children, and
  `<embed>` type-based plugin dispatch.
