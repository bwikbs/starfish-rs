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

use starfish_css::Stylesheet;
use starfish_dom::{Document, NodeId, NodeKind};
use starfish_layout::layout;
use starfish_style::style_tree;

pub use display::PaintCmd;
pub use font::{FontDb, FontMeasurer, GlyphBitmap};
pub use image_store::{DecodedImage, ImageStore};
pub use starfish_layout::{LayoutBox, Rect};
pub use starfish_net::{LoadError, LocalLoader, ResourceLoader, RouterLoader, Url};
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

/// Walk the DOM in document order, building the author `Vec<Stylesheet>`: for
/// each `<style>` parse its text, for each `<link rel="stylesheet" href>`
/// resolve `href` against `base`, fetch via `loader`, and parse the bytes as
/// UTF-8 CSS. One stylesheet per node, in the order encountered → the cascade
/// order `style_tree` consumes (later sheet wins same-specificity ties).
///
/// A `<link>` with no/empty `href`, a bad URL, or a failed fetch is skipped (no
/// panic, no sheet added) — the page still renders with the rest.
fn collect_author_sheets(
    doc: &Document,
    base: &Url,
    loader: &dyn ResourceLoader,
) -> Vec<Stylesheet> {
    let mut sheets = Vec::new();
    let mut stack = vec![doc.root()];
    // DFS; children are pushed reversed so we visit them in document order.
    while let Some(id) = stack.pop() {
        match doc.tag_name(id) {
            Some("style") => {
                let mut css = String::new();
                for c in doc.children(id) {
                    if let NodeKind::Text(t) = doc.kind(c) {
                        css.push_str(t);
                        css.push('\n');
                    }
                }
                sheets.push(starfish_css::parse_stylesheet(&css));
            }
            Some("link") if rel_is_stylesheet(doc.get_attribute(id, "rel")) => {
                if let Some(sheet) = load_link_sheet(doc, id, base, loader) {
                    sheets.push(sheet);
                }
            }
            _ => {}
        }
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    sheets
}

/// `rel` is a space-separated token list; match the `stylesheet` keyword
/// case-insensitively (e.g. `rel="stylesheet"`, `rel="StyleSheet"`).
fn rel_is_stylesheet(rel: Option<&str>) -> bool {
    rel.is_some_and(|r| {
        r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet"))
    })
}

/// Resolve + fetch + parse one linked stylesheet. `None` on any failure (skip).
fn load_link_sheet(
    doc: &Document,
    id: NodeId,
    base: &Url,
    loader: &dyn ResourceLoader,
) -> Option<Stylesheet> {
    let href = doc.get_attribute(id, "href")?.trim();
    if href.is_empty() {
        return None;
    }
    let url = base.join(href).ok()?; // BadUrl → skip
    let res = loader.fetch(&url).ok()?; // NotFound/Io/Unsupported → skip
    let css = String::from_utf8_lossy(&res.bytes); // assume UTF-8 (§8)
    Some(starfish_css::parse_stylesheet(&css))
}

/// Pre-pass: decode every `<img src>` in the document into `images` so layout
/// and paint can read intrinsic sizes / pixels immutably (E2-M4 §3.2).
fn decode_images(doc: &Document, images: &mut ImageStore<'_>) {
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

/// End-to-end: HTML string → rendered RGBA pixmap, resolving and fetching the
/// document's external resources (linked CSS, images) against `base` through
/// `loader`. `base` is the document's own URL. The page height grows with
/// content; the pixmap is `round(viewport_width) × round(root margin-box
/// height)` (each at least 1).
pub fn render_document(
    html: &str,
    base: &Url,
    viewport_width: f32,
    loader: &dyn ResourceLoader,
) -> Pixmap {
    let doc = starfish_html::parse(html);

    // Author sheets in document order (inline <style> + linked <link>).
    let author = collect_author_sheets(&doc, base, loader);
    let styled = style_tree(&doc, &author);
    let fonts = FontDb::load().expect("embedded faces load");

    // ImageStore resolves <img src> against `base` and fetches via `loader`.
    let mut images = ImageStore::new(base.clone(), loader);
    decode_images(&doc, &mut images);

    let root = layout(&doc, &styled, viewport_width, &FontMeasurer(&fonts), &images);

    let width = clamp_dimension(viewport_width);
    let height = clamp_dimension(root.dimensions().margin_box().height);

    paint(&root, &styled, &fonts, &images, width, height)
}

/// Render a local HTML file by path: builds a `file://` base URL and a
/// `LocalLoader`, reads the file, and renders it. The CLI's normal path.
pub fn render_path(path: &Path, viewport_width: f32) -> Result<Pixmap, LoadError> {
    let base = starfish_net::file_url_from_path(path)?;
    let html = std::fs::read_to_string(path).map_err(LoadError::Io)?;
    Ok(render_document(&html, &base, viewport_width, &LocalLoader))
}

/// Fetch and render a document by absolute `Url` (http/https/file) through a
/// scheme `RouterLoader`: GETs the document bytes, then `render_document` so
/// linked CSS + images resolve and fetch over the same router. The document's
/// own URL is the base, so relative resources resolve against it.
pub fn render_url(url: &Url, viewport_width: f32) -> Result<Pixmap, LoadError> {
    let loader = RouterLoader::new();
    let res = loader.fetch(url)?;
    let html = String::from_utf8_lossy(&res.bytes); // assume UTF-8 (charset → M3)
    // Resolve the page's relative sub-resources against the final (post-redirect)
    // URL, not the original input, so a 302 doesn't drop relative CSS/images.
    let base = res.final_url.as_ref().unwrap_or(url);
    Ok(render_document(&html, base, viewport_width, &loader))
}

/// BACK-COMPAT shim (E2-M4 API): render an HTML string with a base *directory*.
/// Builds a trailing-slash `file://` directory base from `base_dir` and a
/// `LocalLoader`, then delegates to `render_document`. The directory base joins
/// relative `src`/`href` exactly like the old `base_dir.join`, so existing
/// callers and the paint unit tests are unaffected.
pub fn render_html(html: &str, viewport_width: f32, base_dir: &Path) -> Pixmap {
    let dir = if base_dir.is_absolute() {
        base_dir.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(base_dir)
    };
    let base = Url::from_directory_path(&dir)
        .unwrap_or_else(|()| Url::parse("file:///").expect("static file URL"));
    render_document(html, &base, viewport_width, &LocalLoader)
}

/// BACK-COMPAT shim (E2-M4): base = current working directory. Keeps image-free
/// callers/tests compiling with a single argument fewer.
pub fn render_html_cwd(html: &str, viewport_width: f32) -> Pixmap {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    render_html(html, viewport_width, &cwd)
}

/// Paint an already-laid-out box tree to a pixmap of the given device size.
pub fn paint(
    layout_root: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore<'_>,
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

    // --- E3-M1: document-order author CSS via collect_author_sheets ---

    use starfish_net::{file_url_from_path, LocalLoader};

    /// A fresh temp dir for E3-M1 fixtures.
    fn e3_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-e3m1-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A `file://` base URL for a document `index.html` living in `dir`.
    fn base_in(dir: &Path) -> Url {
        file_url_from_path(&dir.join("index.html")).unwrap()
    }

    #[test]
    fn collect_author_sheets_inline_only() {
        let dir = e3_dir();
        let doc = starfish_html::parse(
            "<html><head><style>p{color:red}</style></head><body>x</body></html>",
        );
        let sheets = collect_author_sheets(&doc, &base_in(&dir), &LocalLoader);
        assert_eq!(sheets.len(), 1);
    }

    #[test]
    fn linked_then_inline_inline_wins() {
        // <link>(red) before <style>(blue): document order → blue wins.
        let dir = e3_dir();
        std::fs::write(dir.join("theme.css"), "p{color:red}").unwrap();
        let html = "<html><head>\
            <link rel='stylesheet' href='theme.css'>\
            <style>p{color:blue}</style>\
            </head><body><p>hi</p></body></html>";
        let doc = starfish_html::parse(html);
        let sheets = collect_author_sheets(&doc, &base_in(&dir), &LocalLoader);
        assert_eq!(sheets.len(), 2, "linked + inline");
        // Render and sample the glyph color region: blue text, no red.
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80), "expected blue text");
        assert!(!has_color(&pm, |r, g, b| r > 120 && g < 80 && b < 80), "no red text");
    }

    #[test]
    fn inline_then_linked_linked_wins() {
        // <style>(blue) before <link>(red): document order → red wins.
        let dir = e3_dir();
        std::fs::write(dir.join("theme.css"), "p{color:red}").unwrap();
        let html = "<html><head>\
            <style>p{color:blue}</style>\
            <link rel='stylesheet' href='theme.css'>\
            </head><body><p>hi</p></body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(has_color(&pm, |r, g, b| r > 120 && g < 80 && b < 80), "expected red text");
        assert!(!has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80), "no blue text");
    }

    #[test]
    fn missing_link_is_skipped_no_panic() {
        let dir = e3_dir();
        let html = "<html><head>\
            <link rel='stylesheet' href='missing.css'>\
            <style>p{color:green}</style>\
            </head><body><p>hi</p></body></html>";
        let doc = starfish_html::parse(html);
        let sheets = collect_author_sheets(&doc, &base_in(&dir), &LocalLoader);
        assert_eq!(sheets.len(), 1, "missing link skipped");
        // Renders without panic, green text present.
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(has_color(&pm, |r, g, b| g > 120 && r < 80 && b < 80), "expected green text");
    }

    #[test]
    fn link_without_href_or_non_stylesheet_rel_skipped() {
        let dir = e3_dir();
        let html = "<html><head>\
            <link rel='stylesheet'>\
            <link rel='icon' href='theme.css'>\
            <style>p{color:red}</style>\
            </head><body><p>hi</p></body></html>";
        let doc = starfish_html::parse(html);
        let sheets = collect_author_sheets(&doc, &base_in(&dir), &LocalLoader);
        assert_eq!(sheets.len(), 1, "only the <style> sheet");
    }

    /// True if any pixel in the top text band satisfies `pred(r,g,b)`.
    fn has_color(pm: &Pixmap, pred: impl Fn(u8, u8, u8) -> bool) -> bool {
        for y in 0..pm.height().min(40) {
            for x in 0..pm.width().min(160) {
                let (r, g, b, _) = px(pm, x, y);
                if pred(r, g, b) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn render_document_relative_img_resolves_and_loads() {
        let dir = e3_dir();
        write_e3_png(&dir.join("px.png"));
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='px.png' width='4' height='4'></body></html>";
        let pm = render_document(html, &base_in(&dir), 50.0, &LocalLoader);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255)); // TL red
        assert_eq!(px(&pm, 3, 0), (0, 255, 0, 255)); // TR green
        assert_eq!(px(&pm, 0, 3), (0, 0, 255, 255)); // BL blue
        assert_eq!(px(&pm, 3, 3), (255, 255, 255, 255)); // BR white
    }

    #[test]
    fn render_document_subdir_img_resolves() {
        let dir = e3_dir();
        let sub = dir.join("img");
        std::fs::create_dir_all(&sub).unwrap();
        write_e3_png(&sub.join("px.png"));
        let html = "<html><head><style>body{margin:0} img{display:block}</style></head>\
            <body><img src='img/px.png' width='4' height='4'></body></html>";
        let pm = render_document(html, &base_in(&dir), 50.0, &LocalLoader);
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255)); // TL red
    }

    #[test]
    fn render_document_broken_img_no_panic() {
        let dir = e3_dir();
        let html = "<html><head><style>body{margin:0}</style></head>\
            <body><img src='nope.png' width='10' height='10'></body></html>";
        let pm = render_document(html, &base_in(&dir), 50.0, &LocalLoader);
        // interior of the placeholder box stays white; grey border on top edge.
        assert_eq!(px(&pm, 5, 5), (255, 255, 255, 255));
        assert_eq!(px(&pm, 5, 0), (0x80, 0x80, 0x80, 255));
    }

    /// Write a 2×2 PNG (TL red, TR green, BL blue, BR white) at `path`.
    fn write_e3_png(path: &Path) {
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
        img.save(path).unwrap();
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
