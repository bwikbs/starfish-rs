# Roadmap — Epic 6: text & typography quality

Upgrades text rendering from "one vendored DejaVu Sans face" to real font selection,
web fonts, and bidirectional/spaced text. Same per-milestone agent pipeline (design →
analysis → implementation → review → verification), each landing as its own commit + push.

Today: a single embedded DejaVu Sans (regular + bold by weight≥600), fontdue glyph
raster, approximate metrics, no italic, no `font-family` resolution, no `@font-face`, no
bidi.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **E6-M1** | Font matching & faces: a font database (discover system fonts via `fontdb`, with the vendored DejaVu as fallback); `font-family` resolution (family lists + the generic families serif / sans-serif / monospace / cursive→fallback); `font-style` normal/italic/oblique; `font-weight` matching to the nearest available face; real per-face fontdue metrics + advances replacing the approximate measurer. | `paint`, `style` | A page using `font-family: serif`, a specific family, `font-style: italic`, and various weights renders with the right faces/metrics (tested + visual) |
| **E6-M2** | `@font-face`: parse `@font-face` rules (family, src url/local, weight/style descriptors), load the font file via the `ResourceLoader` (file/http/data), register it into the font DB, and use it when `font-family` matches. | `paint`, `style`, `net` | A page declaring an `@font-face` and using its family renders with the loaded font (tested + visual) |
| **E6-M3** | Bidirectional + spaced text: the Unicode bidi algorithm (`direction`/`unicode-bidi`, RTL reordering of runs within a line), plus `letter-spacing`, `word-spacing`, `text-transform` (uppercase/lowercase/capitalize), and `white-space` (pre / nowrap / pre-wrap). | `style`, `layout`, `paint` | RTL text (Arabic/Hebrew) reorders correctly on a line; letter/word-spacing and text-transform and white-space variants apply (tested + visual) |

## Non-goals (deferred)

- Complex-script shaping (HarfBuzz-level: Arabic joining/ligatures, Indic reordering,
  contextual forms) — advances-sum + bidi reordering only; mark visible glyph-shaping
  limits.
- Vertical writing modes, `text-orientation`, ruby.
- `font-variant`/OpenType features, kerning (advances-sum, no pair kerning), `font-stretch`,
  variable-font axes, color/emoji fonts (COLR/CBDT), subpixel/hinting tuning.
- `text-overflow`/ellipsis, hyphenation, `line-break`/`word-break` precision beyond
  whitespace + pre/nowrap.
