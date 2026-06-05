//! Font loading + metrics + rasterization (M5 §2). The two embedded DejaVu
//! faces back both layout's `TextMeasurer` (so measuring matches painting) and
//! the glyph blitting in `raster.rs`.

use fontdue::Font as FontdueFont;
use starfish_layout::{LineMetrics, TextMeasurer};
use starfish_style::FontWeight;

const REGULAR_TTF: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const BOLD_TTF: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");

/// One rasterized glyph: an 8-bit coverage mask + its placement relative to the
/// pen origin (baseline), in device pixels.
pub struct GlyphBitmap {
    pub width: usize,
    pub height: usize,
    /// `metrics.xmin` — left side bearing (+x = right).
    pub left: i32,
    /// Distance from the baseline up to the mask's top (`ymin + height`).
    pub top: i32,
    pub advance: f32,
    /// `width * height` coverage values, 0..=255.
    pub coverage: Vec<u8>,
}

/// The two-face font database; built once per render.
pub struct FontDb {
    regular: FontdueFont,
    bold: FontdueFont,
}

impl FontDb {
    /// Load the embedded DejaVu faces. Returns `Err` only on a corrupt asset
    /// (a build-time bug) rather than panicking.
    pub fn load() -> Result<FontDb, String> {
        let settings = fontdue::FontSettings::default();
        let regular = FontdueFont::from_bytes(REGULAR_TTF, settings)
            .map_err(|e| format!("loading regular face: {e}"))?;
        let bold = FontdueFont::from_bytes(BOLD_TTF, settings)
            .map_err(|e| format!("loading bold face: {e}"))?;
        Ok(FontDb { regular, bold })
    }

    /// weight >= 600 → bold face, else regular.
    fn face(&self, weight: FontWeight) -> &FontdueFont {
        if weight.0 >= 600 {
            &self.bold
        } else {
            &self.regular
        }
    }

    /// Sum of per-glyph advance widths (no kerning). Missing glyphs fall back to
    /// fontdue's `.notdef` advance.
    pub fn advance_width(&self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        let f = self.face(weight);
        text.chars().map(|c| f.metrics(c, font_size).advance_width).sum()
    }

    /// Ascent/descent (both positive, pointing away from the baseline) for a
    /// line at `font_size`, from fontdue's horizontal line metrics.
    pub fn line_metrics(&self, font_size: f32, weight: FontWeight) -> LineMetrics {
        match self.face(weight).horizontal_line_metrics(font_size) {
            Some(lm) => LineMetrics {
                ascent: lm.ascent,
                descent: -lm.descent, // fontdue descent is negative
            },
            None => LineMetrics {
                ascent: font_size * 0.8,
                descent: font_size * 0.2,
            },
        }
    }

    /// Rasterize one char to a coverage mask + placement. Whitespace yields an
    /// empty mask (width == 0) but a real advance.
    pub fn rasterize_glyph(&self, ch: char, font_size: f32, weight: FontWeight) -> GlyphBitmap {
        let (m, coverage) = self.face(weight).rasterize(ch, font_size);
        GlyphBitmap {
            width: m.width,
            height: m.height,
            left: m.xmin,
            // fontdue ymin = offset of the mask bottom from the baseline (up +);
            // the mask top sits ymin + height above the baseline.
            top: m.ymin + m.height as i32,
            advance: m.advance_width,
            coverage,
        }
    }
}

/// Adapts a `FontDb` (by ref) to layout's `TextMeasurer`, so line-breaking
/// during layout uses the same advances the painter draws.
pub struct FontMeasurer<'a>(pub &'a FontDb);

impl TextMeasurer for FontMeasurer<'_> {
    fn measure(&self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        self.0.advance_width(text, font_size, weight)
    }
    fn line_metrics(&self, font_size: f32) -> LineMetrics {
        // weight-independent line box; use the regular face.
        self.0.line_metrics(font_size, FontWeight(400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> FontDb {
        FontDb::load().expect("load embedded faces")
    }

    #[test]
    fn advance_monotonic_and_empty_zero() {
        let f = db();
        assert_eq!(f.advance_width("", 16.0, FontWeight(400)), 0.0);
        let short = f.advance_width("Hi", 16.0, FontWeight(400));
        let long = f.advance_width("Hello world", 16.0, FontWeight(400));
        assert!(long > short);
        assert!(short > 0.0);
    }

    #[test]
    fn advance_scales_with_size() {
        let f = db();
        let small = f.advance_width("Mmm", 10.0, FontWeight(400));
        let big = f.advance_width("Mmm", 20.0, FontWeight(400));
        assert!(big > small);
    }

    #[test]
    fn bold_differs_from_regular() {
        let f = db();
        let reg = f.advance_width("Mmm", 16.0, FontWeight(400));
        let bold = f.advance_width("Mmm", 16.0, FontWeight(700));
        // Different faces → different advances (and a bold "M" has advance > 0).
        assert!(bold > 0.0);
        assert_ne!(reg, bold);
    }

    #[test]
    fn face_selection_boundary() {
        let f = db();
        // weight 600 → bold, 599 → regular: their advances differ for "g".
        let at_600 = f.advance_width("g", 16.0, FontWeight(600));
        let at_599 = f.advance_width("g", 16.0, FontWeight(599));
        let bold = f.advance_width("g", 16.0, FontWeight(700));
        let reg = f.advance_width("g", 16.0, FontWeight(400));
        assert_eq!(at_600, bold);
        assert_eq!(at_599, reg);
    }

    #[test]
    fn line_metrics_positive_and_sane() {
        let f = db();
        let lm = f.line_metrics(20.0, FontWeight(400));
        assert!(lm.ascent > 0.0);
        assert!(lm.descent > 0.0);
        let total = lm.ascent + lm.descent;
        assert!(total > 0.9 * 20.0 && total < 1.5 * 20.0, "total={total}");
    }

    #[test]
    fn rasterize_glyph_has_coverage() {
        let f = db();
        let g = f.rasterize_glyph('x', 24.0, FontWeight(400));
        assert!(g.width > 0 && g.height > 0);
        assert!(g.coverage.iter().any(|&c| c > 0));
        assert!(g.advance > 0.0);
    }

    #[test]
    fn rasterize_space_is_empty_but_advances() {
        let f = db();
        let g = f.rasterize_glyph(' ', 24.0, FontWeight(400));
        assert_eq!(g.width, 0);
        assert!(g.advance > 0.0);
    }
}
