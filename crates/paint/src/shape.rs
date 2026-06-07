//! Text shaping output (E10-M1). `FontDb::shape` runs rustybuzz over the face's
//! GSUB/GPOS tables (Latin kerning + standard ligatures) and yields a flat list
//! of `ShapedGlyph`s in visual (left-to-right) order. Both the measurer
//! (`advance_width` = Σ x_advance) and the rasterizer (`draw_glyph_run`) consume
//! the SAME shaped run, so measure == paint holds at the glyph level.

use fontdb::ID;

/// One shaped glyph: a font glyph index plus its placement, in device pixels.
/// Advances/offsets are already scaled from font design units (upem) to px and
/// include any per-cluster letter/word-spacing folded into `x_advance`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// The face this glyph's index belongs to. A glyph index is only meaningful
    /// against the face it was shaped with, so font fallback (E10-M3) — which can
    /// mix glyphs from several faces in one run — must carry it per glyph so the
    /// rasterizer reads the right face.
    pub face: ID,
    pub glyph_id: u16,
    /// px advance to the next pen position; includes per-cluster spacing.
    pub x_advance: f32,
    /// px horizontal offset of this glyph from the pen.
    pub x_offset: f32,
    /// px vertical offset of this glyph from the baseline, +up.
    pub y_offset: f32,
    /// Byte offset of this glyph's cluster into the source `text`.
    pub cluster: usize,
}
