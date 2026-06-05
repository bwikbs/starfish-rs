//! starfish-paint (M5) — rasterize a laid-out page to an RGBA pixmap / PNG.
//!
//! Pipeline: HTML → DOM → inline `<style>` CSS → styled tree → layout (driven
//! by the embedded DejaVu metrics) → display list → tiny-skia raster. See
//! `docs/design/M5-paint.md`.

mod display;
mod font;
mod image_store;
mod raster;

use std::path::{Path, PathBuf};

use starfish_dom::{Document, NodeKind};
use starfish_layout::layout;
use starfish_style::style_tree;

pub use display::PaintCmd;
pub use font::{FontDb, FontMeasurer, GlyphBitmap};
pub use image_store::{DecodedImage, ImageStore};
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

/// Pre-pass: decode every `<img src>` in the document into `images` so layout
/// and paint can read intrinsic sizes / pixels immutably (E2-M4 §3.2).
fn decode_images(doc: &Document, images: &mut ImageStore) {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("img") {
            if let Some(src) = doc.get_attribute(id, "src") {
                images.get(src);
            }
        }
        for c in doc.children(id) {
            stack.push(c);
        }
    }
}

/// End-to-end: HTML string → rendered RGBA pixmap. `base_dir` is the directory
/// that relative `<img src>` paths resolve against (the input file's parent).
/// The page height grows with content; the pixmap is `round(viewport_width) ×
/// round(root margin-box height)` (each at least 1).
pub fn render_html(html: &str, viewport_width: f32, base_dir: &Path) -> Pixmap {
    let doc = starfish_html::parse(html);
    let css = extract_css(&doc);
    let sheet = starfish_css::parse_stylesheet(&css);
    let styled = style_tree(&doc, &[sheet]);
    let fonts = FontDb::load().expect("embedded faces load");

    let mut images = ImageStore::new(base_dir);
    decode_images(&doc, &mut images);

    let root = layout(&doc, &styled, viewport_width, &FontMeasurer(&fonts), &images);

    let width = clamp_dimension(viewport_width);
    let height = clamp_dimension(root.dimensions().margin_box().height);

    paint(&root, &styled, &fonts, &images, width, height)
}

/// `render_html` resolving relative images against the current directory. Keeps
/// image-free callers/tests compiling with a single argument fewer.
pub fn render_html_cwd(html: &str, viewport_width: f32) -> Pixmap {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    render_html(html, viewport_width, &base)
}

