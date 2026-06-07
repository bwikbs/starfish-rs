//! Font matching + per-face metrics + rasterization (E6-M1). A `fontdb::Database`
//! does the CSS font matching (family-list walk, generic mapping, weight/style
//! ladder); parsed `fontdue` faces are cached by the resolved `fontdb::ID` and
//! back both layout's `TextMeasurer` (so measuring matches painting) and the
//! glyph blitting in `raster.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fontdb::{
    Database, FaceInfo, Family, Language, Query, Source, Stretch, Style as FdbStyle,
    Weight as FdbWeight, ID,
};
use fontdue::{Font as FontdueFont, FontSettings};
use starfish_css::FontFaceStyle;
use starfish_layout::{FontQuery, FontStyle, LineMetrics, TextMeasurer};

// Vendored faces, embedded for determinism (no I/O for these). DejaVu covers
// sans/serif/mono; Liberation Serif provides a real italic.
const DEJAVU_SANS: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const DEJAVU_SANS_BOLD: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");
const DEJAVU_SERIF: &[u8] = include_bytes!("../assets/DejaVuSerif.ttf");
const DEJAVU_SERIF_BOLD: &[u8] = include_bytes!("../assets/DejaVuSerif-Bold.ttf");
const DEJAVU_MONO: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const DEJAVU_MONO_BOLD: &[u8] = include_bytes!("../assets/DejaVuSansMono-Bold.ttf");
const LIB_SERIF: &[u8] = include_bytes!("../assets/LiberationSerif-Regular.ttf");
const LIB_SERIF_IT: &[u8] = include_bytes!("../assets/LiberationSerif-Italic.ttf");
const LIB_SERIF_BD: &[u8] = include_bytes!("../assets/LiberationSerif-Bold.ttf");
const LIB_SERIF_BDIT: &[u8] = include_bytes!("../assets/LiberationSerif-BoldItalic.ttf");

/// Upper bound on the font size (px) handed to fontdue. Sizes are clamped to
/// this to bound rasterization cost; far above any plausible real glyph size.
const MAX_FONT_PX: f32 = 4096.0;

/// Sanitize a font size before passing it to fontdue. Non-finite (inf/NaN) or
/// non-positive sizes would make fontdue panic, so they collapse to 0 (which
/// fontdue handles gracefully, yielding empty metrics/raster). Finite sizes are
/// clamped to `MAX_FONT_PX`; normal sizes pass through unchanged.
fn sane_size(px: f32) -> f32 {
    if px.is_finite() && px > 0.0 {
        px.min(MAX_FONT_PX)
    } else {
        0.0
    }
}

/// Whether a CSS family name is a generic keyword (matched case-insensitively),
/// which maps to a `Family` variant rather than a `Family::Name`.
fn is_generic(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "serif" | "sans-serif" | "monospace" | "cursive" | "fantasy"
    )
}

/// One rasterized glyph: an 8-bit coverage mask + its placement relative to the
/// pen origin (baseline), in device pixels.
pub struct GlyphBitmap {
    pub width: usize,
    pub height: usize,
    /// `metrics.xmin` — left side bearing (+x = right).
    pub left: i32,
    /// Distance from the baseline up to the mask's top (`ymin + height`).
    pub top: i32,
    pub advance: f32,
    /// `width * height` coverage values, 0..=255.
    pub coverage: Vec<u8>,
}

/// The font database: a `fontdb::Database` for matching + a lazily-populated
/// cache of parsed fontdue faces keyed by the resolved fontdb face id.
pub struct FontDb {
    db: Database,
    /// Resolved-face cache. RefCell because resolution happens behind the shared
    /// `&FontDb` that layout + paint both hold; interior mutability keeps the
    /// public API `&self`. Single-threaded render, so RefCell is enough.
    faces: RefCell<HashMap<ID, Rc<FontdueFont>>>,
    /// The id of the vendored DejaVu Sans regular — the guaranteed default when
    /// a query yields None or a face fails to parse.
    default_id: ID,
}

impl FontDb {
    /// Runtime DB: system fonts + the vendored fallback faces, generics mapped
    /// to vendored families.
    pub fn new() -> FontDb {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::finish(db)
    }

