//! Image loading + cache (E2-M4 §2). Decodes local PNG/JPEG files referenced by
//! `<img src>` to RGBA8 once per resolved path; both layout (intrinsic size) and
//! paint (pixels) read from the same store. Never panics — a missing or
//! undecodable file is cached as `None` (a broken image).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One decoded image: tightly-packed RGBA8, row-major, `width*height*4` bytes.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Cache of decoded images keyed by **resolved path**. Each `src` is decoded at
/// most once; both layout (intrinsic size) and paint (pixels) read from here. A
/// failed decode is cached as `None` so we don't retry.
#[derive(Default)]
pub struct ImageStore {
    base: PathBuf,
    cache: HashMap<PathBuf, Option<DecodedImage>>,
}

impl ImageStore {
    /// Create a store resolving relative `src` against `base` (the input HTML
    /// file's directory).
    pub fn new(base: impl Into<PathBuf>) -> ImageStore {
        ImageStore { base: base.into(), cache: HashMap::new() }
    }

    /// Resolve a raw `src` attribute to the path we key on. Absolute `src` is
    /// used as-is; relative `src` joins `self.base`. (No URL parsing, no
    /// `file://`, no `data:` — §8.)
    fn resolve(&self, src: &str) -> PathBuf {
        let p = Path::new(src);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        }
    }

    /// Load + decode (or fetch from cache) the image at `src`. Returns `None` for
    /// a missing / undecodable file (cached so repeats are cheap). Never panics.
    pub fn get(&mut self, src: &str) -> Option<&DecodedImage> {
        let key = self.resolve(src);
        self.cache.entry(key.clone()).or_insert_with(|| decode(&key));
        self.cache.get(&key).and_then(|o| o.as_ref())
    }

    /// Read-only lookup used by paint after layout/pre-pass populated the cache.
    /// Falls back to `None` (broken image) if absent.
    pub fn peek(&self, src: &str) -> Option<&DecodedImage> {
        self.cache.get(&self.resolve(src)).and_then(|o| o.as_ref())
    }
}

impl starfish_layout::ImageSource for ImageStore {
    fn intrinsic_size(&self, src: &str) -> Option<(f32, f32)> {
        self.peek(src).map(|d| (d.width as f32, d.height as f32))
    }
}

/// Decode one file to RGBA8; `None` on any I/O or decode error (never panics).
fn decode(path: &Path) -> Option<DecodedImage> {
    let img = image::open(path).ok()?; // sniffs PNG/JPEG by magic bytes
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedImage { width, height, rgba: rgba.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temp subdir for this test process (no external dep).
    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-img-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let mut store = ImageStore::new(&dir);
        let img = store.get("px.png").expect("decoded");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.rgba.len(), 16);
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]); // pixel (0,0) red
    }

    #[test]
    fn missing_file_is_none_and_cached() {
        let dir = temp_dir();
        let mut store = ImageStore::new(&dir);
        assert!(store.get("nope.png").is_none());
        // called twice → still None (cached, no panic).
        assert!(store.get("nope.png").is_none());
    }

    #[test]
    fn non_image_file_is_none() {
        let dir = temp_dir();
        let path = dir.join("notimg.png");
        std::fs::write(&path, b"not a png at all").unwrap();
        let mut store = ImageStore::new(&dir);
        assert!(store.get("notimg.png").is_none());
    }

    #[test]
    fn resolve_relative_and_absolute() {
        let dir = temp_dir();
        let abs = write_2x2_png(&dir);
        let mut store = ImageStore::new(&dir);
        // relative joins base
        assert!(store.get("px.png").is_some());
        // absolute path passes through
        assert!(store.get(abs.to_str().unwrap()).is_some());
    }

    #[test]
    fn intrinsic_size_via_trait() {
        use starfish_layout::ImageSource;
        let dir = temp_dir();
        write_2x2_png(&dir);
        let mut store = ImageStore::new(&dir);
        store.get("px.png"); // populate
        assert_eq!(store.intrinsic_size("px.png"), Some((2.0, 2.0)));
        assert_eq!(store.intrinsic_size("missing.png"), None);
    }
}
