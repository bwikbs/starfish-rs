//! Image loading + cache (E3-M1 §6). Resolves `<img src>` as a URL against the
//! document base and fetches the bytes through a `ResourceLoader`, decoding
//! PNG/JPEG to RGBA8 once per resolved URL; both layout (intrinsic size) and
//! paint (pixels) read from the same store. Never panics — a missing or
//! undecodable image is cached as `None` (a broken image).

use std::collections::HashMap;

use starfish_net::{ResourceLoader, Url};

/// One decoded image: tightly-packed RGBA8, row-major, `width*height*4` bytes.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A parsed SVG file referenced by `<img src=*.svg>` (E15-M3): its own DOM, the
/// `<svg>` root within it, and its intrinsic content size. The paint pass drives
/// the existing inline-SVG painter over `doc`/`svg_id` into the `<img>` box.
pub struct ParsedSvg {
    pub doc: starfish_dom::Document,
    pub svg_id: starfish_dom::NodeId,
    pub intrinsic: (f32, f32),
}

/// Cache of decoded images keyed by **resolved URL**. Each `src` is decoded at
/// most once; both layout (intrinsic size) and paint (pixels) read from here. A
/// failed resolve / fetch / decode is cached as `None` so we don't retry.
pub struct ImageStore<'a> {
    base: Url,
    loader: &'a dyn ResourceLoader,
    cache: HashMap<Url, Option<DecodedImage>>,
    /// E15-M3: parsed `<img src=*.svg>` files, keyed by resolved URL (the raster
    /// `cache` never gets an entry for these — they render via the SVG painter).
    svgs: HashMap<Url, Option<ParsedSvg>>,
}

impl<'a> ImageStore<'a> {
    /// Create a store resolving relative `src` against `base` (the document's
    /// URL) and fetching through `loader`.
    pub fn new(base: Url, loader: &'a dyn ResourceLoader) -> ImageStore<'a> {
        ImageStore {
            base,
            loader,
            cache: HashMap::new(),
            svgs: HashMap::new(),
        }
    }

    /// Resolve a raw `src` attribute to the absolute `Url` we key on. `None` for
    /// an unresolvable reference (cached as a broken image by the caller).
    fn resolve(&self, src: &str) -> Option<Url> {
        self.base.join(src).ok()
    }

    /// Load + decode (or fetch from cache) the image at `src`. Returns `None`
    /// for an unresolvable / missing / undecodable image (cached so repeats are
    /// cheap). Never panics.
    pub fn get(&mut self, src: &str) -> Option<&DecodedImage> {
        let key = self.resolve(src)?;
        let loader = self.loader;
        // E15-M3: `*.svg` references parse into the SVG cache, never the raster
        // cache (`image::load_from_memory` can't decode SVG). They render via the
        // SVG painter, so `get`/`peek` return `None` for the raster path.
        if is_svg_url(&key) {
            self.svgs
                .entry(key.clone())
                .or_insert_with(|| fetch_and_parse_svg(loader, &key));
            return None;
        }
        self.cache
            .entry(key.clone())
            .or_insert_with(|| fetch_and_decode(loader, &key));
        self.cache.get(&key).and_then(|o| o.as_ref())
    }

    /// Read-only lookup used by paint after layout/pre-pass populated the cache.
    /// Falls back to `None` (broken image) if absent or unresolvable.
    pub fn peek(&self, src: &str) -> Option<&DecodedImage> {
        self.resolve(src)
            .and_then(|k| self.cache.get(&k))
            .and_then(|o| o.as_ref())
    }

    /// Read-only lookup of a parsed `<img src=*.svg>` (E15-M3), populated by the
    /// decode pre-pass via `get`. `None` if absent / unresolvable / unparsable.
    pub fn peek_svg(&self, src: &str) -> Option<&ParsedSvg> {
        self.resolve(src)
            .and_then(|k| self.svgs.get(&k))
            .and_then(|o| o.as_ref())
    }
}

impl starfish_layout::ImageSource for ImageStore<'_> {
    fn intrinsic_size(&self, src: &str) -> Option<(f32, f32)> {
        // An SVG `<img>` sizes from the parsed SVG's intrinsic size (E15-M3); a
        // raster `<img>` from its decoded pixel dimensions.
        if let Some(p) = self.peek_svg(src) {
            return Some(p.intrinsic);
        }
        self.peek(src).map(|d| (d.width as f32, d.height as f32))
    }
}