    /// Deterministic DB for tests/golden: ONLY the vendored faces.
    pub fn vendored_only() -> FontDb {
        Self::finish(Database::new())
    }

    /// BACK-COMPAT shim so existing call sites (`FontDb::load()`) keep compiling.
    /// Infallible: embedded assets are validated at build time, and fontdb
    /// tolerates system-font load failures.
    pub fn load() -> Result<FontDb, String> {
        Ok(FontDb::new())
    }

    fn finish(mut db: Database) -> FontDb {
        // Always register the vendored faces (load_font_data owns the Vec).
        for bytes in [
            DEJAVU_SANS,
            DEJAVU_SANS_BOLD,
            DEJAVU_SERIF,
            DEJAVU_SERIF_BOLD,
            DEJAVU_MONO,
            DEJAVU_MONO_BOLD,
            LIB_SERIF,
            LIB_SERIF_IT,
            LIB_SERIF_BD,
            LIB_SERIF_BDIT,
        ] {
            db.load_font_data(bytes.to_vec());
        }
        // Map generics deterministically to vendored families.
        db.set_sans_serif_family("DejaVu Sans");
        db.set_serif_family("DejaVu Serif");
        db.set_monospace_family("DejaVu Sans Mono");
        db.set_cursive_family("Liberation Serif");
        db.set_fantasy_family("DejaVu Sans");
        // The hard default: query for our sans regular; it is guaranteed present.
        let default_id = db
            .query(&Query {
                families: &[Family::Name("DejaVu Sans")],
                weight: FdbWeight::NORMAL,
                stretch: Default::default(),
                style: FdbStyle::Normal,
            })
            .or_else(|| db.faces().next().map(|f| f.id))
            .expect("vendored DejaVu Sans always present");
        FontDb { db, faces: RefCell::new(HashMap::new()), default_id }
    }

    /// Look up a `local(<name>)` face in the existing db (system + vendored) and
    /// return a copy of its raw bytes, or `None` on a miss. Used to resolve
    /// `@font-face` `local()` sources before registering under the author name.
    pub fn system_face_bytes(&self, name: &str) -> Option<Vec<u8>> {
        let id = self.db.query(&Query {
            families: &[Family::Name(name)],
            weight: FdbWeight::NORMAL,
            stretch: Default::default(),
            style: FdbStyle::Normal,
        })?;
        self.db.with_face_data(id, |bytes, _index| bytes.to_vec())
    }

    /// Register an `@font-face` (E6-M2): validate `bytes` with fontdue, then add
    /// a face to the fontdb under the author `family` name + weight/style, so a
    /// `font-family: <family>` query resolves to it via the existing matcher
    /// (measure == paint preserved). Returns `false` (and registers nothing) if
    /// the bytes don't parse as a font, so the loader can try the next `src`.
    pub fn add_face(
        &mut self,
        family: &str,
        weight: Option<u16>,
        style: Option<FontFaceStyle>,
        bytes: Vec<u8>,
    ) -> bool {
        // Validate up front so a bad face never shadows a good fallback.
        if FontdueFont::from_bytes(
            bytes.as_slice(),
            FontSettings { collection_index: 0, ..Default::default() },
        )
        .is_err()
        {
            return false;
        }
        let fdb_style = match style {
            Some(FontFaceStyle::Italic) => FdbStyle::Italic,
            Some(FontFaceStyle::Oblique) => FdbStyle::Oblique,
            _ => FdbStyle::Normal,
        };
        // CSS family names match case-insensitively (ASCII). fontdb compares
        // family strings byte-exact, so index the @font-face under a normalized
        // (trimmed + ASCII-lowercased) key; `resolve_id` queries that same key.
        let family = family.trim().to_ascii_lowercase();
        self.db.push_face_info(FaceInfo {
            id: ID::dummy(),
            source: Source::Binary(std::sync::Arc::new(bytes)),
            index: 0,
            families: vec![(family.clone(), Language::English_UnitedStates)],
            post_script_name: family,
            style: fdb_style,
            weight: FdbWeight(weight.unwrap_or(400)),
            stretch: Stretch::Normal,
            monospaced: false,
        });
        true
    }

