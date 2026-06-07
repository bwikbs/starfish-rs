//! Text measurement abstraction (§4.1). M4 ships a crude default; M5 injects a
//! real font-backed implementation; E6-M1 threads a `FontQuery` so the measurer
//! resolves the same face the painter rasterizes.

use starfish_style::{FontStyle, FontWeight};

/// Ascent/descent for one line at a given font (used by M5 for baseline
/// placement; M4 only needs the trait to exist).
#[derive(Debug, Clone, Copy)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

/// Everything needed to resolve + measure with the right face (E6-M1 §2.1).
/// Borrows the family list from the owning `ComputedStyle` (no clone on the hot
/// path).
#[derive(Debug, Clone, Copy)]
pub struct FontQuery<'a> {
    /// the font-family list, in order; may be empty (→ UA default sans).
    pub family: &'a [String],
    pub style: FontStyle,
    pub weight: FontWeight,
    /// px.
    pub size: f32,
}

/// Measures the advance width of a text string at a given font.
pub trait TextMeasurer {
    /// Advance width in px of `text` for the queried face/size.
    fn measure(&self, text: &str, font: &FontQuery) -> f32;

    /// Ascent/descent for one line of the queried face. Default: 0.8/0.2 of size.
    fn line_metrics(&self, font: &FontQuery) -> LineMetrics {
        LineMetrics { ascent: font.size * 0.8, descent: font.size * 0.2 }
    }
}

/// Supplies intrinsic image sizes to layout (§3.2). paint's `ImageStore`
/// implements it; tests use a stub. Mirrors the `TextMeasurer` injection pattern.
pub trait ImageSource {
    /// Intrinsic `(width, height)` in px of the image at `src`, or `None` for a
    /// missing / undecodable image (→ broken-image sizing).
    fn intrinsic_size(&self, src: &str) -> Option<(f32, f32)>;
}

/// No-op source: every image is broken. Used by `layout_default` and tests that
/// don't exercise images.
pub struct NoImages;

impl ImageSource for NoImages {
    fn intrinsic_size(&self, _src: &str) -> Option<(f32, f32)> {
        None
    }
}

/// Crude default: every char (incl. space) advances `0.5 * font_size`; bold is
/// unchanged. Makes word widths predictable for wrapping tests.
pub struct DefaultMeasurer;

impl TextMeasurer for DefaultMeasurer {
    fn measure(&self, text: &str, font: &FontQuery) -> f32 {
        text.chars().count() as f32 * 0.5 * font.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(size: f32, weight: u16) -> FontQuery<'static> {
        FontQuery { family: &[], style: FontStyle::Normal, weight: FontWeight(weight), size }
    }

    #[test]
    fn default_per_char_width() {
        let m = DefaultMeasurer;
        // 5 chars * 0.5 * 16 = 40.
        assert_eq!(m.measure("hello", &q(16.0, 400)), 40.0);
        // space counts as a char.
        assert_eq!(m.measure(" ", &q(20.0, 700)), 10.0);
        assert_eq!(m.measure("", &q(16.0, 400)), 0.0);
    }

    #[test]
    fn default_line_metrics() {
        let m = DefaultMeasurer;
        let lm = m.line_metrics(&q(10.0, 400));
        assert_eq!(lm.ascent, 8.0);
        assert_eq!(lm.descent, 2.0);
    }
}
