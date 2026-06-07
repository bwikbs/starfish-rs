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

use starfish_css::{FontSrc, Stylesheet};
use starfish_dom::{Document, NodeId, NodeKind};
use starfish_layout::layout;
use starfish_style::style_tree;

pub use display::PaintCmd;
pub use font::{FontDb, FontMeasurer, GlyphBitmap};
pub use image_store::{DecodedImage, ImageStore};
pub use starfish_layout::{LayoutBox, Rect};
pub use starfish_net::{
    CachingLoader, DataLoader, LoadError, LocalLoader, ResourceLoader, RouterLoader, Url,
};
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

/// E6-M2: load every captured `@font-face` and register it into `fonts`. For
/// each rule, the `src` entries are tried in source order until one yields valid
/// font bytes (fetched + parsed); the first that registers wins, the rest are
/// not fetched. All failures are non-fatal (skip the entry/rule, no panic) — the
/// family then falls back to normal fontdb matching.
fn load_font_faces(sheets: &[Stylesheet], base: &Url, loader: &dyn ResourceLoader, fonts: &mut FontDb) {
    for sheet in sheets {
        for ff in &sheet.font_faces {
            for src in &ff.src {
                let bytes = match src {
                    FontSrc::Url { url, format } => {
                        if !format_supported(format.as_deref()) {
                            continue; // skip woff/woff2/… fontdue can't read.
                        }
                        let Ok(u) = base.join(url) else { continue };
                        let Ok(res) = loader.fetch(&u) else { continue };
                        res.bytes
                    }
                    FontSrc::Local(name) => match fonts.system_face_bytes(name) {
                        Some(b) => b,
                        None => continue,
                    },
                };
                if fonts.add_face(&ff.family, ff.weight, ff.style, bytes) {
                    break; // registered → stop trying this rule's sources.
                }
                // parse failed → fall through to the next src.
            }
        }
    }
}

/// Whether a `format()` hint names a raw TTF/OTF fontdue can parse. `None` (no
/// hint) ⇒ attempt it (fontdue's `Err` is the catch-all). WOFF/WOFF2/EOT/SVG are
/// deferred and skipped early.
fn format_supported(format: Option<&str>) -> bool {
    match format {
        None => true,
        Some(f) => matches!(
            f,
            "truetype" | "opentype" | "truetype-collection" | "opentype-collection"
        ),
    }
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
    let mut doc = starfish_html::parse(html);

    // E8-M1: collect author sheets BEFORE scripts so `getComputedStyle` can
    // cascade on demand during scripting. Threaded into `run_scripts`.
    let pre_sheets = std::rc::Rc::new(collect_author_sheets(&doc, base, loader));

    // E4-M1/M2: run <script>s against the (mutable) document BEFORE styling, so
    // styling sees the post-script DOM.
    let js = starfish_js::run_scripts(&mut doc, base, loader, pre_sheets);
    report_console(&js);

    // E8-M1: re-collect author sheets AFTER scripts — a script may have injected
    // a <style>/<link> (via innerHTML/insertAdjacentHTML) that must affect the
    // final render. For a no-script page this is an identical second walk.
    let author = collect_author_sheets(&doc, base, loader);
    let styled = style_tree(&doc, &author);
    let mut fonts = FontDb::new();
    // E6-M2: fetch + register @font-face faces BEFORE layout, so layout's
    // measurer and paint both resolve them (measure == paint).
    load_font_faces(&author, base, loader, &mut fonts);

    // ImageStore resolves <img src> against `base` and fetches via `loader`.
    let mut images = ImageStore::new(base.clone(), loader);
    decode_images(&doc, &mut images);

    let root = layout(&doc, &styled, viewport_width, &FontMeasurer(&fonts), &images);

    let width = clamp_dimension(viewport_width);
    let height = clamp_dimension(root.dimensions().margin_box().height);

    paint(&root, &styled, &fonts, &images, width, height)
}