    /// Resolve the matched face id for a query, delegating the CSS match to
    /// fontdb. Falls back to the vendored default on a miss.
    fn resolve_id(&self, q: &FontQuery) -> ID {
        // Lowercased copies of the requested NAME families, owned so the
        // borrowed `Family::Name` entries below can reference them. Generics map
        // to their own Family variant and need no lowercased copy.
        let lowered: Vec<String> = q
            .family
            .iter()
            .filter(|name| !is_generic(name))
            .map(|name| name.to_ascii_lowercase())
            .collect();
        // Build the borrowed Family list: per requested family, a generic keyword
        // maps to its Family variant (so fontdb's set_*_family applies); a real
        // name pushes BOTH the original (exact-case system-font match) AND the
        // lowercased variant (matches @font-face faces indexed by normalized key).
        let mut lowered_iter = lowered.iter();
        let mut families: Vec<Family> = Vec::with_capacity(q.family.len() * 2);
        for name in q.family {
            match name.to_ascii_lowercase().as_str() {
                "serif" => families.push(Family::Serif),
                "sans-serif" => families.push(Family::SansSerif),
                "monospace" => families.push(Family::Monospace),
                "cursive" => families.push(Family::Cursive),
                "fantasy" => families.push(Family::Fantasy),
                _ => {
                    families.push(Family::Name(name.as_str()));
                    // Paired lowercased copy (built in the same name order).
                    let low = lowered_iter.next().expect("one lowered per name family");
                    if low.as_str() != name.as_str() {
                        families.push(Family::Name(low.as_str()));
                    }
                }
            }
        }
        // Empty list (no font-family) → UA default sans.
        let fallback = [Family::SansSerif];
        let fam_slice = if families.is_empty() { &fallback[..] } else { &families[..] };

        self.db
            .query(&Query {
                families: fam_slice,
                weight: FdbWeight(q.weight.0),
                stretch: Default::default(),
                style: match q.style {
                    FontStyle::Normal => FdbStyle::Normal,
                    FontStyle::Italic => FdbStyle::Italic,
                    FontStyle::Oblique => FdbStyle::Oblique,
                },
            })
            .unwrap_or(self.default_id)
    }

    /// Resolve a font query to a parsed fontdue face (always usable).
    fn resolve(&self, q: &FontQuery) -> Rc<FontdueFont> {
        self.face_by_id(self.resolve_id(q))
    }

    /// Parse-and-cache the fontdue face for a resolved id. On a parse failure or
    /// a missing-data id, fall back to the default face (and cache that).
    fn face_by_id(&self, id: ID) -> Rc<FontdueFont> {
        if let Some(f) = self.faces.borrow().get(&id) {
            return f.clone();
        }
        let parsed = self
            .db
            .with_face_data(id, |bytes, index| {
                FontdueFont::from_bytes(
                    bytes,
                    FontSettings { collection_index: index, ..Default::default() },
                )
                .ok()
            })
            .flatten();
        let face = match parsed {
            Some(f) => Rc::new(f),
            None if id != self.default_id => return self.face_by_id(self.default_id),
            None => panic!("default face must parse"), // unreachable: vendored asset
        };
        self.faces.borrow_mut().insert(id, face.clone());
        face
    }

    /// Sum of per-glyph advance widths (no kerning) for the resolved face at
    /// `q.size` px. Missing glyphs fall back to fontdue's `.notdef` advance.
    pub fn advance_width(&self, text: &str, q: &FontQuery) -> f32 {
        let f = self.resolve(q);
        let size = sane_size(q.size);
        let glyphs: f32 = text.chars().map(|c| f.metrics(c, size).advance_width).sum();
        glyphs + starfish_layout::extra_spacing(text, q)
    }

    /// Ascent/descent (both positive, pointing away from the baseline) for one
    /// line of the resolved face, from fontdue's horizontal line metrics.
    pub fn line_metrics(&self, q: &FontQuery) -> LineMetrics {
        let size = sane_size(q.size);
        match self.resolve(q).horizontal_line_metrics(size) {
            Some(lm) => LineMetrics {
                ascent: lm.ascent,
                descent: -lm.descent, // fontdue descent is negative
            },
            None => LineMetrics {
                ascent: size * 0.8,
                descent: size * 0.2,
            },
        }
    }

