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
    /// `letter-spacing` in px, added after every char (E6-M3 §4).
    pub letter_spacing: f32,
    /// `word-spacing` in px, added at each U+0020 space (E6-M3 §4).
    pub word_spacing: f32,
}

/// Extra advance from letter/word-spacing for `text` under `q`: `letter_spacing`
/// after every char + `word_spacing` at each space. Shared by the default
/// measurer and the font-backed measurer/rasterizer so measure == paint (§4.2).
pub fn extra_spacing(text: &str, q: &FontQuery) -> f32 {
    if q.letter_spacing == 0.0 && q.word_spacing == 0.0 {
        return 0.0;
    }
    let mut extra = 0.0;
    for c in text.chars() {
        extra += q.letter_spacing;
        if c == ' ' {
            extra += q.word_spacing;
        }
    }
    extra
}

/// Measures the advance width of a text string at a given font.
pub trait TextMeasurer {
    /// Advance width in px of `text` for the queried face/size.
    fn measure(&self, text: &str, font: &FontQuery) -> f32;

    /// Ascent/descent for one line of the queried face. Default: 0.8/0.2 of size.
    fn line_metrics(&self, font: &FontQuery) -> LineMetrics {
        LineMetrics {
            ascent: font.size * 0.8,
            descent: font.size * 0.2,
        }
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
        text.chars().count() as f32 * 0.5 * font.size + extra_spacing(text, font)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(size: f32, weight: u16) -> FontQuery<'static> {
        FontQuery {
            family: &[],
            style: FontStyle::Normal,
            weight: FontWeight(weight),
            size,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
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
    fn default_letter_spacing_widens_per_char() {
        let m = DefaultMeasurer;
        let mut spaced = q(16.0, 400);
        spaced.letter_spacing = 5.0;
        // "abc": base 3*0.5*16=24, + 3*5 = 39.
        assert_eq!(m.measure("abc", &spaced), 24.0 + 15.0);
    }

    #[test]
    fn default_word_spacing_widens_at_spaces() {
        let m = DefaultMeasurer;
        let mut spaced = q(16.0, 400);
        spaced.word_spacing = 10.0;
        // "a b": base 3*0.5*16=24, + 1 space * 10 = 34.
        assert_eq!(m.measure("a b", &spaced), 24.0 + 10.0);
    }

    #[test]
    fn default_line_metrics() {
        let m = DefaultMeasurer;
        let lm = m.line_metrics(&q(10.0, 400));
        assert_eq!(lm.ascent, 8.0);
        assert_eq!(lm.descent, 2.0);
    }
}
