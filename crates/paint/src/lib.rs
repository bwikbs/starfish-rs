//! starfish-paint (M5) — rasterize a laid-out page to an RGBA pixmap / PNG.
//!
//! Pipeline: HTML → DOM → inline `<style>` CSS → styled tree → layout (driven
//! by the embedded DejaVu metrics) → display list → tiny-skia raster. See
//! `docs/design/M5-paint.md`.

mod display;
mod font;
mod raster;

use starfish_dom::{Document, NodeKind};
use starfish_layout::layout;
use starfish_style::style_tree;

pub use display::PaintCmd;
pub use font::{FontDb, FontMeasurer, GlyphBitmap};
pub use starfish_layout::{LayoutBox, Rect};
pub use starfish_style::StyledTree;
pub use tiny_skia::Pixmap;

use display::build_display_list;
use raster::rasterize;

/// Upper bound on rasterized pixmap dimensions. Caps allocation from extreme,
/// huge, or non-finite (NaN/∞) inputs reaching the f32→u32 conversion below.
/// 20000² × 4 bytes ≈ 1.6 GiB worst case — large but bounded and non-panicking.
const MAX_DIMENSION: u32 = 20_000;

/// Convert a possibly non-finite / out-of-range layout dimension (in px) to a
/// pixmap dimension clamped to `[1, MAX_DIMENSION]`. NaN and negatives map to 1.
fn clamp_dimension(px: f32) -> u32 {
    if !px.is_finite() || px < 1.0 {
        return 1;
    }
    (px.round() as i64).clamp(1, MAX_DIMENSION as i64) as u32
}

/// Concatenate the text content of every `<style>` element, in document order.
/// (M1 stores `<style>` content as ordinary `Text` children — M5 §5.1.)
fn extract_css(doc: &Document) -> String {
    let mut css = String::new();
    let mut stack = vec![doc.root()];
    // DFS; children are pushed reversed so we visit them in document order.
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("style") {
            for c in doc.children(id) {
                if let NodeKind::Text(t) = doc.kind(c) {
                    css.push_str(t);
                    css.push('\n');
                }
            }
        }
        let children = doc.children(id);
        for c in children.into_iter().rev() {
            stack.push(c);
        }
    }
    css
}

/// End-to-end: HTML string → rendered RGBA pixmap. The page height grows with
/// content; the pixmap is `round(viewport_width) × round(root margin-box
/// height)` (each at least 1).
pub fn render_html(html: &str, viewport_width: f32) -> Pixmap {
    let doc = starfish_html::parse(html);
    let css = extract_css(&doc);
    let sheet = starfish_css::parse_stylesheet(&css);
    let styled = style_tree(&doc, &[sheet]);
    let fonts = FontDb::load().expect("embedded faces load");

    let root = layout(&doc, &styled, viewport_width, &FontMeasurer(&fonts));

    let width = clamp_dimension(viewport_width);
    let height = clamp_dimension(root.dimensions().margin_box().height);

    paint(&root, &styled, &fonts, width, height)
}

/// Paint an already-laid-out box tree to a pixmap of the given device size.
pub fn paint(
    layout_root: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    width: u32,
    height: u32,
) -> Pixmap {
    let cmds = build_display_list(layout_root, styled, fonts);
    rasterize(&cmds, width, height, fonts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let p = pm.pixel(x, y).expect("in bounds");
        // tiny-skia premultiplied; for opaque pixels these equal straight bytes.
        (p.red(), p.green(), p.blue(), p.alpha())
    }

    #[test]
    fn extract_css_collects_style_text() {
        let doc = starfish_html::parse(
            "<html><head><style>p{color:red}</style></head><body>x</body></html>",
        );
        let css = extract_css(&doc);
        assert!(css.contains("p{color:red}"), "got: {css:?}");
    }

    #[test]
    fn dimensions_match_viewport_and_height() {
        let html = "<html><body><div style='x'>hi</div></body></html>";
        let pm = render_html(html, 800.0);
        assert_eq!(pm.width(), 800);
        assert!(pm.height() >= 1);
    }

    #[test]
    fn empty_html_does_not_panic() {
        let pm = render_html("", 200.0);
        assert_eq!(pm.width(), 200);
        assert!(pm.height() >= 1);
    }

    #[test]
    fn infinite_width_does_not_panic_and_clamps() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm = render_html(html, f32::INFINITY);
        assert!(pm.width() >= 1 && pm.width() <= MAX_DIMENSION);
        assert!(pm.height() >= 1 && pm.height() <= MAX_DIMENSION);
    }

    #[test]
    fn zero_and_negative_width_clamp_to_at_least_one() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm0 = render_html(html, 0.0);
        assert!(pm0.width() >= 1);
        let pm_neg = render_html(html, -42.0);
        assert!(pm_neg.width() >= 1);
        let pm_nan = render_html(html, f32::NAN);
        assert!(pm_nan.width() >= 1);
    }

    #[test]
    fn huge_finite_width_clamps_to_max() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm = render_html(html, 1e9);
        assert_eq!(pm.width(), MAX_DIMENSION);
    }

    #[test]
    fn solid_block_pixel_is_background_color() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div></div></body></html>";
        let pm = render_html(html, 200.0);
        assert_eq!(pm.width(), 200);
        // inside the block → red.
        assert_eq!(px(&pm, 10, 10), (255, 0, 0, 255));
        // outside the block (to the right) → white.
        assert_eq!(px(&pm, 150, 10), (255, 255, 255, 255));
    }

    #[test]
    fn border_pixel_is_border_color() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;border:5px solid #0000ff}\
            </style></head><body><div></div></body></html>";
        let pm = render_html(html, 200.0);
        // top border band (y in 0..5) → blue.
        assert_eq!(px(&pm, 50, 2), (0, 0, 255, 255));
        // interior (no background) → white.
        assert_eq!(px(&pm, 50, 25), (255, 255, 255, 255));
    }

    #[test]
    fn text_region_has_non_white_pixels() {
        let html = "<html><head><style>\
            body{margin:0} p{margin:0;color:#000000;font-size:20px}\
            </style></head><body><p>Hello</p></body></html>";
        let pm = render_html(html, 200.0);
        // scan the first line region for any non-white pixel.
        let mut found = false;
        'scan: for y in 0..pm.height().min(30) {
            for x in 0..pm.width().min(120) {
                let (r, g, b, _) = px(&pm, x, y);
                if (r, g, b) != (255, 255, 255) {
                    found = true;
                    break 'scan;
                }
            }
        }
        assert!(found, "expected glyph pixels in the text region");
        // a region well below the single line of text stays white.
        let h = pm.height();
        if h > 60 {
            assert_eq!(px(&pm, 150, h - 5), (255, 255, 255, 255));
        }
    }
}