/// Paint an already-laid-out box tree to a pixmap of the given device size.
pub fn paint(
    layout_root: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    width: u32,
    height: u32,
) -> Pixmap {
    let cmds = build_display_list(layout_root, styled, fonts, images);
    rasterize(&cmds, width, height, fonts, images)
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
        let pm = render_html_cwd(html, 800.0);
        assert_eq!(pm.width(), 800);
        assert!(pm.height() >= 1);
    }

    #[test]
    fn empty_html_does_not_panic() {
        let pm = render_html_cwd("", 200.0);
        assert_eq!(pm.width(), 200);
        assert!(pm.height() >= 1);
    }

    #[test]
    fn infinite_width_does_not_panic_and_clamps() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm = render_html_cwd(html, f32::INFINITY);
        assert!(pm.width() >= 1 && pm.width() <= MAX_DIMENSION);
        assert!(pm.height() >= 1 && pm.height() <= MAX_DIMENSION);
    }

    #[test]
    fn zero_and_negative_width_clamp_to_at_least_one() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm0 = render_html_cwd(html, 0.0);
        assert!(pm0.width() >= 1);
        let pm_neg = render_html_cwd(html, -42.0);
        assert!(pm_neg.width() >= 1);
        let pm_nan = render_html_cwd(html, f32::NAN);
        assert!(pm_nan.width() >= 1);
    }

    #[test]
    fn huge_finite_width_clamps_to_max() {
        let html = "<html><body><p>hi</p></body></html>";
        let pm = render_html_cwd(html, 1e9);
        assert_eq!(pm.width(), MAX_DIMENSION);
    }

    #[test]
    fn solid_block_pixel_is_background_color() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
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
        let pm = render_html_cwd(html, 200.0);
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
        let pm = render_html_cwd(html, 200.0);
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

    // --- E2-M4: <img> end-to-end pixels ---

    /// Write a 2×2 PNG (TL red, TR green, BL blue, BR white) into a fresh temp
    /// dir and return that dir.
    fn fixture_dir_with_png() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-m4-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
        img.save(dir.join("px.png")).unwrap();
        dir
    }

    #[test]
    fn img_upscaled_pixels_match_source() {
        let dir = fixture_dir_with_png();
        // 2×2 image upscaled to 4×4 at the origin; nearest-neighbour quadrants.
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='px.png' width='4' height='4'></body></html>";
        let pm = render_html(html, 50.0, &dir);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255)); // TL red
        assert_eq!(px(&pm, 3, 0), (0, 255, 0, 255)); // TR green
        assert_eq!(px(&pm, 0, 3), (0, 0, 255, 255)); // BL blue
        assert_eq!(px(&pm, 3, 3), (255, 255, 255, 255)); // BR white
    }

    #[test]
    fn broken_img_render_does_not_panic() {
        let dir = fixture_dir_with_png();
        let html = "<html><head><style>body{margin:0}</style></head>\
            <body><img src='nope.png' width='10' height='10'></body></html>";
        let pm = render_html(html, 50.0, &dir);
        // no blit; interior of the placeholder box stays white.
        assert_eq!(px(&pm, 5, 5), (255, 255, 255, 255));
        // placeholder grey border pixel on the top edge.
        assert_eq!(px(&pm, 5, 0), (0x80, 0x80, 0x80, 255));
    }

    #[test]
    fn img_huge_attr_render_does_not_hang() {
        let dir = fixture_dir_with_png();
        // A decodable image scaled to a colossal width/height: the pixmap is
        // clamped to MAX_DIMENSION, and the blit must not iterate the unclamped
        // 1e8×1e8 dest (which would spin for >30s).
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='px.png' width='100000000' height='100000000'></body></html>";
        let start = std::time::Instant::now();
        let pm = render_html(html, 50.0, &dir);
        assert!(
            start.elapsed().as_secs() < 5,
            "huge-attr img render hung ({:?})",
            start.elapsed()
        );
        // Height (driven by the colossal img) is clamped to the max dimension;
        // the visible top-left still samples red.
        assert_eq!(pm.width(), 50);
        assert_eq!(pm.height(), MAX_DIMENSION);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255));
    }

    #[test]
    fn img_inf_attr_falls_back_to_intrinsic() {
        let dir = fixture_dir_with_png();
        // width=inf must be rejected by attr_px (non-finite) → falls back to the
        // 2×2 intrinsic size rather than producing an infinite used size.
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='px.png' width='inf'></body></html>";
        let pm = render_html(html, 50.0, &dir);
        assert!(pm.height() < MAX_DIMENSION);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255));
    }

    // --- E2-M5: gradient / border-radius / box-shadow / opacity pixels ---

    #[test]
    fn gradient_to_bottom_varies_top_to_bottom() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:100px;\
            background:linear-gradient(to bottom, #ff0000, #0000ff)}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 100.0);
        let (rt, _, bt, _) = px(&pm, 50, 2);
        let (rb, _, bb, _) = px(&pm, 50, 97);
        // top is red-dominant, bottom is blue-dominant.
        assert!(rt > bt, "top red>blue: {:?}", px(&pm, 50, 2));
        assert!(bb > rb, "bottom blue>red: {:?}", px(&pm, 50, 97));
        // red decreases and blue increases top→bottom.
        assert!(rb < rt, "red decreases downward");
        assert!(bt < bb, "blue increases downward");
    }

    #[test]
    fn gradient_to_right_varies_left_to_right() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:100px;\
            background:linear-gradient(to right, #ff0000, #0000ff)}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 100.0);
        let (rl, _, bl, _) = px(&pm, 2, 50);
        let (rr, _, br, _) = px(&pm, 97, 50);
        assert!(rl > bl, "left red-dominant");
        assert!(br > rr, "right blue-dominant");
    }

    #[test]
    fn rounded_corner_is_background_through() {
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:100px;background:#ff0000;border-radius:30px}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 100.0);
        // a corner pixel shows the white canvas through the rounded cut.
        assert_eq!(px(&pm, 1, 1), (255, 255, 255, 255));
        // center is red.
        assert_eq!(px(&pm, 50, 50), (255, 0, 0, 255));
        // top-edge midpoint (the straight part) is red.
        assert_eq!(px(&pm, 50, 1), (255, 0, 0, 255));
    }

    #[test]
    fn box_shadow_paints_dark_pixels_at_offset() {
        // A tall wrapper keeps the page height ≥ the shadow's bottom edge.
        let html = "<html><head><style>\
            body{margin:0} #w{height:120px} \
            #b{width:50px;height:50px;background:#ffffff;box-shadow:10px 10px 0 0 #000000}\
            </style></head><body><div id='w'><div id='b'></div></div></body></html>";
        let pm = render_html_cwd(html, 120.0);
        // inside the shadow rect but outside the 50×50 box → dark.
        let (r, g, b, _) = px(&pm, 58, 58);
        assert!(r < 60 && g < 60 && b < 60, "shadow pixel dark: {:?}", px(&pm, 58, 58));
        // box interior covers its own shadow → white.
        assert_eq!(px(&pm, 25, 25), (255, 255, 255, 255));
        // far from both → white.
        assert_eq!(px(&pm, 110, 110), (255, 255, 255, 255));
    }

    #[test]
    fn box_shadow_blur_is_a_gradient() {
        let html = "<html><head><style>\
            body{margin:0} div{width:50px;height:50px;background:#ffffff;\
            box-shadow:0px 0px 8px 0 #000000}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 120.0);
        // Just outside the box edge a blurred shadow gives a grey (between black
        // and white) — scan the band right of the box for a grey pixel.
        let mut found_grey = false;
        for x in 50..70 {
            let (r, _, _, _) = px(&pm, x, 25);
            if r > 20 && r < 235 {
                found_grey = true;
                break;
            }
        }
        assert!(found_grey, "expected a grey blurred-shadow pixel right of the box");
    }

    #[test]
    fn opacity_half_black_over_white_is_grey() {
        let html = "<html><head><style>\
            body{margin:0} div{width:50px;height:50px;background:#000000;opacity:0.5}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 100.0);
        let (r, g, b, _) = px(&pm, 10, 10);
        for ch in [r, g, b] {
            assert!((120..=136).contains(&ch), "≈128 mid-grey, got {:?}", px(&pm, 10, 10));
        }
    }

    #[test]
    fn nested_opacity_multiplies() {
        // outer opacity 0.5 wrapping inner opacity-0.5 black box over white →
        // effective alpha 0.25 → ≈ (191,191,191).
        let html = "<html><head><style>\
            body{margin:0} \
            #o{opacity:0.5;width:50px;height:50px} \
            #i{opacity:0.5;width:50px;height:50px;background:#000000}\
            </style></head><body><div id='o'><div id='i'></div></div></body></html>";
        let pm = render_html_cwd(html, 100.0);
        let (r, g, b, _) = px(&pm, 10, 10);
        for ch in [r, g, b] {
            assert!((183..=199).contains(&ch), "≈191, got {:?}", px(&pm, 10, 10));
        }
    }

    #[test]
    fn solid_block_no_regression_after_migration() {
        // The pre-M5 solid-background assert must still hold byte-identically.
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        assert_eq!(px(&pm, 10, 10), (255, 0, 0, 255));
        assert_eq!(px(&pm, 150, 10), (255, 255, 255, 255));
    }

    #[test]
    fn img_intrinsic_render_height_grows() {
        let dir = fixture_dir_with_png();
        // No size attrs → intrinsic 2×2; page height ≥ 2.
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='px.png'></body></html>";
        let pm = render_html(html, 50.0, &dir);
        assert_eq!(pm.width(), 50);
        assert!(pm.height() >= 2);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255));
    }

    #[test]
    fn rounded_border_no_bg_interior_is_canvas() {
        // Rounded border with NO background: the border must paint only as a
        // ring — the interior shows the white canvas, not the border color.
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:100px;\
            border:10px solid #ff0000;border-radius:20px}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        // center is the canvas (white), NOT the border red.
        assert_eq!(px(&pm, 50, 50), (255, 255, 255, 255));
        // a point inside the border band (y≈5, mid-x) is red.
        assert_eq!(px(&pm, 50, 5), (255, 0, 0, 255));
    }

    #[test]
    fn rounded_border_opaque_bg_interior_is_bg() {
        // Same box but WITH an opaque background: interior is the bg color,
        // the border band is the border color.
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:100px;background:#00ff00;\
            border:10px solid #0000ff;border-radius:20px}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        // interior → green background.
        assert_eq!(px(&pm, 50, 50), (0, 255, 0, 255));
        // border band → blue.
        assert_eq!(px(&pm, 50, 5), (0, 0, 255, 255));
    }

    #[test]
    fn thick_border_on_small_box_no_panic() {
        // 40px border on a 50px box: the inner padding-box is tiny (≈10px) and
        // the inset corner radii clamp to ≥0 — must render without panicking
        // and the box is essentially all border color.
        let html = "<html><head><style>\
            body{margin:0} div{width:50px;height:50px;\
            border:40px solid #ff0000;border-radius:20px}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        // a point well inside the border band is red.
        assert_eq!(px(&pm, 65, 10), (255, 0, 0, 255));
    }

    #[test]
    fn huge_box_shadow_blur_does_not_panic() {
        // An absurd blur radius must not overflow the box-blur accumulation
        // (panic in debug) nor hang; the render completes and stays bounded.
        let html = "<html><head><style>\
            body{margin:0} #w{height:120px} \
            #b{width:50px;height:50px;background:#ffffff;\
            box-shadow:0 0 99999999px #000000}\
            </style></head><body><div id='w'><div id='b'></div></div></body></html>";
        let start = std::time::Instant::now();
        let pm = render_html_cwd(html, 120.0);
        assert!(
            start.elapsed().as_secs() < 5,
            "huge-blur shadow render hung ({:?})",
            start.elapsed()
        );
        assert_eq!(pm.width(), 120);
        assert!(pm.height() >= 1 && pm.height() <= MAX_DIMENSION);
    }
}
