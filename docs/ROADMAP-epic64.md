# Roadmap — Epic 64: `dir` attribute & RTL round 2

The CSS `direction`/`unicode-bidi` properties parse and drive inline bidi
(E6/E10), but the **HTML `dir` attribute is not wired to them** — `<div dir=rtl>`
renders LTR unless you also write `style="direction:rtl"`. This epic maps the
`dir` attribute to direction (the spec's UA presentational hint), adds `<bdo>`/
`<bdi>`, models `unicode-bidi: isolate`, and the `dir=auto` first-strong-char
heuristic.

Same per-milestone pipeline. Additive: pages with no `dir` attribute / `<bdo>` /
`<bdi>` stay byte-identical (golden + existing bidi tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E64-M1** | **`dir` attribute → direction**: UA `[dir=ltr]{direction:ltr}` / `[dir=rtl]{direction:rtl}`, and `bdo[dir=ltr]`/`bdo[dir=rtl]` additionally set `unicode-bidi:bidi-override`. `<div dir=rtl>` now right-aligns + RTL-reorders its inline content with no inline style. | `style` (ua) | `<p dir=rtl>` right-aligns Latin text; `<bdo dir=rtl>one two</bdo>` paints the words reversed (tested + visual) | ☐ |
| **E64-M2** | **`unicode-bidi: isolate` + `<bdi>`**: UA `bdi{unicode-bidi:isolate}`; model isolate in `reorder_line` so an isolated inline's text is treated as a single neutral unit in the parent paragraph (its internal bidi is independent and does not reorder across the isolate boundary). | `style` (ua), `layout` | an LTR sentence with a `<bdi>`-wrapped RTL phrase keeps the surrounding word order stable while the phrase reorders internally (tested) | ☐ |
| **E64-M3** | **`dir=auto` heuristic**: `dir=auto` (and `<bdi>`'s default) computes direction from the first strong directional char in the element's text (L → ltr; R/AL → rtl; none → ltr). Resolved at cascade time from the element's text content. | `style` | `<p dir=auto>` with a leading Arabic/Hebrew char computes RTL; leading Latin computes LTR (tested) | ☐ |

## Non-goals (deferred)

- Full per-run isolate nesting / `isolate-override` distinct from `bidi-override`
  (MVP folds `isolate-override` into override; isolate is a neutral-unit subset).
- `dir=auto` re-evaluation on DOM text mutation (computed once at cascade).
- Mirrored bracket glyphs and the full UBA explicit-formatting-code handling
  beyond what the `unicode-bidi` crate already does at line granularity.
- Per-character RTL shaping for scripts with no vendored font (Arabic/Hebrew
  glyphs may render as tofu; positions/reordering are still correct + tested).