    /// Rasterize one char with the resolved face. Whitespace yields an empty mask
    /// (width == 0) but a real advance.
    pub fn rasterize_glyph(&self, ch: char, q: &FontQuery) -> GlyphBitmap {
        let (m, coverage) = self.resolve(q).rasterize(ch, sane_size(q.size));
        GlyphBitmap {
            width: m.width,
            height: m.height,
            left: m.xmin,
            // fontdue ymin = offset of the mask bottom from the baseline (up +);
            // the mask top sits ymin + height above the baseline.
            top: m.ymin + m.height as i32,
            advance: m.advance_width,
            coverage,
        }
    }
}

impl Default for FontDb {
    fn default() -> Self {
        FontDb::new()
    }
}

/// Adapts a `FontDb` (by ref) to layout's `TextMeasurer`, so line-breaking
/// during layout uses the same advances the painter draws.
pub struct FontMeasurer<'a>(pub &'a FontDb);

impl TextMeasurer for FontMeasurer<'_> {
    fn measure(&self, text: &str, font: &FontQuery) -> f32 {
        self.0.advance_width(text, font)
    }
    fn line_metrics(&self, font: &FontQuery) -> LineMetrics {
        self.0.line_metrics(font)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_style::FontWeight;

    fn db() -> FontDb {
        FontDb::vendored_only()
    }

    /// Build a query borrowing a family slice.
    fn q<'a>(family: &'a [String], style: FontStyle, weight: u16, size: f32) -> FontQuery<'a> {
        FontQuery {
            family,
            style,
            weight: FontWeight(weight),
            size,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
    }

    fn fam(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn advance_monotonic_and_empty_zero() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        assert_eq!(f.advance_width("", &q(&sans, FontStyle::Normal, 400, 16.0)), 0.0);
        let short = f.advance_width("Hi", &q(&sans, FontStyle::Normal, 400, 16.0));
        let long = f.advance_width("Hello world", &q(&sans, FontStyle::Normal, 400, 16.0));
        assert!(long > short);
        assert!(short > 0.0);
    }

    #[test]
    fn advance_scales_with_size() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let small = f.advance_width("Mmm", &q(&sans, FontStyle::Normal, 400, 10.0));
        let big = f.advance_width("Mmm", &q(&sans, FontStyle::Normal, 400, 20.0));
        assert!(big > small);
    }

    #[test]
    fn mono_differs_from_sans() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let mono = fam(&["DejaVu Sans Mono"]);
        let a = f.advance_width("Illili", &q(&sans, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("Illili", &q(&mono, FontStyle::Normal, 400, 16.0));
        // proportional sans vs fixed-width mono → different total advances.
        assert_ne!(a, b);
    }

    #[test]
    fn monospace_generic_maps_to_mono_face() {
        let f = db();
        let mono = fam(&["DejaVu Sans Mono"]);
        let generic = fam(&["monospace"]);
        let a = f.advance_width("Hello", &q(&mono, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("Hello", &q(&generic, FontStyle::Normal, 400, 16.0));
        assert_eq!(a, b, "monospace generic resolves to DejaVu Sans Mono");
    }

    #[test]
    fn serif_resolves_to_serif_face() {
        let f = db();
        let serif = fam(&["DejaVu Serif", "serif"]);
        let sans = fam(&["DejaVu Sans"]);
        // distinct faces → distinct glyph advances for the same text/size, and a
        // distinct resolved id.
        let serif_id = f.resolve_id(&q(&serif, FontStyle::Normal, 400, 16.0));
        let sans_id = f.resolve_id(&q(&sans, FontStyle::Normal, 400, 16.0));
        assert_ne!(serif_id, sans_id, "serif resolves to a different face than sans");
        let a = f.advance_width("Reading", &q(&serif, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("Reading", &q(&sans, FontStyle::Normal, 400, 16.0));
        assert_ne!(a, b, "serif advances differ from sans");
        // serif generic resolves to the same DejaVu Serif face as the explicit name.
        let generic = fam(&["serif"]);
        let c = f.advance_width("Reading", &q(&generic, FontStyle::Normal, 400, 16.0));
        assert_eq!(a, c);
    }

    #[test]
    fn italic_resolves_to_distinct_face() {
        let f = db();
        let lib = fam(&["Liberation Serif"]);
        let normal_id = f.resolve_id(&q(&lib, FontStyle::Normal, 400, 16.0));
        let italic_id = f.resolve_id(&q(&lib, FontStyle::Italic, 400, 16.0));
        assert_ne!(normal_id, italic_id, "Liberation Serif italic is a distinct face");
        // DejaVu has no italic → italic falls back to the same regular face.
        let serif = fam(&["DejaVu Serif"]);
        let dn = f.resolve_id(&q(&serif, FontStyle::Normal, 400, 16.0));
        let di = f.resolve_id(&q(&serif, FontStyle::Italic, 400, 16.0));
        assert_eq!(dn, di, "DejaVu Serif has no italic → same face");
    }

    #[test]
    fn weight_700_differs_from_400() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let reg_id = f.resolve_id(&q(&sans, FontStyle::Normal, 400, 16.0));
        let bold_id = f.resolve_id(&q(&sans, FontStyle::Normal, 700, 16.0));
        assert_ne!(reg_id, bold_id, "bold resolves to a different face");
        let reg = f.advance_width("Mmm", &q(&sans, FontStyle::Normal, 400, 16.0));
        let bold = f.advance_width("Mmm", &q(&sans, FontStyle::Normal, 700, 16.0));
        assert!(bold > 0.0);
        assert_ne!(reg, bold);
    }

    #[test]
    fn unknown_family_falls_back_to_sans() {
        let f = db();
        let nonexist = fam(&["No Such Font 12345", "sans-serif"]);
        let sans_generic = fam(&["sans-serif"]);
        // resolves without panic and matches the sans-serif advance.
        let a = f.advance_width("fallback", &q(&nonexist, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("fallback", &q(&sans_generic, FontStyle::Normal, 400, 16.0));
        assert_eq!(a, b);
    }

    #[test]
    fn empty_family_behaves_as_sans() {
        let f = db();
        let empty: Vec<String> = Vec::new();
        let sans_generic = fam(&["sans-serif"]);
        let a = f.advance_width("xyz", &q(&empty, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("xyz", &q(&sans_generic, FontStyle::Normal, 400, 16.0));
        assert_eq!(a, b);
    }

    #[test]
    fn line_metrics_positive_and_sane() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let lm = f.line_metrics(&q(&sans, FontStyle::Normal, 400, 20.0));
        assert!(lm.ascent > 0.0);
        assert!(lm.descent > 0.0);
        let total = lm.ascent + lm.descent;
        assert!(total > 0.9 * 20.0 && total < 1.5 * 20.0, "total={total}");
    }

    #[test]
    fn rasterize_glyph_has_coverage() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let g = f.rasterize_glyph('x', &q(&sans, FontStyle::Normal, 400, 24.0));
        assert!(g.width > 0 && g.height > 0);
        assert!(g.coverage.iter().any(|&c| c > 0));
        assert!(g.advance > 0.0);
    }

    #[test]
    fn rasterize_space_is_empty_but_advances() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let g = f.rasterize_glyph(' ', &q(&sans, FontStyle::Normal, 400, 24.0));
        assert_eq!(g.width, 0);
        assert!(g.advance > 0.0);
    }

    #[test]
    fn pathological_sizes_do_not_panic() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        for size in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 1e9_f32] {
            let query = q(&sans, FontStyle::Normal, 400, size);

            let adv = f.advance_width("Hello", &query);
            assert!(adv.is_finite() && adv >= 0.0, "advance_width({size}) = {adv}");

            let lm = f.line_metrics(&query);
            assert!(
                lm.ascent.is_finite() && lm.ascent >= 0.0,
                "ascent({size}) = {}",
                lm.ascent
            );
            assert!(
                lm.descent.is_finite() && lm.descent >= 0.0,
                "descent({size}) = {}",
                lm.descent
            );

            let g = f.rasterize_glyph('x', &query);
            assert!(g.advance.is_finite() && g.advance >= 0.0, "raster advance({size}) = {}", g.advance);
            assert_eq!(g.width * g.height, g.coverage.len(), "coverage len for size {size}");
        }
    }

    #[test]
    fn normal_size_unchanged_by_clamp() {
        // The sanitizer must not perturb ordinary sizes: advance at 16px is
        // exactly what fontdue reports for 16px.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let query = q(&sans, FontStyle::Normal, 400, 16.0);
        let measured = f.advance_width("Hello, world!", &query);

        let face = f.resolve(&query);
        let expected: f32 = "Hello, world!"
            .chars()
            .map(|c| face.metrics(c, 16.0).advance_width)
            .sum();
        assert_eq!(measured, expected);
    }

    // --- E6-M2: @font-face registration via add_face ---

    #[test]
    fn add_face_registers_and_resolves() {
        // Register Liberation Serif bytes under a novel author family, then a
        // query for that family resolves to it — advances differ from the
        // vendored default sans for the same string (a distinct face drove it).
        let mut f = db();
        let myfont = fam(&["MyFont"]);
        let sans = fam(&["MyFont", "sans-serif"]);
        // Before registering, "MyFont" is unknown → falls back to sans default.
        let before = f.advance_width("Reading", &q(&sans, FontStyle::Normal, 400, 16.0));
        assert!(f.add_face("MyFont", None, None, LIB_SERIF.to_vec()));
        let after = f.advance_width("Reading", &q(&myfont, FontStyle::Normal, 400, 16.0));
        let sans_only = f.advance_width("Reading", &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0));
        assert_eq!(before, sans_only, "pre-registration MyFont == sans fallback");
        assert_ne!(after, sans_only, "registered MyFont resolves to the loaded face");
    }

    #[test]
    fn add_face_resolves_case_insensitively() {
        // CSS family names match case-insensitively (ASCII). A @font-face
        // registered as "MyFont" must resolve for any casing of the query name,
        // and all to the SAME loaded face (distinct from the sans fallback).
        let mut f = db();
        assert!(f.add_face("MyFont", None, None, LIB_SERIF.to_vec()));
        let sans_only = f.advance_width("Reading", &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0));
        let exact = f.advance_width("Reading", &q(&fam(&["MyFont"]), FontStyle::Normal, 400, 16.0));
        let lower = f.advance_width("Reading", &q(&fam(&["myfont"]), FontStyle::Normal, 400, 16.0));
        let upper = f.advance_width("Reading", &q(&fam(&["MYFONT"]), FontStyle::Normal, 400, 16.0));
        assert_ne!(exact, sans_only, "registered MyFont resolves to the loaded face");
        assert_eq!(exact, lower, "lowercase myfont resolves to the same face");
        assert_eq!(exact, upper, "uppercase MYFONT resolves to the same face");
    }

    #[test]
    fn system_family_still_resolves_with_doubled_query() {
        // The doubled (original + lowercased) query list must not regress a
        // normal vendored/system family: "DejaVu Sans" still resolves to the
        // sans face, distinct from serif.
        let f = db();
        let sans_id = f.resolve_id(&q(&fam(&["DejaVu Sans"]), FontStyle::Normal, 400, 16.0));
        let serif_id = f.resolve_id(&q(&fam(&["DejaVu Serif"]), FontStyle::Normal, 400, 16.0));
        assert_ne!(sans_id, serif_id, "DejaVu Sans still resolves distinctly from serif");
        let a = f.advance_width("Reading", &q(&fam(&["DejaVu Sans"]), FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("Reading", &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0));
        assert_eq!(a, b, "DejaVu Sans matches the sans-serif generic face");
    }

    #[test]
    fn add_face_bad_bytes_returns_false_and_falls_back() {
        let mut f = db();
        assert!(!f.add_face("X", None, None, b"not a font".to_vec()));
        // A query for the unregistered family falls back to sans, no panic.
        let x = fam(&["X", "sans-serif"]);
        let a = f.advance_width("hi", &q(&x, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width("hi", &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0));
        assert_eq!(a, b);
    }

    #[test]
    fn add_face_weight_picks_bold() {
        // Two faces for the same family: weight 400 (regular) + weight 700
        // (bold), with distinct metrics. Query weight 700 → the bold face;
        // weight 400 → the regular; weight 600 → nearest (bold).
        let mut f = db();
        let myfont = fam(&["MyFont"]);
        assert!(f.add_face("MyFont", Some(400), None, LIB_SERIF.to_vec()));
        assert!(f.add_face("MyFont", Some(700), None, LIB_SERIF_BD.to_vec()));
        let reg = f.advance_width("Reading", &q(&myfont, FontStyle::Normal, 400, 16.0));
        let bold = f.advance_width("Reading", &q(&myfont, FontStyle::Normal, 700, 16.0));
        let mid = f.advance_width("Reading", &q(&myfont, FontStyle::Normal, 600, 16.0));
        assert_ne!(reg, bold, "regular vs bold faces have distinct advances");
        assert_eq!(mid, bold, "weight 600 matches the nearest (700) face");
    }

    #[test]
    fn add_face_style_picks_italic() {
        // Regular + italic faces for one family; query italic → the italic face.
        let mut f = db();
        let myfont = fam(&["MyFont"]);
        assert!(f.add_face("MyFont", None, None, LIB_SERIF.to_vec()));
        assert!(f.add_face("MyFont", None, Some(FontFaceStyle::Italic), LIB_SERIF_IT.to_vec()));
        let normal_id = f.resolve_id(&q(&myfont, FontStyle::Normal, 400, 16.0));
        let italic_id = f.resolve_id(&q(&myfont, FontStyle::Italic, 400, 16.0));
        assert_ne!(normal_id, italic_id, "italic resolves to a distinct registered face");
    }

    #[test]
    fn add_face_measure_equals_paint() {
        // The measure == paint invariant holds for a registered @font-face family.
        let mut f = db();
        f.add_face("MyFont", None, None, LIB_SERIF.to_vec());
        let myfont = fam(&["MyFont"]);
        let query = q(&myfont, FontStyle::Normal, 400, 18.0);
        let measured = f.advance_width("Hello, world!", &query);
        let painted: f32 = "Hello, world!"
            .chars()
            .map(|c| f.rasterize_glyph(c, &query).advance)
            .sum();
        assert!((measured - painted).abs() < 1e-3, "measure {measured} == paint {painted}");
    }

    #[test]
    fn spacing_measure_equals_paint() {
        // letter/word-spacing: the measured advance equals the rasterizer's pen
        // walk (g.advance + letter_spacing + word_spacing at spaces), per §4.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let mut query = q(&sans, FontStyle::Normal, 400, 18.0);
        query.letter_spacing = 4.0;
        query.word_spacing = 7.0;
        let text = "a b c";
        let measured = f.advance_width(text, &query);
        let painted: f32 = text
            .chars()
            .map(|c| {
                f.rasterize_glyph(c, &query).advance
                    + query.letter_spacing
                    + if c == ' ' { query.word_spacing } else { 0.0 }
            })
            .sum();
        assert!((measured - painted).abs() < 1e-3, "measure {measured} == paint {painted}");
        // and spacing actually widens vs the unspaced measure.
        let plain = f.advance_width(text, &q(&sans, FontStyle::Normal, 400, 18.0));
        // 5 chars * 4 letter-spacing + 2 spaces * 7 word-spacing = 34.
        assert!((measured - plain - 34.0).abs() < 1e-3, "extra={}", measured - plain);
    }

    #[test]
    fn measure_equals_paint_advance() {
        // The invariant guaranteeing text fits its measured box: the width the
        // measurer sums equals the pen advance the rasterizer walks, per face.
        let f = db();
        let cases = [
            (fam(&["DejaVu Sans"]), FontStyle::Normal, 400u16),
            (fam(&["DejaVu Serif"]), FontStyle::Normal, 400),
            (fam(&["DejaVu Sans Mono"]), FontStyle::Normal, 400),
            (fam(&["Liberation Serif"]), FontStyle::Italic, 400),
            (fam(&["DejaVu Sans"]), FontStyle::Normal, 700),
        ];
        for (family, style, weight) in cases {
            let query = q(&family, style, weight, 18.0);
            let measured = f.advance_width("Hello, world!", &query);
            let painted: f32 = "Hello, world!"
                .chars()
                .map(|c| f.rasterize_glyph(c, &query).advance)
                .sum();
            assert!(
                (measured - painted).abs() < 1e-3,
                "measure {measured} == paint {painted} for {family:?}"
            );
        }
    }
}