/// True if `url`'s last path segment ends with `.svg` (case-insensitive).
fn is_svg_url(url: &Url) -> bool {
    url.path()
        .rsplit('/')
        .next()
        .map(|seg| seg.to_ascii_lowercase().ends_with(".svg"))
        .unwrap_or(false)
}

/// Fetch the bytes via the loader and decode to RGBA8; `None` on any fetch or
/// decode error (never panics).
fn fetch_and_decode(loader: &dyn ResourceLoader, url: &Url) -> Option<DecodedImage> {
    let res = loader.fetch(url).ok()?; // NotFound/Io/Unsupported → broken image
    let img = image::load_from_memory(&res.bytes).ok()?; // sniffs PNG/JPEG
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Fetch + parse an `<img src=*.svg>` file into a `ParsedSvg` (E15-M3): fetch the
/// bytes, parse them as HTML (the lenient parser keeps `<svg>` attrs+children),
/// find the first `<svg>` element, and compute its intrinsic size. `None` on any
/// fetch error or if the file has no `<svg>` root (never panics).
fn fetch_and_parse_svg(loader: &dyn ResourceLoader, url: &Url) -> Option<ParsedSvg> {
    let res = loader.fetch(url).ok()?;
    let text = String::from_utf8_lossy(&res.bytes);
    let doc = starfish_html::parse(&text);
    let svg_id = find_svg_root(&doc)?;
    let intrinsic = svg_intrinsic(&doc, svg_id);
    Some(ParsedSvg {
        doc,
        svg_id,
        intrinsic,
    })
}

/// DFS for the first element whose tag is `svg` (E15-M3).
fn find_svg_root(doc: &starfish_dom::Document) -> Option<starfish_dom::NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("svg") {
            return Some(id);
        }
        // Push children in reverse so the leftmost child is visited first.
        let kids = doc.children(id);
        for c in kids.into_iter().rev() {
            stack.push(c);
        }
    }
    None
}

/// The `<svg>` intrinsic content size, mirroring layout's `svg_intrinsic_size`
/// (E9-M1 §3.2): `width`/`height` attrs (px) win; a missing dimension derives
/// from the `viewBox` aspect ratio; with neither, the CSS replaced default
/// 300×150 applies.
fn svg_intrinsic(doc: &starfish_dom::Document, id: starfish_dom::NodeId) -> (f32, f32) {
    let w = svg_attr_px(doc, id, "width");
    let h = svg_attr_px(doc, id, "height");
    let vb = starfish_layout::parse_view_box(doc.get_attribute(id, "viewBox"));
    match (w, h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, vb.map(|v| w * v.h / v.w).unwrap_or(150.0)),
        (None, Some(h)) => (vb.map(|v| h * v.w / v.h).unwrap_or(300.0), h),
        (None, None) => vb.map(|v| (v.w, v.h)).unwrap_or((300.0, 150.0)),
    }
}

