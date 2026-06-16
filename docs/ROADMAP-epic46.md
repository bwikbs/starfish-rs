# Roadmap — Epic 46: fonts round 3 (features / variants / variable fonts)

Drives more of the rustybuzz/ttf-parser shaping stack (E10 complex-script
shaping, `crates/paint/src/font.rs` `shape()` calls `rustybuzz::shape(&face, &[],
buf)` — the empty slice is the OpenType feature list): `font-feature-settings` +
`font-kerning`, the `font-variant-*` longhands, and variable-font
`font-variation-settings`.

Same per-milestone pipeline. Additive: text not using these features shapes
byte-identically (the feature/variation lists are empty → same shaping; golden +
existing shaping tests unchanged). ComputedStyle is at a stack limit — any new
field must be small/boxed and verified with FULL `cargo test`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E46-M1** | **`font-feature-settings` + `font-kerning`**: parse `font-feature-settings: "liga" 1, "smcp" …` into `(tag, value)` pairs and `font-kerning: auto\|normal\|none`; thread them into `shape()` as `rustybuzz::Feature`s (kerning off → disable `kern`); the shape cache key includes them. | `style`, `paint` | `font-feature-settings:"liga" 0` disables ligatures (different glyph run than default); `font-kerning:none` changes spacing; default text byte-identical (tested) | ✅ |
| **E46-M2** | **`font-variant-*`**: `font-variant-caps` (small-caps), `font-variant-ligatures` (no-common-ligatures…), `font-variant-numeric` (tabular-nums, oldstyle-nums…), `font-variant` shorthand → mapped to the corresponding OpenType features fed to `shape()`. | `style`, `paint` | `font-variant-numeric: tabular-nums` enables `tnum`; `font-variant-ligatures: no-common-ligatures` disables `liga` (tested) | ✅ |
| **E46-M3** | **Variable fonts**: `font-variation-settings: "wght" 700, "wdth" …` set the variation axes on the `rustybuzz::Face` (via `set_variations`) before shaping; `font-weight`/`font-stretch` also map to `wght`/`wdth` axes when the face is variable. | `style`, `paint` | a variable font shaped at `font-variation-settings:"wght" 800` differs from the default instance (plumbing verified; no variable font vendored) (tested) | ✅ |

## Non-goals (deferred)

- Bundling a NEW variable test font if none is vendored — if no variable face is
  available, M3 wires the plumbing + tests the variation list reaches `shape()`
  and `set_variations` is called (axis application verified on whatever face
  supports it, else documented as plumbing-only).
- `font-optical-sizing`, `font-variant-east-asian`, `font-variant-alternates`,
  `@font-feature-values`, and `font-synthesis`.
- Per-glyph feature ranges; only whole-run features.
