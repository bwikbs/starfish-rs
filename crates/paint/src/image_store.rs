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

/// Cache of decoded images keyed by **resolved URL**. Each `src` is decoded at
/// most once; both layout (intrinsic size) and paint (pixels) read from here. A
/// failed resolve / fetch / decode is cached as `None` so we don't retry.
pub struct ImageStore<'a> {
    base: Url,
    loader: &'a dyn ResourceLoader,
    cache: HashMap<Url, Option<DecodedImage>>,
}

impl<'a> ImageStore<'a> {
    /// Create a store resolving relative `src` against `base` (the document's
    /// URL) and fetching through `loader`.
    pub fn new(base: Url, loader: &'a dyn ResourceLoader) -> ImageStore<'a> {
        ImageStore { base, loader, cache: HashMap::new() }
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
}

impl starfish_layout::ImageSource for ImageStore<'_> {
    fn intrinsic_size(&self, src: &str) -> Option<(f32, f32)> {
        self.peek(src).map(|d| (d.width as f32, d.height as f32))
    }
}

/// Fetch the bytes via the loader and decode to RGBA8; `None` on any fetch or
/// decode error (never panics).
fn fetch_and_decode(loader: &dyn ResourceLoader, url: &Url) -> Option<DecodedImage> {
    let res = loader.fetch(url).ok()?; // NotFound/Io/Unsupported → broken image
    let img = image::load_from_memory(&res.bytes).ok()?; // sniffs PNG/JPEG
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedImage { width, height, rgba: rgba.into_raw() })
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
        let dir =
            std::env::temp_dir().join(format!("starfish-img-{}-{n}", std::process::id()));
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
}