/// Parse a non-negative px `width`/`height` attribute (`"100"` / `"100px"`).
fn svg_attr_px(doc: &starfish_dom::Document, id: starfish_dom::NodeId, name: &str) -> Option<f32> {
    doc.get_attribute(id, name)
        .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_net::{file_url_from_path, LocalLoader};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temp subdir for this test process (no external dep).
    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-img-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A `file://` base URL for a document living directly in `dir`.
    fn base_for(dir: &Path) -> Url {
        file_url_from_path(&dir.join("index.html")).unwrap()
    }

    /// Write a 2×2 RGBA PNG (TL red, TR green, BL blue, BR white). Deterministic.
    fn write_2x2_png(dir: &Path) -> PathBuf {
        use image::{Rgba, RgbaImage};
        let mut img = RgbaImage::new(2, 2);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, Rgba([255, 255, 255, 255]));
        let path = dir.join("px.png");
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn decodes_2x2_png() {
        let dir = temp_dir();
        write_2x2_png(&dir);
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        let img = store.get("px.png").expect("decoded");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.rgba.len(), 16);
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]); // pixel (0,0) red
    }

    #[test]
    fn missing_file_is_none_and_cached() {
        let dir = temp_dir();
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        assert!(store.get("nope.png").is_none());
        // called twice → still None (cached, no panic).
        assert!(store.get("nope.png").is_none());
    }

    #[test]
    fn non_image_file_is_none() {
        let dir = temp_dir();
        std::fs::write(dir.join("notimg.png"), b"not a png at all").unwrap();
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        assert!(store.get("notimg.png").is_none());
    }

    #[test]
    fn resolve_relative_and_subdir() {
        let dir = temp_dir();
        write_2x2_png(&dir);
        let sub = dir.join("img");
        std::fs::create_dir_all(&sub).unwrap();
        write_2x2_png(&sub);
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        // relative joins base
        assert!(store.get("px.png").is_some());
        // multi-segment relative ref joins too
        assert!(store.get("img/px.png").is_some());
    }

    #[test]
    fn intrinsic_size_via_trait() {
        use starfish_layout::ImageSource;
        let dir = temp_dir();
        write_2x2_png(&dir);
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        store.get("px.png"); // populate
        assert_eq!(store.intrinsic_size("px.png"), Some((2.0, 2.0)));
        assert_eq!(store.intrinsic_size("missing.png"), None);
    }

    // E15-M1: gif / bmp / webp decode (the `image` features are now enabled).
    // We round-trip via `RgbaImage::save(<ext>)` (the encoder ships with each
    // feature) then decode through the store and check intrinsic size + a pixel.

    /// Write a 3×2 RGBA image (TL red) at `dir/<name>`; the extension picks the
    /// encoder.
    fn write_3x2(dir: &Path, name: &str) {
        use image::{Rgba, RgbaImage};
        let mut img = RgbaImage::new(3, 2);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.save(dir.join(name)).unwrap();
    }

    #[test]
    fn decodes_gif_first_frame() {
        let dir = temp_dir();
        write_3x2(&dir, "g.gif");
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        let img = store.get("g.gif").expect("decoded gif");
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.rgba.len(), 3 * 2 * 4);
        // GIF is palettized; the TL pixel is fully opaque red.
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn decodes_bmp() {
        let dir = temp_dir();
        write_3x2(&dir, "b.bmp");
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        let img = store.get("b.bmp").expect("decoded bmp");
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn decodes_webp() {
        let dir = temp_dir();
        write_3x2(&dir, "w.webp");
        let mut store = ImageStore::new(base_for(&dir), &LocalLoader);
        let img = store.get("w.webp").expect("decoded webp");
        assert_eq!((img.width, img.height), (3, 2));
        // Lossless webp preserves the exact red TL pixel.
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    // --- E15-M3: <img src=*.svg> parse + intrinsic size ---

    /// Write `svg` markup to `dir/name` and return a store whose pre-pass `get`
    /// has parsed it (mirroring decode_images).
    fn parsed_svg_store<'a>(dir: &Path, name: &str, svg: &str) -> ImageStore<'a> {
        std::fs::write(dir.join(name), svg).unwrap();
        let mut store = ImageStore::new(base_for(dir), &LocalLoader);
        store.get(name); // raster path returns None; SVG path parses + caches.
        store
    }

    #[test]
    fn svg_img_parses_and_peek_svg_some() {
        use starfish_layout::ImageSource;
        let dir = temp_dir();
        let store = parsed_svg_store(
            &dir,
            "c.svg",
            "<svg viewBox='0 0 40 30'><circle cx='20' cy='15' r='10' fill='red'/></svg>",
        );
        let p = store.peek_svg("c.svg").expect("parsed svg");
        // Intrinsic comes from the viewBox when no width/height attr.
        assert_eq!(p.intrinsic, (40.0, 30.0));
        // The trait routes SVG `<img>` sizing to the parsed intrinsic, not raster.
        assert_eq!(store.intrinsic_size("c.svg"), Some((40.0, 30.0)));
        // The raster cache never gets an entry for an SVG ref.
        assert!(store.peek("c.svg").is_none());
    }

    #[test]
    fn svg_img_intrinsic_width_only_aspect_from_viewbox() {
        let dir = temp_dir();
        let store = parsed_svg_store(&dir, "w.svg", "<svg width='80' viewBox='0 0 40 30'></svg>");
        // width=80, no height → height derives from viewBox aspect (40:30) → 60.
        assert_eq!(store.peek_svg("w.svg").unwrap().intrinsic, (80.0, 60.0));
    }

    #[test]
    fn svg_img_intrinsic_default_300x150() {
        let dir = temp_dir();
        let store = parsed_svg_store(&dir, "n.svg", "<svg><rect/></svg>");
        // No width/height/viewBox → CSS replaced default 300×150.
        assert_eq!(store.peek_svg("n.svg").unwrap().intrinsic, (300.0, 150.0));
    }
}
