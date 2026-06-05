//! Text measurement abstraction (§4.1). M4 ships a crude default; M5 injects a
//! real font-backed implementation.

use starfish_style::FontWeight;

/// Ascent/descent for one line at a given font (used by M5 for baseline
/// placement; M4 only needs the trait to exist).
#[derive(Debug, Clone, Copy)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

/// Measures the advance width of a text string at a given font.
pub trait TextMeasurer {
    /// Advance width in px of `text` rendered at `font_size` px and `weight`.
    fn measure(&self, text: &str, font_size: f32, weight: FontWeight) -> f32;

    /// Ascent/descent for one line at this font. Default: 0.8/0.2 of font_size.
    fn line_metrics(&self, font_size: f32) -> LineMetrics {
        LineMetrics { ascent: font_size * 0.8, descent: font_size * 0.2 }
    }
}

/// Crude default: every char (incl. space) advances `0.5 * font_size`; bold is
/// unchanged. Makes word widths predictable for wrapping tests.
pub struct DefaultMeasurer;

impl TextMeasurer for DefaultMeasurer {
    fn measure(&self, text: &str, font_size: f32, _w: FontWeight) -> f32 {
        text.chars().count() as f32 * 0.5 * font_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_per_char_width() {
        let m = DefaultMeasurer;
        // 5 chars * 0.5 * 16 = 40.
        assert_eq!(m.measure("hello", 16.0, FontWeight(400)), 40.0);
        // space counts as a char.
        assert_eq!(m.measure(" ", 20.0, FontWeight(700)), 10.0);
        assert_eq!(m.measure("", 16.0, FontWeight(400)), 0.0);
    }

    #[test]
    fn default_line_metrics() {
        let m = DefaultMeasurer;
        let lm = m.line_metrics(10.0);
        assert_eq!(lm.ascent, 8.0);
        assert_eq!(lm.descent, 2.0);
    }
}
