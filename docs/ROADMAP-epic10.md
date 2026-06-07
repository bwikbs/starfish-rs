# Roadmap — Epic 10: complex-script text shaping

Replaces the per-`char` advance-sum with real text shaping via **rustybuzz** (a
pure-Rust HarfBuzz port). The current pipeline walks `text.chars()` in both the
measurer (`FontDb::advance_width`) and the rasterizer (`draw_glyph_run`), summing
per-codepoint advances — no kerning, no ligatures, no Arabic joining, no mark
positioning. Epic 10 introduces a shaping layer that turns a text run + resolved
face into a sequence of positioned **glyphs** (glyph-id, x-advance, x/y-offset,
cluster), and routes both measure and paint through it so the `measure == paint`
invariant holds at the glyph level.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push.

Key constraint: rustybuzz shapes against a `ttf_parser::Face`; the glyph ids it
returns index the same `glyf`/`CFF` order fontdue rasterizes by
(`rasterize_indexed`). Both derive from the same font bytes, so ids match — that
is what lets shaping (rustybuzz) and rasterization (fontdue) stay consistent.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E10-M1** | Shaping infrastructure: add `rustybuzz` to `paint`; `FontDb::shape(text, q) -> Vec<ShapedGlyph{ glyph_id, x_advance, x_offset, y_offset, cluster }>` (cache the parsed `rustybuzz::Face` alongside the fontdue face, keyed by id). Rewrite `advance_width` to sum shaped x-advances (+ `extra_spacing` at cluster boundaries) and `draw_glyph_run` to walk shaped glyphs, rasterizing by **glyph index** (`rasterize_indexed`) with x/y offsets applied. Latin text now gets real kerning + standard ligatures. | `paint`, `layout` | Latin text renders via shaping (kerning/ligatures); `measure == paint` holds glyph-for-glyph; existing render tests still pass (tested + visual) | ☐ |
| **E10-M2** | Arabic / RTL shaping: shape runs with their script + direction (rustybuzz `set_direction`/`set_script`), so Arabic gets joining forms (isolated/initial/medial/final) and ligatures; integrate with the existing `unicode-bidi` line reordering so shaped RTL runs are placed right-to-left at the correct pen positions. Vendor an Arabic-capable face with GSUB (e.g. Noto Naskh Arabic / Amiri subset) + license. | `paint`, `layout` | An Arabic string renders with correct joining + RTL placement; mixed LTR/RTL line lays out correctly (tested + visual) | ☐ |
| **E10-M3** | Marks + fallback: honor x/y glyph offsets for combining marks (mark-to-base / mark-to-mark), and add per-cluster **font fallback** — when the resolved face has no glyph for a cluster (shaping yields `.notdef`), reshape that cluster with the next family / a fallback face. Optionally Devanagari (Indic reordering is rustybuzz-native). | `paint`, `layout` | Combining diacritics position correctly; a cluster the primary face lacks falls back to a face that has it (tested + visual) | ☐ |

## Non-goals (deferred)

- Vertical text / `writing-mode` (horizontal only).
- OpenType feature control from CSS (`font-feature-settings`, `font-variant-*`);
  M1 uses rustybuzz's default feature set per script.
- `font-kerning`/`letter-spacing` interaction subtleties beyond applying
  `extra_spacing` at cluster boundaries (spacing inside a ligature is not split).
- Full Unicode script/run itemization (BMP + common scripts; itemize by
  bidi level + coarse script run, not the full UAX-24 algorithm).
- Color/emoji glyph tables (COLR/CBDT/sbix), variable-font axes, hinting.
- Shaping caches beyond the parsed-face cache (no per-run shape memoization yet —
  that overlaps Epic 11 performance work).