/// Surface captured `console`/script-error output: `render_document` returns
/// only a `Pixmap` (its signature must not change), so script output is made
/// visible by printing — warnings/errors to stderr, the rest to stdout.
fn report_console(js: &starfish_js::ScriptOutcome) {
    use starfish_js::ConsoleLevel;
    for m in &js.console {
        match m.level {
            ConsoleLevel::Error | ConsoleLevel::Warn => eprintln!("[console] {}", m.text),
            _ => println!("[console] {}", m.text),
        }
    }
    for e in &js.errors {
        eprintln!("[script error] {}", e.message);
    }
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
    // CachingLoader dedupes the document/CSS/image fetches by URL within this
    // render (e.g. the same <link href> twice → fetched once).
    let loader = CachingLoader::new(RouterLoader::new());
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

    #[test]
    fn render_document_infinite_font_size_no_panic() {
        // CSS `1e999px` tokenizes to f32::INFINITY; the size must be sanitized
        // before reaching fontdue so the render does not panic.
        let dir = e3_dir();
        let html = "<html><head><style>p{font-size:1e999px}</style></head>\
            <body><p>hi</p></body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(pm.width() > 0 && pm.height() > 0, "valid pixmap");
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

    // --- E3-M3: data: URLs + caching + UTF-8 robustness ---

    /// Base64 of a 2×2 PNG (TL red, TR green, BL blue, BR white).
    fn png_2x2_base64() -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        STANDARD.encode(buf.into_inner())
    }

    /// A `data:` document base. Relative sub-resources can't resolve against it,
    /// but absolute `data:` sub-resources (our case) join fine.
    fn data_base() -> Url {
        Url::parse("data:text/html,doc").unwrap()
    }

    #[test]
    fn data_img_renders_pixels() {
        let b64 = png_2x2_base64();
        let html = format!(
            "<html><head><style>body{{margin:0}} img{{display:block}}</style></head>\
             <body><img src='data:image/png;base64,{b64}' width='4' height='4'></body></html>"
        );
        let pm = render_document(&html, &data_base(), 50.0, &RouterLoader::new());
        assert_eq!(px(&pm, 0, 0), (255, 0, 0, 255)); // TL red
        assert_eq!(px(&pm, 3, 0), (0, 255, 0, 255)); // TR green
        assert_eq!(px(&pm, 0, 3), (0, 0, 255, 255)); // BL blue
        assert_eq!(px(&pm, 3, 3), (255, 255, 255, 255)); // BR white
    }

    #[test]
    fn data_css_link_applies_plain() {
        let html = "<html><head>\
            <link rel='stylesheet' href='data:text/css,p{color:blue}'>\
            </head><body><p>hi</p></body></html>";
        let pm = render_document(html, &data_base(), 200.0, &RouterLoader::new());
        assert!(has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80), "expected blue text");
    }

    #[test]
    fn data_css_link_applies_base64() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        let b64 = STANDARD.encode("p{color:blue}");
        let html = format!(
            "<html><head>\
             <link rel='stylesheet' href='data:text/css;base64,{b64}'>\
             </head><body><p>hi</p></body></html>"
        );
        let pm = render_document(&html, &data_base(), 200.0, &RouterLoader::new());
        assert!(has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80), "expected blue text");
    }

    #[test]
    fn malformed_data_subresources_no_panic() {
        // A bad-base64 data: image and a no-comma data: link are both skipped;
        // the page still renders with the inline green style.
        let html = "<html><head>\
            <link rel='stylesheet' href='data:text/plain'>\
            <style>p{color:green}</style>\
            </head><body>\
            <p>hi</p>\
            <img src='data:image/png;base64,@@@' width='10' height='10'>\
            </body></html>";
        let pm = render_document(html, &data_base(), 200.0, &RouterLoader::new());
        assert!(has_color(&pm, |r, g, b| g > 120 && r < 80 && b < 80), "green text present");
    }

    #[test]
    fn invalid_utf8_css_is_lossy_no_panic() {
        // CSS bytes with an invalid UTF-8 tail; from_utf8_lossy must not panic.
        struct Bad;
        impl ResourceLoader for Bad {
            fn fetch(&self, url: &Url) -> Result<starfish_net::Resource, LoadError> {
                Ok(starfish_net::Resource {
                    bytes: b"p{color:green}\xFF\xFE".to_vec(),
                    content_type: Some("text/css".into()),
                    final_url: Some(url.clone()),
                })
            }
        }
        let base = Url::parse("file:///x/index.html").unwrap();
        let html = "<html><head>\
            <link rel='stylesheet' href='theme.css'>\
            </head><body><p>hi</p></body></html>";
        let pm = render_document(html, &base, 200.0, &Bad);
        // The valid prefix parsed → green text; no panic on the invalid tail.
        assert!(has_color(&pm, |r, g, b| g > 120 && r < 80 && b < 80), "green text present");
    }

    /// A loader serving a fixed CSS body, counting fetches via a shared `Cell`
    /// the test still holds (so we can read the count after the cache wraps it).
    struct CountingCss {
        hits: std::rc::Rc<std::cell::Cell<u32>>,
    }
    impl ResourceLoader for CountingCss {
        fn fetch(&self, url: &Url) -> Result<starfish_net::Resource, LoadError> {
            self.hits.set(self.hits.get() + 1);
            Ok(starfish_net::Resource {
                bytes: b"p{color:blue}".to_vec(),
                content_type: Some("text/css".into()),
                final_url: Some(url.clone()),
            })
        }
    }

    #[test]
    fn repeated_linked_css_fetched_once_via_cache() {
        let base = Url::parse("file:///x/index.html").unwrap();
        let html = "<html><head>\
            <link rel='stylesheet' href='theme.css'>\
            <link rel='stylesheet' href='theme.css'>\
            </head><body><p>hi</p></body></html>";
        let hits = std::rc::Rc::new(std::cell::Cell::new(0));
        let loader = CachingLoader::new(CountingCss { hits: hits.clone() });
        let pm = render_document(html, &base, 200.0, &loader);
        // Style still applies (blue text).
        assert!(has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80), "blue text present");
        // The same CSS URL was fetched exactly once despite two <link>s.
        assert_eq!(hits.get(), 1, "css fetched once");
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

    // --- E4-M1: JS phase is render-transparent + non-fatal ---

    #[test]
    fn script_page_renders_identically_to_no_script() {
        // Two HTMLs differing only by an added <script> in <head>: in M1 the
        // script has no DOM effect, so both render to identical pixmap bytes.
        let base = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div></div></body></html>";
        let with_script = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style><script>console.log('x')</script></head><body><div></div></body></html>";
        let dir = e3_dir();
        let no_js = render_document(base, &base_in(&dir), 200.0, &LocalLoader);
        let with_js = render_document(with_script, &base_in(&dir), 200.0, &LocalLoader);
        assert_eq!(no_js.data(), with_js.data(), "JS phase must be render-transparent");
    }

    #[test]
    fn throwing_script_does_not_break_rendering() {
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style><script>throw 1</script></head><body><div></div></body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        // Page still renders: the red block pixel is red (no panic, no blank page).
        assert_eq!(px(&pm, 10, 10), (255, 0, 0, 255));
    }

    #[test]
    fn no_script_page_zero_overhead_regression() {
        // The script-free path takes the early return (no Boa Context) and must
        // render byte-identically to the pre-E4 baseline assert.
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        assert_eq!(px(&pm, 10, 10), (255, 0, 0, 255));
        assert_eq!(px(&pm, 150, 10), (255, 255, 255, 255));
    }

    // --- E4-M2: DOM mutations + inline style are visible in the render ---

    /// Scan the whole frame for any pixel matching `pred`.
    fn any_pixel(pm: &Pixmap, pred: impl Fn(u8, u8, u8) -> bool) -> bool {
        for y in 0..pm.height() {
            for x in 0..pm.width() {
                let (r, g, b, _) = px(pm, x, y);
                if pred(r, g, b) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn script_inline_style_recolors_box() {
        // A script sets style.background to green; the box must paint green.
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} #x{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div id='x'></div>\
            <script>document.getElementById('x').style.background='#00ff00'</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        // Box top-left is now green (inline style beat the author red).
        assert_eq!(px(&pm, 5, 5), (0, 255, 0, 255));
        assert!(!any_pixel(&pm, |r, g, b| r > 200 && g < 80 && b < 80), "no red left");
    }

    #[test]
    fn script_set_class_triggers_author_rule() {
        // setAttribute('class','hot') makes the author `.hot{background}` apply.
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} div{width:100px;height:50px;background:#ffffff}\
            .hot{background:#0000ff}\
            </style></head><body><div id='x'></div>\
            <script>document.getElementById('x').setAttribute('class','hot')</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert_eq!(px(&pm, 5, 5), (0, 0, 255, 255));
    }

    #[test]
    fn script_creates_visible_box() {
        // createElement + appendChild a styled block → it appears in the render.
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} .added{width:100px;height:40px;background:#00ff00}\
            </style></head><body><div id='root'></div>\
            <script>var c=document.createElement('div');\
            c.setAttribute('class','added');\
            document.getElementById('root').appendChild(c)</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(any_pixel(&pm, |r, g, b| r < 80 && g > 200 && b < 80), "added green box");
    }

    #[test]
    fn no_op_script_renders_identically_to_no_script() {
        // A script that reads but does not mutate the DOM must render byte-for-
        // byte identically to the same page without the script (no regression).
        let base = "<html><head><style>\
            body{margin:0} div{width:80px;height:30px;background:#123456}\
            </style></head><body><div></div></body></html>";
        let with_script = "<html><head><style>\
            body{margin:0} div{width:80px;height:30px;background:#123456}\
            </style><script>var _=document.body;\
            document.querySelectorAll('div').length</script>\
            </head><body><div></div></body></html>";
        let dir = e3_dir();
        let no_js = render_document(base, &base_in(&dir), 150.0, &LocalLoader);
        let with_js = render_document(with_script, &base_in(&dir), 150.0, &LocalLoader);
        assert_eq!(no_js.data(), with_js.data(), "read-only DOM access must not change pixels");
    }

    // --- E4-M3: timer / event-driven mutations are visible in the render ---

    #[test]
    fn set_timeout_recolors_box_before_paint() {
        // A setTimeout(5ms) callback recolors the box; the bounded virtual-time
        // drain runs it before paint, so the rendered pixel is orange.
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} #x{width:100px;height:50px;background:#ff0000}\
            </style></head><body><div id='x'></div>\
            <script>setTimeout(function(){\
              document.getElementById('x').style.background='#ff8800'; }, 5)</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        // #ff8800 orange at the box top-left; the author red is gone.
        assert_eq!(px(&pm, 5, 5), (255, 136, 0, 255));
        assert!(!any_pixel(&pm, |r, g, b| r > 200 && g < 80 && b < 80), "no red left");
    }

    #[test]
    fn window_load_listener_appends_box() {
        // A `load` listener appends a styled box → it appears in the render.
        let dir = e3_dir();
        let html = "<html><head><style>\
            body{margin:0} .added{width:100px;height:40px;background:#00ff00}\
            </style></head><body><div id='root'></div>\
            <script>window.addEventListener('load', function(){\
              var c=document.createElement('div'); c.setAttribute('class','added');\
              document.getElementById('root').appendChild(c); });</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(any_pixel(&pm, |r, g, b| r < 80 && g > 200 && b < 80), "load-appended green box");
    }

    // --- E8-M1: innerHTML / insertAdjacentHTML / script-injected <style> visible
    //     in the render. Markup goes in JS strings as `\x3c`/`\x3e` because the
    //     M1 parser has no <script> rawtext mode (a literal `<` truncates source).

    #[test]
    fn script_inner_html_renders_new_content() {
        let dir = e3_dir();
        let html = "<html><head><style>body{margin:0}</style></head>\
            <body><div id='app'></div>\
            <script>document.getElementById('app').innerHTML=\
              '\\x3cdiv style=\"width:100px;height:40px;background:#00ff00\"\\x3e\\x3c/div\\x3e';\
            </script></body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(
            any_pixel(&pm, |r, g, b| r < 80 && g > 200 && b < 80),
            "innerHTML-built green box must render"
        );
    }

    #[test]
    fn script_insert_adjacent_html_adds_box() {
        let dir = e3_dir();
        let html = "<html><head><style>body{margin:0}</style></head>\
            <body><div id='c'></div>\
            <script>document.getElementById('c').insertAdjacentHTML('beforeend',\
              '\\x3cdiv style=\"background:#00ff00;width:50px;height:50px\"\\x3e\\x3c/div\\x3e');\
            </script></body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(
            any_pixel(&pm, |r, g, b| r < 80 && g > 200 && b < 80),
            "insertAdjacentHTML green box must render"
        );
    }

    #[test]
    fn script_injected_style_affects_render() {
        // A script builds a <style> node and appends it; the post-script
        // re-collect (§6.2) must honor it → the <p> paints blue. (createElement
        // is used instead of innerHTML because a fragment-parsed <style> lands in
        // the throwaway <head>, which the body-child graft does not lift.)
        let dir = e3_dir();
        let html = "<html><head><style>body{margin:0}</style></head>\
            <body><p>hi</p>\
            <script>var s=document.createElement('style');\
              s.textContent='p{color:blue}';\
              document.getElementsByTagName('head')[0].appendChild(s);</script>\
            </body></html>";
        let pm = render_document(html, &base_in(&dir), 200.0, &LocalLoader);
        assert!(
            has_color(&pm, |r, g, b| b > 120 && r < 80 && g < 80),
            "script-injected <style> must color the <p> blue"
        );
    }

    // --- E5-M3: 2D transforms (font-independent pixel sampling) ---

    /// Render a single `#d` box with the given CSS rules on a white 200×200
    /// canvas. `#d` is absolutely positioned at (0,0) so transforms that move
    /// pixels off the box's flow rect still land on the 200px-tall canvas (a
    /// `#spacer` forces the page height; transforms are paint-only and don't
    /// grow the canvas themselves).
    fn render_box(rules: &str) -> Pixmap {
        let html = format!(
            "<html><head><style>body{{margin:0}} \
             #d{{position:absolute;top:0;left:0;{rules}}} \
             #spacer{{width:1px;height:200px}}</style></head>\
             <body><div id='d'></div><div id='spacer'></div></body></html>"
        );
        render_html_cwd(&html, 200.0)
    }

    fn is_red(p: (u8, u8, u8, u8)) -> bool {
        p.0 > 200 && p.1 < 80 && p.2 < 80
    }
    fn is_white(p: (u8, u8, u8, u8)) -> bool {
        p.0 > 240 && p.1 > 240 && p.2 > 240
    }

    #[test]
    fn transform_translate_moves_pixels() {
        let pm = render_box("width:40px;height:40px;background:#ff0000;transform:translate(50px,30px)");
        // original top-left region is now white (box moved away).
        assert!(is_white(px(&pm, 5, 5)), "origin cleared: {:?}", px(&pm, 5, 5));
        // shifted center (50+20, 30+20) = (70,50) is red.
        assert!(is_red(px(&pm, 70, 50)), "shifted center red: {:?}", px(&pm, 70, 50));
    }

    #[test]
    fn transform_scale_about_center() {
        // 40x40 box at (0,0), origin center (20,20). scale(2) → covers (-20,-20)..(60,60).
        let pm = render_box("width:40px;height:40px;background:#ff0000;transform:scale(2)");
        // (2,2) was white pre-scale (inside the box only after doubling) → red.
        assert!(is_red(px(&pm, 2, 2)), "scaled box covers (2,2): {:?}", px(&pm, 2, 2));
        // a pixel near (55,55) inside the doubled box → red.
        assert!(is_red(px(&pm, 55, 55)), "doubled extent: {:?}", px(&pm, 55, 55));
        // far pixel well outside → white.
        assert!(is_white(px(&pm, 120, 120)), "outside doubled box: {:?}", px(&pm, 120, 120));
    }

    #[test]
    fn transform_rotate_90_about_center() {
        // 80x20 box at (0,0), origin center (40,10). rotate(90deg) → 20x80 vertical
        // strip centered at (40,10): x in [30,50], y in [-30,50].
        let pm = render_box("width:80px;height:20px;background:#ff0000;transform:rotate(90deg)");
        // red only post-rotation (on the vertical strip).
        assert!(is_red(px(&pm, 40, 45)), "vertical strip: {:?}", px(&pm, 40, 45));
        // red only pre-rotation (off the strip after rotation) → white.
        assert!(is_white(px(&pm, 70, 10)), "off strip cleared: {:?}", px(&pm, 70, 10));
    }

    #[test]
    fn transform_origin_pivot_differs_from_center() {
        // 80x20 box rotated 90deg (clockwise). Center pivot (40,10): the box maps
        // to a vertical strip x in [30,50], y in [-30,50] (clipped to y>=0), so
        // (40,30) is red. Corner pivot (0,0): point (x,y) -> (-y, x), so the strip
        // lands at x in [-20,0] (off-canvas left) -> (40,30) is white. Distinguishes.
        let corner = render_box(
            "width:80px;height:20px;background:#ff0000;transform:rotate(90deg);transform-origin:0 0",
        );
        let center = render_box("width:80px;height:20px;background:#ff0000;transform:rotate(90deg)");
        assert!(is_red(px(&center, 40, 30)), "center pivot covers (40,30): {:?}", px(&center, 40, 30));
        assert!(is_white(px(&corner, 40, 30)), "corner pivot misses (40,30): {:?}", px(&corner, 40, 30));
    }

    #[test]
    fn transform_skewx_shifts() {
        // skewX shears the box rightward proportional to y (about origin center).
        // 40x40 box, origin center (20,20): at y=35 the shift is tan(30)*(35-20)
        // ~= 8.7, so content reaches x ~= 48 (past the un-skewed right edge 40).
        let pm = render_box("width:40px;height:40px;background:#ff0000;transform:skewX(30deg)");
        assert!(is_red(px(&pm, 45, 35)), "sheared right at bottom: {:?}", px(&pm, 45, 35));
        // The opposite (top-left) corner is sheared LEFT, leaving (2,38) ... actually
        // the top is sheared left; the bottom-left interior near x=2,y=2 is sheared
        // left off the box -> white. Sample the un-sheared upper-left far corner.
        assert!(is_white(px(&pm, 2, 38)), "lower-left sheared away: {:?}", px(&pm, 2, 38));
    }

    #[test]
    fn transform_matrix_equals_translatex() {
        // matrix(1,0,0,1,30,0) ≡ translateX(30px).
        let m = render_box("width:40px;height:40px;background:#ff0000;transform:matrix(1,0,0,1,30,0)");
        let t = render_box("width:40px;height:40px;background:#ff0000;transform:translateX(30px)");
        // both: red at (50,20), white at the cleared origin (5,5).
        assert!(is_red(px(&m, 50, 20)) && is_red(px(&t, 50, 20)), "both shifted red");
        assert!(is_white(px(&m, 5, 5)) && is_white(px(&t, 5, 5)), "both cleared origin");
    }

    #[test]
    fn transform_composition_order_matters() {
        // translate(40px,0) rotate(90deg) vs rotate(90deg) translate(40px,0) land
        // the box differently. Sample a pixel that distinguishes them.
        let tr = render_box("width:40px;height:20px;background:#ff0000;transform:translate(40px,0) rotate(90deg)");
        let rt = render_box("width:40px;height:20px;background:#ff0000;transform:rotate(90deg) translate(40px,0)");
        // The two renders differ somewhere in the relevant region.
        let mut differ = false;
        for y in 0..120u32 {
            for x in 0..120u32 {
                if is_red(px(&tr, x, y)) != is_red(px(&rt, x, y)) {
                    differ = true;
                }
            }
        }
        assert!(differ, "composition order should change the result");
    }

    #[test]
    fn transform_does_not_affect_layout() {
        // #a transformed, #b a normal sibling. #b's green bg must paint at its
        // untransformed flow position (y in [40,60)).
        let html = "<html><head><style>body{margin:0} \
            #a{width:100px;height:40px;background:#ff0000;transform:translate(80px,0)} \
            #b{width:100px;height:20px;background:#00ff00}</style></head>\
            <body><div id='a'></div><div id='b'></div></body></html>";
        let pm = render_html_cwd(html, 200.0);
        // #b green at (10,50) — its normal position, unaffected by #a's transform.
        let p = px(&pm, 10, 50);
        assert!(p.1 > 200 && p.0 < 80 && p.2 < 80, "sibling at flow position: {p:?}");
    }

    #[test]
    fn transform_with_opacity_half_blends() {
        // black box, opacity 0.5, translated; over white → ~ (128,128,128).
        let pm = render_box(
            "width:40px;height:40px;background:#000000;opacity:0.5;transform:translate(50px,50px)",
        );
        let p = px(&pm, 70, 70); // (50+20,50+20) translated center
        assert!(
            (110..=145).contains(&p.0) && (110..=145).contains(&p.1) && (110..=145).contains(&p.2),
            "half-blended grey: {p:?}"
        );
    }

    #[test]
    fn no_transform_render_byte_identical() {
        // A box with no transform must render byte-for-byte the same as before
        // (no layer, fast path). Compare a transform:none render to a plain one.
        let plain = render_box("width:60px;height:40px;background:#0000ff");
        let none = render_box("width:60px;height:40px;background:#0000ff;transform:none");
        assert_eq!(plain.data(), none.data(), "transform:none must be byte-identical");
    }

    #[test]
    fn degenerate_scale_zero_no_panic() {
        // scale(0) collapses the box to nothing; must not panic and paints nothing.
        let pm = render_box("width:40px;height:40px;background:#ff0000;transform:scale(0)");
        // no red anywhere.
        assert!(!any_pixel(&pm, |r, g, b| r > 200 && g < 80 && b < 80), "scale(0) paints nothing");
    }

    #[test]
    fn rotated_translated_box_renders_tilted() {
        // VISUAL: a rotated + translated colored box renders as a tilted shape.
        let pm = render_box(
            "width:60px;height:30px;background:#3366cc;transform:translate(40px,40px) rotate(25deg)",
        );
        // the blue box is present somewhere offset from the origin.
        assert!(
            any_pixel(&pm, |r, g, b| (40..=80).contains(&r) && (90..=130).contains(&g) && b > 180),
            "tilted blue box present"
        );
        // the untransformed origin region (0,0) is white (box moved away).
        assert!(is_white(px(&pm, 2, 2)), "origin cleared: {:?}", px(&pm, 2, 2));
    }

    // --- E6-M1: font matching drives layout + paint (deterministic, vendored) ---

    /// The content width of the first TextRun fragment matching `t`, laid out
    /// deterministically with the vendored-only FontDb.
    fn text_run_width(html: &str, css: &str, t: &str) -> f32 {
        use starfish_layout::BoxKind;
        let doc = starfish_html::parse(html);
        let sheet = starfish_css::parse_stylesheet(css);
        let styled = starfish_style::style_tree(&doc, &[sheet]);
        let fonts = FontDb::vendored_only();
        let images = ImageStore::new(Url::parse("file:///").unwrap(), &LocalLoader);
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        fn find(b: &LayoutBox, t: &str) -> Option<f32> {
            if b.kind() == BoxKind::TextRun && b.text() == Some(t) {
                return Some(b.dimensions().content.width);
            }
            for c in b.children() {
                if let Some(w) = find(c, t) {
                    return Some(w);
                }
            }
            None
        }
        find(&root, t).unwrap_or_else(|| panic!("no text run {t:?}"))
    }

    #[test]
    fn monospace_run_measures_differently_from_sans() {
        // Same string + size; mono vs sans resolve to distinct faces, so the
        // measured (laid-out) text-run width differs — proving real metrics drive
        // line breaking, not a fixed 0.5*size approximation.
        let mono = text_run_width(
            "<html><body><p>Illili</p></body></html>",
            "body{margin:0} p{margin:0;font-family:monospace;font-size:16px}",
            "Illili",
        );
        let sans = text_run_width(
            "<html><body><p>Illili</p></body></html>",
            "body{margin:0} p{margin:0;font-family:sans-serif;font-size:16px}",
            "Illili",
        );
        assert!(mono > 0.0 && sans > 0.0);
        assert_ne!(mono, sans, "mono {mono} vs sans {sans} widths differ");
    }

    /// Render a one-line `<p>` with the given CSS to a vendored-only pixmap.
    fn render_vendored(html: &str) -> Pixmap {
        let doc = starfish_html::parse(html);
        let author = collect_author_sheets(&doc, &Url::parse("file:///").unwrap(), &LocalLoader);
        let styled = starfish_style::style_tree(&doc, &author);
        let fonts = FontDb::vendored_only();
        let images = ImageStore::new(Url::parse("file:///").unwrap(), &LocalLoader);
        let root = layout(&doc, &styled, 440.0, &FontMeasurer(&fonts), &images);
        let h = clamp_dimension(root.dimensions().margin_box().height);
        paint(&root, &styled, &fonts, &images, 440, h)
    }

    #[test]
    fn distinct_faces_render_and_no_regression() {
        // A page with sans / serif / monospace / bold / italic lines renders with
        // visibly distinct faces. We assert each line drew glyphs (non-white
        // pixels) and that mono vs sans widths differ (distinct faces drove
        // layout). No exact-pixel / system-font dependence.
        let html = "<html><head><style>\
            body{margin:0;color:#000000;font-size:20px} \
            p{margin:0} \
            .sans{font-family:sans-serif} \
            .serif{font-family:serif} \
            .mono{font-family:monospace} \
            .bold{font-weight:700} \
            .ital{font-family:'Liberation Serif';font-style:italic}\
            </style></head><body>\
            <p class='sans'>The quick brown fox</p>\
            <p class='serif'>The quick brown fox</p>\
            <p class='mono'>The quick brown fox</p>\
            <p class='bold'>The quick brown fox</p>\
            <p class='ital'>The quick brown fox</p>\
            </body></html>";
        let pm = render_vendored(html);
        // At least some glyph pixels were drawn somewhere on the page.
        assert!(
            any_pixel(&pm, |r, g, b| (r, g, b) != (255, 255, 255)),
            "expected rendered glyph pixels"
        );
        // mono vs sans widths differ for the same string.
        let css = "body{margin:0} p{margin:0;font-size:20px}";
        let mono = text_run_width(
            "<html><body><p>The</p></body></html>",
            &format!("{css} p{{font-family:monospace;font-size:20px}}"),
            "The",
        );
        let sans = text_run_width(
            "<html><body><p>The</p></body></html>",
            &format!("{css} p{{font-family:sans-serif;font-size:20px}}"),
            "The",
        );
        assert_ne!(mono, sans, "mono and sans render at different widths");
    }

    #[test]
    fn ordinary_page_still_renders_text() {
        // NO-REGRESSION: a plain default-font paragraph still draws glyphs.
        let pm = render_vendored(
            "<html><head><style>body{margin:0;color:#000000;font-size:20px}</style></head>\
             <body><p>Hello world</p></body></html>",
        );
        let mut found = false;
        'scan: for y in 0..pm.height().min(40) {
            for x in 0..pm.width().min(200) {
                if px(&pm, x, y) != (255, 255, 255, 255) {
                    found = true;
                    break 'scan;
                }
            }
        }
        assert!(found, "expected glyph pixels in an ordinary page");
    }

    // --- E6-M2: @font-face end-to-end (vendored-only, deterministic) ---

    /// One vendored TTF served as the @font-face file: Liberation Serif, whose
    /// metrics differ from the default DejaVu Sans, so a face change is visible.
    const WEBFONT_TTF: &[u8] = include_bytes!("../assets/LiberationSerif-Regular.ttf");
    const WEBFONT_BOLD_TTF: &[u8] = include_bytes!("../assets/LiberationSerif-Bold.ttf");

    /// The content width of the first TextRun matching `t`, laid out over the
    /// full E6-M2 pipeline (collect sheets → vendored FontDb → load_font_faces →
    /// layout) so @font-face faces drive measurement.
    fn webfont_run_width(html: &str, base: &Url, loader: &dyn ResourceLoader, t: &str) -> f32 {
        use starfish_layout::BoxKind;
        let doc = starfish_html::parse(html);
        let author = collect_author_sheets(&doc, base, loader);
        let styled = starfish_style::style_tree(&doc, &author);
        let mut fonts = FontDb::vendored_only();
        load_font_faces(&author, base, loader, &mut fonts);
        let images = ImageStore::new(base.clone(), loader);
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        fn find(b: &LayoutBox, t: &str) -> Option<f32> {
            if b.kind() == BoxKind::TextRun && b.text() == Some(t) {
                return Some(b.dimensions().content.width);
            }
            for c in b.children() {
                if let Some(w) = find(c, t) {
                    return Some(w);
                }
            }
            None
        }
        find(&root, t).unwrap_or_else(|| panic!("no text run {t:?}"))
    }

    /// Render the full E6-M2 pipeline (with @font-face loading) to a pixmap.
    fn render_webfont(html: &str, base: &Url, loader: &dyn ResourceLoader) -> Pixmap {
        let doc = starfish_html::parse(html);
        let author = collect_author_sheets(&doc, base, loader);
        let styled = starfish_style::style_tree(&doc, &author);
        let mut fonts = FontDb::vendored_only();
        load_font_faces(&author, base, loader, &mut fonts);
        let images = ImageStore::new(base.clone(), loader);
        let root = layout(&doc, &styled, 440.0, &FontMeasurer(&fonts), &images);
        let h = clamp_dimension(root.dimensions().margin_box().height);
        paint(&root, &styled, &fonts, &images, 440, h)
    }

    /// Base64 of the vendored webfont, for a `data:` URL @font-face src.
    fn webfont_base64(bytes: &[u8]) -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        STANDARD.encode(bytes)
    }

    #[test]
    fn font_face_file_url_drives_layout() {
        // @font-face served as a file:// ttf; the <p> using it measures with the
        // loaded face → width differs from the same <p> in the default sans.
        let dir = e3_dir();
        std::fs::write(dir.join("myfont.ttf"), WEBFONT_TTF).unwrap();
        let base = base_in(&dir);
        let with_face = "<html><head><style>\
            @font-face{font-family:\"MyFont\";src:url(\"myfont.ttf\")} \
            body{margin:0} p{margin:0;font-family:MyFont;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let plain = "<html><head><style>\
            body{margin:0} p{margin:0;font-family:sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let a = webfont_run_width(with_face, &base, &LocalLoader, "Reading");
        let b = webfont_run_width(plain, &base, &LocalLoader, "Reading");
        assert!(a > 0.0 && b > 0.0);
        assert_ne!(a, b, "loaded @font-face face drove layout (a={a} vs sans b={b})");
        // Glyph pixels are drawn.
        let pm = render_webfont(with_face, &base, &LocalLoader);
        assert!(any_pixel(&pm, |r, g, b| (r, g, b) != (255, 255, 255)), "glyph pixels present");
    }

    #[test]
    fn font_face_data_url_loads() {
        // @font-face via a base64 data: URL ttf, rendered through RouterLoader.
        let b64 = webfont_base64(WEBFONT_TTF);
        let with_face = format!(
            "<html><head><style>\
             @font-face{{font-family:\"MyFont\";src:url(\"data:font/ttf;base64,{b64}\")}} \
             body{{margin:0}} p{{margin:0;font-family:MyFont;font-size:20px}}\
             </style></head><body><p>Reading</p></body></html>"
        );
        let plain = "<html><head><style>\
            body{margin:0} p{margin:0;font-family:sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let base = data_base();
        let a = webfont_run_width(&with_face, &base, &RouterLoader::new(), "Reading");
        let b = webfont_run_width(plain, &base, &RouterLoader::new(), "Reading");
        assert_ne!(a, b, "data: @font-face loaded and drove layout");
    }

    #[test]
    fn font_face_weight_selects_bold() {
        // Two @font-face rules for one family (weight 400 + 700, distinct
        // metrics). A weight:700 paragraph measures with the bold face.
        let dir = e3_dir();
        std::fs::write(dir.join("reg.ttf"), WEBFONT_TTF).unwrap();
        std::fs::write(dir.join("bold.ttf"), WEBFONT_BOLD_TTF).unwrap();
        let base = base_in(&dir);
        let style = "@font-face{font-family:\"MyFont\";src:url(\"reg.ttf\");font-weight:400} \
            @font-face{font-family:\"MyFont\";src:url(\"bold.ttf\");font-weight:700} \
            body{margin:0} p{margin:0;font-family:MyFont;font-size:20px}";
        let reg_html = format!("<html><head><style>{style}</style></head>\
            <body><p style='font-weight:400'>Reading</p></body></html>");
        let bold_html = format!("<html><head><style>{style}</style></head>\
            <body><p style='font-weight:700'>Reading</p></body></html>");
        let reg = webfont_run_width(&reg_html, &base, &LocalLoader, "Reading");
        let bold = webfont_run_width(&bold_html, &base, &LocalLoader, "Reading");
        assert_ne!(reg, bold, "weight 700 picks the distinct bold @font-face (reg={reg} bold={bold})");
    }

    #[test]
    fn font_face_failed_src_falls_back_no_panic() {
        // A missing src file → the rule registers nothing; the family falls back
        // to the next family (sans-serif). No panic; glyph pixels still drawn.
        let dir = e3_dir();
        let base = base_in(&dir);
        let html = "<html><head><style>\
            @font-face{font-family:\"MyFont\";src:url(\"nope.ttf\")} \
            body{margin:0} p{margin:0;font-family:MyFont,sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let with = webfont_run_width(html, &base, &LocalLoader, "Reading");
        let sans = "<html><head><style>\
            body{margin:0} p{margin:0;font-family:sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let fallback = webfont_run_width(sans, &base, &LocalLoader, "Reading");
        assert_eq!(with, fallback, "failed @font-face → sans-serif fallback");
        let pm = render_webfont(html, &base, &LocalLoader);
        assert!(any_pixel(&pm, |r, g, b| (r, g, b) != (255, 255, 255)), "glyph pixels present");
    }

    #[test]
    fn font_face_unsupported_format_skipped() {
        // A woff2-only src is skipped (fontdue can't read it) → family falls back;
        // no panic.
        let dir = e3_dir();
        std::fs::write(dir.join("x.woff2"), WEBFONT_TTF).unwrap(); // bytes irrelevant
        let base = base_in(&dir);
        let html = "<html><head><style>\
            @font-face{font-family:\"MyFont\";src:url(\"x.woff2\") format(\"woff2\")} \
            body{margin:0} p{margin:0;font-family:MyFont,sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let with = webfont_run_width(html, &base, &LocalLoader, "Reading");
        let sans = "<html><head><style>\
            body{margin:0} p{margin:0;font-family:sans-serif;font-size:20px}\
            </style></head><body><p>Reading</p></body></html>";
        let fallback = webfont_run_width(sans, &base, &LocalLoader, "Reading");
        assert_eq!(with, fallback, "woff2 src skipped → sans-serif fallback");
    }

    #[test]
    fn unused_font_face_does_not_change_other_text() {
        // NO-REGRESSION: an @font-face whose family no element uses must not
        // perturb the rest of the page — a default-font paragraph renders
        // byte-identically with and without the (loaded but unused) face.
        let dir = e3_dir();
        std::fs::write(dir.join("myfont.ttf"), WEBFONT_TTF).unwrap();
        let base = base_in(&dir);
        let plain = "<html><head><style>\
            body{margin:0;color:#000000} p{margin:0;font-size:20px}\
            </style></head><body><p>Hello world</p></body></html>";
        let with_unused = "<html><head><style>\
            @font-face{font-family:\"Unused\";src:url(\"myfont.ttf\")} \
            body{margin:0;color:#000000} p{margin:0;font-size:20px}\
            </style></head><body><p>Hello world</p></body></html>";
        let a = render_webfont(plain, &base, &LocalLoader);
        let b = render_webfont(with_unused, &base, &LocalLoader);
        assert_eq!(a.data(), b.data(), "unused @font-face must not change page pixels");
    }
}
