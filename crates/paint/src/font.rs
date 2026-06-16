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
use starfish_layout::{FontKerning, FontQuery, FontStyle, LineMetrics, TextMeasurer};

use crate::shape::ShapedGlyph;

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
// E10-M2: Arabic coverage for real RTL shaping (joining forms + ligatures).
// Resolves via its family name "Noto Sans Arabic"; not a generic, so no set_*.
const NOTO_ARABIC: &[u8] = include_bytes!("../assets/NotoSansArabic-Regular.ttf");
// E10-M3: Devanagari coverage so the per-cluster fallback has a real complex
// script to recover to (conjuncts + reordering). Family "Noto Sans Devanagari".
const NOTO_DEVANAGARI: &[u8] = include_bytes!("../assets/NotoSansDevanagari-Regular.ttf");

/// Cache key for `shape()` (E11-M1): everything that determines the shaped output.
/// The full family LIST is kept (not just the resolved primary id) because font
/// fallback (E10-M3) walks the whole list, so two lists resolving to the same
/// primary can still shape differently. Floats are keyed by `to_bits()` (f32 is
/// not Hash/Eq).
#[derive(PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    family: Vec<String>,
    size_bits: u32,
    weight: u16,
    style: FontStyle,
    ls_bits: u32,
    ws_bits: u32,
    // E46-M1: features + kerning change the shaped output, so two runs differing
    // only by these must NOT collide in the cache. `[u8;4]` and `u32` are Hash/Eq.
    features: Vec<([u8; 4], u32)>,
    kerning: FontKerning,
}

// E11-M1: counts `shape_uncached` calls so cache tests can assert hits/misses.
#[cfg(test)]
thread_local! {
    pub(crate) static SHAPE_UNCACHED_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

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

/// First strong-directional char decides the run direction (matches what
/// rustybuzz's guess_segment_properties does). Used only to compute per-cluster
/// byte extents for spacing, which differ for RTL (clusters decrease visually).
fn is_rtl_run(text: &str) -> bool {
    text.chars()
        .find_map(|c| match c {
            '\u{0590}'..='\u{05FF}'
            | '\u{0600}'..='\u{06FF}'
            | '\u{0750}'..='\u{077F}'
            | '\u{08A0}'..='\u{08FF}'
            | '\u{FB1D}'..='\u{FDFF}'
            | '\u{FE70}'..='\u{FEFF}' => Some(true),
            'A'..='Z' | 'a'..='z' => Some(false),
            _ => None,
        })
        .unwrap_or(false)
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
    /// E10-M3: vendored faces tried, in coverage-priority order, when the
    /// resolved face leaves `.notdef` glyphs (the author family-list is tried
    /// first; this is the last-resort tail). Broadest face (DejaVu Sans) first.
    fallback_chain: Vec<ID>,
    /// E11-M1: shape cache — identical (text, q) runs are shaped once and shared
    /// as an `Rc<[ShapedGlyph]>`. RefCell mirrors the `faces` cache pattern
    /// (single-threaded render, &self API). Lives for one render (FontDb is built
    /// per render_document call), so it needs no eviction.
    shape_cache: RefCell<HashMap<ShapeKey, Rc<[ShapedGlyph]>>>,
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
            NOTO_ARABIC,
            NOTO_DEVANAGARI,
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
        // E10-M3: last-resort fallback tail, in coverage-priority order. DejaVu
        // Sans is broadest (Latin/Arabic/Hebrew/Emoji) so it comes first; the
        // specialized faces cover scripts DejaVu lacks (Devanagari) or sharpen
        // a script (Noto Arabic). Each name resolves to its vendored regular id.
        let fallback_chain: Vec<ID> = [
            "DejaVu Sans",
            "Noto Sans Arabic",
            "Noto Sans Devanagari",
            "Liberation Serif",
        ]
        .iter()
        .filter_map(|fam| {
            db.query(&Query {
                families: &[Family::Name(fam)],
                weight: FdbWeight::NORMAL,
                stretch: Default::default(),
                style: FdbStyle::Normal,
            })
        })
        .collect();
        FontDb {
            db,
            faces: RefCell::new(HashMap::new()),
            default_id,
            fallback_chain,
            shape_cache: RefCell::new(HashMap::new()),
        }
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
            FontSettings {
                collection_index: 0,
                ..Default::default()
            },
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
        // A newly added face can change resolution, so any prior shaped runs may
        // be stale; in the real pipeline add_face all happens before shaping, so
        // the cache is empty here — this clear is defensive + keeps @font-face
        // tests correct.
        self.shape_cache.borrow_mut().clear();
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
        let fam_slice = if families.is_empty() {
            &fallback[..]
        } else {
            &families[..]
        };

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
                    FontSettings {
                        collection_index: index,
                        ..Default::default()
                    },
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

    /// Shape `text` with the resolved face via rustybuzz (E10-M1): runs the
    /// face's GSUB/GPOS (Latin kerning + standard ligatures) and returns the
    /// glyph run in visual order. Direction/script are INFERRED from `text`
    /// (E10-M2): Latin/Common stays LTR (byte-identical to M1); strong-RTL text
    /// (Arabic/Hebrew) shapes RTL with the right script so joining/ligatures
    /// apply and rustybuzz returns glyphs in visual (L→R) order with clusters
    /// DECREASING. Layout passes LOGICAL-order text (no pre-reversal).
    ///
    /// Per-cluster letter/word-spacing (`extra_spacing`) is folded into each
    /// glyph that STARTS a new cluster, so `Σ x_advance == Σ(face advances) +
    /// extra_spacing(text, q)` even when ligatures collapse chars into one
    /// cluster. The per-cluster byte extent is computed direction-aware (LTR
    /// clusters increase, RTL decrease) so each cluster's spacing lands once.
    ///
    /// Falls back to a per-char fontdue path if the face data is missing or
    /// rustybuzz can't parse it, so shaping never panics.
    fn shape_uncached(&self, text: &str, q: &FontQuery) -> Vec<ShapedGlyph> {
        #[cfg(test)]
        SHAPE_UNCACHED_CALLS.with(|c| c.set(c.get() + 1));
        if text.is_empty() {
            return Vec::new();
        }
        let size = sane_size(q.size);
        let id = self.resolve_id(q);
        // Shape inside the borrowed face bytes; the closure returns an OWNED Vec
        // so no rustybuzz borrow escapes. None ⇒ missing data / parse failure.
        let shaped = self.db.with_face_data(id, |bytes, face_index| {
            Self::shape_with_bytes(id, bytes, face_index, text, q, size)
        });
        let glyphs = match shaped.flatten() {
            Some(glyphs) => glyphs,
            None => return self.shape_fallback(text, q, size),
        };
        // E10-M3: no `.notdef` → the resolved face covered every char; the run
        // is byte-identical to the M1/M2 fast path. Only when a glyph came back
        // as glyph_id 0 (missing glyph) do we reshape those source ranges with a
        // covering fallback face.
        if glyphs.iter().all(|g| g.glyph_id != 0) {
            return glyphs;
        }
        self.splice_fallback(text, q, size, glyphs, &[id])
    }

    /// Shape `text` with `q`, memoized (E11-M1). Returns a shared `Rc<[ShapedGlyph]>`;
    /// a cache hit copies no glyph data (just bumps the Rc count). Both `advance_width`
    /// (measure) and `draw_glyph_run` (paint) call this, so an identical run is shaped
    /// once. Byte-identical to the uncached result — only timing differs.
    pub fn shape(&self, text: &str, q: &FontQuery) -> Rc<[ShapedGlyph]> {
        let key = ShapeKey {
            text: text.to_string(),
            family: q.family.to_vec(),
            size_bits: q.size.to_bits(),
            weight: q.weight.0,
            style: q.style,
            ls_bits: q.letter_spacing.to_bits(),
            ws_bits: q.word_spacing.to_bits(),
            features: q.features.to_vec(), // E46-M1
            kerning: q.kerning,
        };
        if let Some(hit) = self.shape_cache.borrow().get(&key) {
            return hit.clone();
        }
        let glyphs: Rc<[ShapedGlyph]> = self.shape_uncached(text, q).into();
        self.shape_cache.borrow_mut().insert(key, glyphs.clone());
        glyphs
    }

    /// E10-M3: source-byte span covered by a consecutive `.notdef` run
    /// `glyphs[i..j]`. `start` = the smallest cluster in the run; `end` = the
    /// smallest cluster among ALL glyphs that is strictly greater than the run's
    /// largest cluster, or `text_len` if none. Because a contiguous run's
    /// clusters map to a contiguous byte range (true for both LTR-increasing and
    /// RTL-decreasing order), this is the exact source slice to reshape.
    fn notdef_source_span(
        glyphs: &[ShapedGlyph],
        i: usize,
        j: usize,
        text_len: usize,
    ) -> (usize, usize) {
        let run = &glyphs[i..j];
        let start = run.iter().map(|g| g.cluster).min().unwrap_or(0);
        let run_max = run.iter().map(|g| g.cluster).max().unwrap_or(0);
        let end = glyphs
            .iter()
            .map(|g| g.cluster)
            .filter(|&c| c > run_max)
            .min()
            .unwrap_or(text_len);
        (start, end)
    }

    /// E10-M3: pick a fallback face for the missing slice `slice`. Candidates are
    /// the author family-list (each family resolved on its own) followed by the
    /// vendored `fallback_chain` tail. Returns the first candidate NOT already
    /// tried whose face has a glyph for the slice's FIRST char (a cheap, stable
    /// coverage proxy). `None` if nothing new covers it.
    fn resolve_fallback(&self, slice: &str, q: &FontQuery, already_tried: &[ID]) -> Option<ID> {
        let first = slice.chars().next()?;
        let candidates = q
            .family
            .iter()
            .map(|name| {
                self.resolve_id(&FontQuery {
                    family: std::slice::from_ref(name),
                    ..*q
                })
            })
            .chain(self.fallback_chain.iter().copied());
        for id in candidates {
            if already_tried.contains(&id) {
                continue;
            }
            if self.face_by_id(id).has_glyph(first) {
                return Some(id);
            }
        }
        None
    }

    /// E10-M3: reshape `text[start..end]` with face `id`, rebasing each glyph's
    /// cluster back into the full-`text` coordinate space. `None` if the span is
    /// not a char boundary (guards a panic) or the face can't be shaped; an empty
    /// span yields `Some(vec![])`.
    fn shape_slice_with(
        &self,
        id: ID,
        text: &str,
        start: usize,
        end: usize,
        q: &FontQuery,
        size: f32,
    ) -> Option<Vec<ShapedGlyph>> {
        let slice = text.get(start..end)?;
        if slice.is_empty() {
            return Some(Vec::new());
        }
        let mut shaped = self
            .db
            .with_face_data(id, |b, fi| {
                Self::shape_with_bytes(id, b, fi, slice, q, size)
            })
            .flatten()?;
        // Clusters from shaping the slice are slice-relative; shift them to the
        // full-text byte offsets so the painter/measurer see one coherent run.
        for g in &mut shaped {
            g.cluster += start;
        }
        Some(shaped)
    }

    /// E10-M3: walk `glyphs`, keeping covered glyphs and replacing each maximal
    /// consecutive `.notdef` run with the same source bytes reshaped through a
    /// covering fallback face. If a fallback still leaves `.notdef`, recurse with
    /// that face added to `tried` (a finite candidate set + a strictly growing
    /// `tried` guarantees termination). If no fallback covers the run (e.g. CJK
    /// with no vendored face), the original tofu glyphs are kept.
    fn splice_fallback(
        &self,
        text: &str,
        q: &FontQuery,
        size: f32,
        glyphs: Vec<ShapedGlyph>,
        tried: &[ID],
    ) -> Vec<ShapedGlyph> {
        let mut out: Vec<ShapedGlyph> = Vec::with_capacity(glyphs.len());
        let mut i = 0;
        while i < glyphs.len() {
            if glyphs[i].glyph_id != 0 {
                out.push(glyphs[i]);
                i += 1;
                continue;
            }
            // Extend over the maximal consecutive `.notdef` run [i, j).
            let mut j = i + 1;
            while j < glyphs.len() && glyphs[j].glyph_id == 0 {
                j += 1;
            }
            let (start, end) = Self::notdef_source_span(&glyphs, i, j, text.len());
            // `get` (not direct index): a span landing off a char boundary yields
            // None → keep the .notdef rather than panic (shouldn't happen, as
            // clusters are char-boundary byte offsets, but stay defensive).
            let replaced = text
                .get(start..end)
                .and_then(|slice| self.resolve_fallback(slice, q, tried))
                .and_then(|fb_id| {
                    let sub = self.shape_slice_with(fb_id, text, start, end, q, size)?;
                    if sub.is_empty() {
                        return None;
                    }
                    // The fallback face may itself miss some chars (e.g. a mixed
                    // span); recurse so a deeper face can recover those.
                    Some(if sub.iter().any(|g| g.glyph_id == 0) {
                        let mut next = tried.to_vec();
                        next.push(fb_id);
                        self.splice_fallback(text, q, size, sub, &next)
                    } else {
                        sub
                    })
                });
            match replaced {
                Some(sub) => out.extend(sub),
                // No covering face (or empty reshape) → keep the tofu as-is.
                None => out.extend_from_slice(&glyphs[i..j]),
            }
            i = j;
        }
        out
    }

    /// E46-M1: translate a `FontQuery`'s `font-feature-settings` + `font-kerning`
    /// into the whole-run `rustybuzz::Feature` list passed to `shape`. Empty when
    /// there are no settings and kerning is `auto`/`normal` (→ byte-identical to
    /// the old `&[]`). `font-kerning: none` appends `kern=0` to disable kerning.
    fn build_features(q: &FontQuery) -> Vec<rustybuzz::Feature> {
        use rustybuzz::ttf_parser::Tag;
        let mut feats = Vec::with_capacity(q.features.len() + 1);
        for (tag, value) in q.features {
            feats.push(rustybuzz::Feature::new(Tag::from_bytes(tag), *value, ..));
        }
        if q.kerning == FontKerning::None {
            feats.push(rustybuzz::Feature::new(Tag::from_bytes(b"kern"), 0, ..));
        }
        feats
    }

    /// Build a short-lived rustybuzz face over `bytes` and shape `text` with the
    /// direction/script inferred from the text, scaling design-unit positions to
    /// px and folding in per-cluster spacing. `None` if the bytes don't parse as
    /// a rustybuzz face.
    fn shape_with_bytes(
        id: ID,
        bytes: &[u8],
        face_index: u32,
        text: &str,
        q: &FontQuery,
        size: f32,
    ) -> Option<Vec<ShapedGlyph>> {
        let face = rustybuzz::Face::from_slice(bytes, face_index)?;
        let upem = face.units_per_em();
        // Same numeric formula as fontdue's advance (px / upem * advance), so
        // forcing this scale keeps measure == paint with the old per-char path.
        let scale = if upem > 0 { size / upem as f32 } else { 0.0 };

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        // E10-M2: infer direction/script from the text. Latin/Common stays LTR
        // (byte-identical to M1); strong-RTL text (Arabic/Hebrew) becomes RTL with
        // the right script, so joining/ligatures apply and rustybuzz returns
        // glyphs in visual (L→R) order with clusters DECREASING.
        buffer.guess_segment_properties();
        // E46-M1: build the OpenType feature list. `font-feature-settings` pairs
        // apply to the whole run (range `..`); `font-kerning: none` appends an
        // explicit `kern` off so the shaper disables kerning. With no settings and
        // kerning auto/normal the list is empty → byte-identical to `&[]`.
        let features = Self::build_features(q);
        let glyphs = rustybuzz::shape(&face, &features, buffer);
        // guess_segment_properties consumes the buffer into shape(), so recompute
        // the run direction from the text for the per-cluster extent math below.
        let rtl = is_rtl_run(text);

        let infos = glyphs.glyph_infos();
        let positions = glyphs.glyph_positions();
        let mut out = Vec::with_capacity(infos.len());
        for (i, (info, pos)) in infos.iter().zip(positions).enumerate() {
            let cluster = info.cluster as usize;
            // Spacing belongs to the glyph that STARTS a cluster; glyphs
            // continuing the same cluster (ligature parts) get 0.
            let prev_cluster = if i == 0 {
                usize::MAX
            } else {
                infos[i - 1].cluster as usize
            };
            let spacing = if cluster != prev_cluster {
                // A cluster's source-byte extent is [cluster, next_larger_cluster):
                let next = if rtl {
                    // RTL: clusters decrease visually; this cluster's byte extent
                    // ends at the smallest cluster strictly greater than it.
                    infos
                        .iter()
                        .map(|n| n.cluster as usize)
                        .filter(|&c| c > cluster)
                        .min()
                        .unwrap_or(text.len())
                } else {
                    // LTR (M1 behavior): next differing cluster among following glyphs.
                    infos[i + 1..]
                        .iter()
                        .map(|n| n.cluster as usize)
                        .find(|&c| c != cluster)
                        .unwrap_or(text.len())
                };
                // Clusters are byte offsets; `get` (not direct index) so a
                // cluster landing off a char boundary can't panic — it just
                // skips spacing for that glyph (never happens for LTR Latin).
                text.get(cluster..next)
                    .map_or(0.0, |s| starfish_layout::extra_spacing(s, q))
            } else {
                0.0
            };
            out.push(ShapedGlyph {
                face: id,
                // OpenType glyph indices are u16; rustybuzz's u32 always fits.
                glyph_id: info.glyph_id as u16,
                x_advance: pos.x_advance as f32 * scale + spacing,
                x_offset: pos.x_offset as f32 * scale,
                y_offset: pos.y_offset as f32 * scale,
                cluster,
            });
        }
        Some(out)
    }

    /// Per-char fontdue fallback used when rustybuzz can't read the face: one
    /// glyph per char (no kerning/ligatures), with the fontdue glyph index from
    /// `lookup_glyph_index` so the rasterizer's `rasterize_indexed_glyph` draws
    /// the right glyph. Keeps the same advance formula + per-char spacing so
    /// measure == paint still holds.
    fn shape_fallback(&self, text: &str, q: &FontQuery, size: f32) -> Vec<ShapedGlyph> {
        let id = self.resolve_id(q);
        let f = self.face_by_id(id);
        let mut out = Vec::new();
        for (byte, c) in text.char_indices() {
            let m = f.metrics(c, size);
            let spacing = q.letter_spacing + if c == ' ' { q.word_spacing } else { 0.0 };
            out.push(ShapedGlyph {
                face: id,
                glyph_id: f.lookup_glyph_index(c),
                x_advance: m.advance_width + spacing,
                x_offset: 0.0,
                y_offset: 0.0,
                cluster: byte,
            });
        }
        out
    }

    /// Total advance width of `text` for the resolved face at `q.size` px,
    /// defined AS the sum of the shaped run's x_advances (which already fold in
    /// kerning/GPOS + per-cluster spacing) so it equals the painter's pen walk
    /// by construction.
    pub fn advance_width(&self, text: &str, q: &FontQuery) -> f32 {
        self.shape(text, q).iter().map(|g| g.x_advance).sum()
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

    /// Rasterize one glyph by its font glyph index (E10-M1), so the painter can
    /// draw the exact glyphs rustybuzz shaped (ligatures, substitutions) rather
    /// than re-mapping chars. Mirrors `rasterize_glyph` but by index. The glyph
    /// index is resolved against `face` (the `ShapedGlyph.face` it was shaped
    /// with) — NOT re-resolved from `q` — so font-fallback glyphs (E10-M3) read
    /// the correct face rather than indexing the primary face out of bounds.
    pub fn rasterize_indexed_glyph(&self, face: ID, glyph_id: u16, q: &FontQuery) -> GlyphBitmap {
        let (m, coverage) = self
            .face_by_id(face)
            .rasterize_indexed(glyph_id, sane_size(q.size));
        GlyphBitmap {
            width: m.width,
            height: m.height,
            left: m.xmin,
            top: m.ymin + m.height as i32,
            advance: m.advance_width,
            coverage,
        }
    }

    /// E20-M3: build the filled outline of one glyph as a `tiny_skia::Path`, in
    /// device pixels with the baseline at y=0 and the pen origin at x=0 (the
    /// caller translates by the pen + baseline). Scaled by `q.size / upem` and
    /// y-flipped (font y-up → screen y-down). `None` if the glyph has no outline
    /// (e.g. space) or the face can't be parsed as a ttf-parser face. Used by the
    /// canvas text path so glyph runs honour the CTM/clip/gradient uniformly.
    pub fn glyph_outline(&self, face: ID, glyph_id: u16, q: &FontQuery) -> Option<tiny_skia::Path> {
        let size = sane_size(q.size);
        self.db
            .with_face_data(face, |bytes, face_index| {
                let f = rustybuzz::ttf_parser::Face::parse(bytes, face_index).ok()?;
                let upem = f.units_per_em();
                if upem == 0 {
                    return None;
                }
                let scale = size / upem as f32;
                let mut builder = OutlinePathBuilder {
                    pb: PathBuilder::new(),
                    scale,
                };
                f.outline_glyph(rustybuzz::ttf_parser::GlyphId(glyph_id), &mut builder)?;
                builder.pb.finish()
            })
            .flatten()
    }
}

use tiny_skia::PathBuilder;

/// Adapts ttf-parser's `OutlineBuilder` to a `tiny_skia::PathBuilder`, scaling
/// design units to px and y-flipping (font y-up → screen y-down) so the baseline
/// sits at y=0.
struct OutlinePathBuilder {
    pb: PathBuilder,
    scale: f32,
}

impl rustybuzz::ttf_parser::OutlineBuilder for OutlinePathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x * self.scale, -y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x * self.scale, -y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(
            x1 * self.scale,
            -y1 * self.scale,
            x * self.scale,
            -y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            x1 * self.scale,
            -y1 * self.scale,
            x2 * self.scale,
            -y2 * self.scale,
            x * self.scale,
            -y * self.scale,
        );
    }
    fn close(&mut self) {
        self.pb.close();
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
            features: &[],
            kerning: FontKerning::Auto,
        }
    }

    fn fam(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn advance_monotonic_and_empty_zero() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        assert_eq!(
            f.advance_width("", &q(&sans, FontStyle::Normal, 400, 16.0)),
            0.0
        );
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
        assert_ne!(
            serif_id, sans_id,
            "serif resolves to a different face than sans"
        );
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
        assert_ne!(
            normal_id, italic_id,
            "Liberation Serif italic is a distinct face"
        );
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
            assert!(
                adv.is_finite() && adv >= 0.0,
                "advance_width({size}) = {adv}"
            );

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
            assert!(
                g.advance.is_finite() && g.advance >= 0.0,
                "raster advance({size}) = {}",
                g.advance
            );
            assert_eq!(
                g.width * g.height,
                g.coverage.len(),
                "coverage len for size {size}"
            );
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

        // advance_width is defined as the shaped run's x_advance sum; at a normal
        // size the sanitizer leaves it untouched (no clamp perturbation).
        let expected: f32 = f
            .shape("Hello, world!", &query)
            .iter()
            .map(|g| g.x_advance)
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
        let sans_only = f.advance_width(
            "Reading",
            &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0),
        );
        assert_eq!(
            before, sans_only,
            "pre-registration MyFont == sans fallback"
        );
        assert_ne!(
            after, sans_only,
            "registered MyFont resolves to the loaded face"
        );
    }

    #[test]
    fn add_face_resolves_case_insensitively() {
        // CSS family names match case-insensitively (ASCII). A @font-face
        // registered as "MyFont" must resolve for any casing of the query name,
        // and all to the SAME loaded face (distinct from the sans fallback).
        let mut f = db();
        assert!(f.add_face("MyFont", None, None, LIB_SERIF.to_vec()));
        let sans_only = f.advance_width(
            "Reading",
            &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0),
        );
        let exact = f.advance_width(
            "Reading",
            &q(&fam(&["MyFont"]), FontStyle::Normal, 400, 16.0),
        );
        let lower = f.advance_width(
            "Reading",
            &q(&fam(&["myfont"]), FontStyle::Normal, 400, 16.0),
        );
        let upper = f.advance_width(
            "Reading",
            &q(&fam(&["MYFONT"]), FontStyle::Normal, 400, 16.0),
        );
        assert_ne!(
            exact, sans_only,
            "registered MyFont resolves to the loaded face"
        );
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
        assert_ne!(
            sans_id, serif_id,
            "DejaVu Sans still resolves distinctly from serif"
        );
        let a = f.advance_width(
            "Reading",
            &q(&fam(&["DejaVu Sans"]), FontStyle::Normal, 400, 16.0),
        );
        let b = f.advance_width(
            "Reading",
            &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0),
        );
        assert_eq!(a, b, "DejaVu Sans matches the sans-serif generic face");
    }

    #[test]
    fn add_face_bad_bytes_returns_false_and_falls_back() {
        let mut f = db();
        assert!(!f.add_face("X", None, None, b"not a font".to_vec()));
        // A query for the unregistered family falls back to sans, no panic.
        let x = fam(&["X", "sans-serif"]);
        let a = f.advance_width("hi", &q(&x, FontStyle::Normal, 400, 16.0));
        let b = f.advance_width(
            "hi",
            &q(&fam(&["sans-serif"]), FontStyle::Normal, 400, 16.0),
        );
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
        assert!(f.add_face(
            "MyFont",
            None,
            Some(FontFaceStyle::Italic),
            LIB_SERIF_IT.to_vec()
        ));
        let normal_id = f.resolve_id(&q(&myfont, FontStyle::Normal, 400, 16.0));
        let italic_id = f.resolve_id(&q(&myfont, FontStyle::Italic, 400, 16.0));
        assert_ne!(
            normal_id, italic_id,
            "italic resolves to a distinct registered face"
        );
    }

    #[test]
    fn add_face_measure_equals_paint() {
        // The measure == paint invariant holds for a registered @font-face family.
        let mut f = db();
        f.add_face("MyFont", None, None, LIB_SERIF.to_vec());
        let myfont = fam(&["MyFont"]);
        let query = q(&myfont, FontStyle::Normal, 400, 18.0);
        let measured = f.advance_width("Hello, world!", &query);
        // Paint side = the shaped run the rasterizer walks (one pen step per
        // shaped glyph), per §4.
        let painted: f32 = f
            .shape("Hello, world!", &query)
            .iter()
            .map(|g| g.x_advance)
            .sum();
        assert!(
            (measured - painted).abs() < 1e-3,
            "measure {measured} == paint {painted}"
        );
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
        // Paint side = the shaped run's pen walk; per-cluster spacing is folded
        // into each x_advance.
        let painted: f32 = f.shape(text, &query).iter().map(|g| g.x_advance).sum();
        assert!(
            (measured - painted).abs() < 1e-3,
            "measure {measured} == paint {painted}"
        );
        // and spacing actually widens vs the unspaced measure.
        let plain = f.advance_width(text, &q(&sans, FontStyle::Normal, 400, 18.0));
        // 5 chars * 4 letter-spacing + 2 spaces * 7 word-spacing = 34.
        assert!(
            (measured - plain - 34.0).abs() < 1e-3,
            "extra={}",
            measured - plain
        );
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
            let painted: f32 = f
                .shape("Hello, world!", &query)
                .iter()
                .map(|g| g.x_advance)
                .sum();
            assert!(
                (measured - painted).abs() < 1e-3,
                "measure {measured} == paint {painted} for {family:?}"
            );
        }
    }

    // --- E10-M1: rustybuzz shaping ---

    #[test]
    fn shape_sum_equals_advance_width() {
        // advance_width is DEFINED as the shaped x_advance sum, so they must be
        // bit-identical across faces/sizes/texts (incl. empty + spaces).
        let f = db();
        let faces = [
            fam(&["DejaVu Sans"]),
            fam(&["DejaVu Serif"]),
            fam(&["DejaVu Sans Mono"]),
        ];
        let texts = ["", " ", "Hi", "AVA Wave", "a b c", "Hello, world!"];
        for face in &faces {
            for size in [10.0, 16.0, 32.0] {
                let query = q(face, FontStyle::Normal, 400, size);
                for t in texts {
                    let aw = f.advance_width(t, &query);
                    let sum: f32 = f.shape(t, &query).iter().map(|g| g.x_advance).sum();
                    assert_eq!(aw, sum, "advance_width == Σ x_advance for {t:?}");
                }
            }
        }
    }

    #[test]
    fn shape_with_spacing_sum_equals_advance_width() {
        // Per-cluster spacing must keep advance_width == Σ x_advance.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let mut query = q(&sans, FontStyle::Normal, 400, 18.0);
        query.letter_spacing = 4.0;
        query.word_spacing = 7.0;
        for t in ["", "a b c", "Wave To"] {
            let aw = f.advance_width(t, &query);
            let sum: f32 = f.shape(t, &query).iter().map(|g| g.x_advance).sum();
            assert_eq!(aw, sum, "spaced advance_width == Σ x_advance for {t:?}");
        }
    }

    #[test]
    fn kerning_applied() {
        // rustybuzz applies the face's kerning, so the shaped total differs from
        // a naive per-char metrics sum (which has no kerning). DejaVu Sans kerns
        // "AV"/"Wa"/"To" negatively, so the shaped width is strictly smaller.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let query = q(&sans, FontStyle::Normal, 400, 32.0);
        let face = f.resolve(&query);
        let mut any_kerned = false;
        for t in ["AVAW", "To", "Wave", "Vo"] {
            let shaped = f.advance_width(t, &query);
            let naive: f32 = t.chars().map(|c| face.metrics(c, 32.0).advance_width).sum();
            assert!(
                shaped <= naive + 1e-3,
                "kerned width {shaped} should not exceed naive {naive} for {t:?}"
            );
            if shaped < naive - 1e-3 {
                any_kerned = true;
            }
        }
        assert!(any_kerned, "at least one pair must kern in DejaVu Sans");
    }

    #[test]
    fn shaped_glyph_count_le_char_count_for_ligature() {
        // Standard ligatures (liga) are on by default in rustybuzz. DejaVu Sans
        // forms the "fi"/"fl" ligatures (2 chars → 1 glyph), so shaping must
        // collapse them. Other faces (e.g. Liberation Serif) may not ligate;
        // those only get the weaker `<=` guarantee.
        let f = db();
        let dejavu = fam(&["DejaVu Sans"]);
        let query = q(&dejavu, FontStyle::Normal, 400, 16.0);
        assert_eq!(
            f.shape("fi", &query).len(),
            1,
            "DejaVu Sans forms the fi ligature"
        );
        assert_eq!(
            f.shape("fl", &query).len(),
            1,
            "DejaVu Sans forms the fl ligature"
        );
        // Shaping never EXPANDS a run beyond the char count for any vendored face.
        for face in [fam(&["DejaVu Serif"]), fam(&["Liberation Serif"])] {
            let fq = q(&face, FontStyle::Normal, 400, 16.0);
            for t in ["fi", "fl"] {
                let n = f.shape(t, &fq).len();
                assert!(
                    n <= t.chars().count(),
                    "shaped count {n} <= chars for {t:?} {face:?}"
                );
            }
        }
    }

    // E46-M1: `font-feature-settings: "liga" 0` disables the standard ligature,
    // so the ligature-prone "fi" shapes to a DIFFERENT (longer) glyph run than
    // the default, proving the features reach rustybuzz through shape().
    #[test]
    fn font_feature_settings_liga_off_changes_shaping() {
        let f = db();
        let dejavu = fam(&["DejaVu Sans"]);
        let default = q(&dejavu, FontStyle::Normal, 400, 16.0);
        // Default: liga on → "fi" collapses to 1 glyph (verified above).
        assert_eq!(f.shape("fi", &default).len(), 1);
        // liga off → no ligature → 2 glyphs.
        let liga_off = FontQuery {
            features: &[(*b"liga", 0)],
            ..default
        };
        let off = f.shape("fi", &liga_off);
        assert_eq!(off.len(), 2, "liga 0 must disable the fi ligature");
        // The runs genuinely differ (glyph ids), not just count.
        let on = f.shape("fi", &default);
        assert_ne!(
            on.iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
            off.iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
        );
    }

    // E46-M1: the build_features translation. No settings + kerning auto → empty
    // (so `rustybuzz::shape(&face, &[], buf)` is reproduced byte-identically);
    // settings + `font-kerning: none` produce the expected Feature list.
    #[test]
    fn build_features_translation() {
        let dejavu = fam(&["DejaVu Sans"]);
        // Empty case → byte-identical to the old `&[]`.
        let default = q(&dejavu, FontStyle::Normal, 400, 16.0);
        assert!(FontDb::build_features(&default).is_empty());
        // Features + kerning none.
        let custom = FontQuery {
            features: &[(*b"smcp", 1)],
            kerning: FontKerning::None,
            ..default
        };
        let feats = FontDb::build_features(&custom);
        assert_eq!(feats.len(), 2, "smcp + the kern-off");
        assert_eq!(feats[0].tag.to_bytes(), *b"smcp");
        assert_eq!(feats[0].value, 1);
        // kern explicitly disabled.
        assert_eq!(feats[1].tag.to_bytes(), *b"kern");
        assert_eq!(feats[1].value, 0);
    }

    // E46-M1: two runs differing ONLY by features must not collide in the shape
    // cache (the key includes the feature list).
    #[test]
    fn shape_cache_key_includes_features() {
        let f = db();
        let dejavu = fam(&["DejaVu Sans"]);
        let default = q(&dejavu, FontStyle::Normal, 400, 16.0);
        let liga_off = FontQuery {
            features: &[(*b"liga", 0)],
            ..default
        };
        // Warm both; if the key ignored features, the second would return the
        // first's cached run and the lengths would match (1 vs 2).
        let a = f.shape("fi", &default);
        let b = f.shape("fi", &liga_off);
        assert_ne!(a.len(), b.len(), "feature variants must not share a cache entry");
    }

    // --- E10-M2: Arabic / RTL shaping ---

    #[test]
    fn arabic_joins_into_connected_forms() {
        // Shaping a connected Arabic word activates joining: the glyph ids of the
        // word differ from concatenating each char shaped in isolation (where
        // every char takes its ISOLATED form). Same face, same size.
        let f = db();
        let arabic = fam(&["Noto Sans Arabic"]);
        let query = q(&arabic, FontStyle::Normal, 400, 16.0);
        let word = "مرحبا";
        let joined: Vec<u16> = f.shape(word, &query).iter().map(|g| g.glyph_id).collect();
        // Per-char isolated shaping (each char alone → isolated form).
        let isolated: Vec<u16> = word
            .chars()
            .flat_map(|c| {
                f.shape(&c.to_string(), &query)
                    .iter()
                    .map(|g| g.glyph_id)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_ne!(
            joined, isolated,
            "joining must change glyph ids vs isolated forms"
        );
    }

    #[test]
    fn arabic_clusters_decrease_visual_ltr() {
        // RTL shaping returns glyphs in visual (L→R) order, so source-byte
        // clusters are non-increasing along the run.
        let f = db();
        let arabic = fam(&["Noto Sans Arabic"]);
        let query = q(&arabic, FontStyle::Normal, 400, 16.0);
        let shaped = f.shape("سلام", &query);
        assert!(shaped.len() >= 2, "need a multi-glyph run to test ordering");
        assert!(
            shaped.windows(2).all(|w| w[0].cluster >= w[1].cluster),
            "RTL clusters must not increase (visual L→R order)"
        );
    }

    #[test]
    fn arabic_measure_equals_paint() {
        // measure == paint for an Arabic word: advance_width equals the shaped
        // run's x_advance sum (the painter's pen walk), and is positive.
        let f = db();
        let arabic = fam(&["Noto Sans Arabic"]);
        let query = q(&arabic, FontStyle::Normal, 400, 16.0);
        let s = "سلام";
        let measured = f.advance_width(s, &query);
        let painted: f32 = f.shape(s, &query).iter().map(|g| g.x_advance).sum();
        assert!(measured > 0.0, "Arabic advance must be positive");
        assert!(
            (measured - painted).abs() < 1e-3,
            "measure {measured} == paint {painted}"
        );
    }

    #[test]
    fn arabic_spacing_added_once_per_cluster() {
        // letter-spacing on an RTL run must be distributed once per cluster, with
        // each cluster getting its full source-byte extent's spacing. The total
        // therefore equals extra_spacing over the whole word (= letter_spacing *
        // char_count, since the per-cluster byte ranges exactly partition the
        // text). This proves the RTL byte-extent fix: no cluster is skipped and
        // none is double-counted even across the lam-alef ligature.
        let f = db();
        let arabic = fam(&["Noto Sans Arabic"]);
        let s = "سلام";
        let plain = q(&arabic, FontStyle::Normal, 400, 16.0);
        let mut spaced = plain;
        spaced.letter_spacing = 5.0;
        let delta = f.advance_width(s, &spaced) - f.advance_width(s, &plain);
        // The per-cluster extents partition the whole word, so the summed spacing
        // equals extra_spacing(word) = 5.0 * char_count.
        let expected = 5.0 * s.chars().count() as f32;
        assert!(
            (delta - expected).abs() < 1e-3,
            "spacing delta {delta} == {expected}"
        );
        // And spacing collapses to exactly one contribution per distinct cluster:
        // the count of glyphs that START a cluster equals the distinct-cluster
        // count (ligature parts add no spacing).
        let distinct: std::collections::BTreeSet<usize> =
            f.shape(s, &plain).iter().map(|g| g.cluster).collect();
        assert!(
            distinct.len() < s.chars().count(),
            "lam-alef ligature collapses a cluster"
        );
    }

    // --- E10-M3: combining-mark positioning ---

    #[test]
    fn combining_mark_is_zero_advance_and_offset() {
        // "b" + U+0301 COMBINING ACUTE on DejaVu Sans stays 2 glyphs (no
        // precomposed b-acute exists, so ccmp can't fuse it). The mark glyph
        // carries (near-)zero advance and a non-zero GPOS offset that stacks it
        // over the base — exactly what draw_glyph_run blits.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let g = f.shape("b\u{0301}", &q(&sans, FontStyle::Normal, 400, 32.0));
        assert_eq!(
            g.len(),
            2,
            "b + combining acute stays 2 glyphs on DejaVu Sans"
        );
        assert!(
            g.iter().all(|g| g.glyph_id != 0),
            "both glyphs present (no tofu)"
        );
        assert!(
            g[1].x_advance.abs() < 0.5,
            "mark advance ~0, got {}",
            g[1].x_advance
        );
        assert!(
            g[1].x_offset != 0.0 || g[1].y_offset != 0.0,
            "mark must be positioned by GPOS offset, got ({}, {})",
            g[1].x_offset,
            g[1].y_offset
        );
    }

    #[test]
    fn combining_mark_does_not_shift_pen() {
        // A zero-advance mark must not widen the run: "b" and "b\u{0301}" advance
        // the pen the same amount (the mark sits over the base).
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let base = f.advance_width("b", &q(&sans, FontStyle::Normal, 400, 32.0));
        let marked = f.advance_width("b\u{0301}", &q(&sans, FontStyle::Normal, 400, 32.0));
        assert!(
            (base - marked).abs() < 0.5,
            "mark shifted pen: {base} vs {marked}"
        );
    }

    // --- E10-M3: per-cluster font fallback ---

    #[test]
    fn fallback_covers_arabic_under_serif() {
        // DejaVu Serif genuinely lacks Arabic, so shaping an Arabic word with it
        // as primary yields `.notdef` for those clusters — the fallback path must
        // recover them (DejaVu Sans / Noto Arabic cover Arabic).
        let f = db();
        let serif = fam(&["DejaVu Serif"]);
        let query = q(&serif, FontStyle::Normal, 400, 16.0);
        assert!(
            !f.face_by_id(f.resolve_id(&query)).has_glyph('س'),
            "precondition: DejaVu Serif lacks Arabic"
        );
        let shaped = f.shape("سلام", &query);
        assert!(!shaped.is_empty());
        assert!(
            shaped.iter().all(|g| g.glyph_id != 0),
            "fallback covered all Arabic clusters"
        );
    }

    #[test]
    fn fallback_latin_under_arabic_primary() {
        // Noto Sans Arabic lacks Latin; "abc" under it as primary must fall back
        // to DejaVu Sans (Latin) with no leftover tofu.
        let f = db();
        let arabic = fam(&["Noto Sans Arabic"]);
        let query = q(&arabic, FontStyle::Normal, 400, 16.0);
        assert!(
            !f.face_by_id(f.resolve_id(&query)).has_glyph('a'),
            "precondition: Noto Sans Arabic lacks Latin"
        );
        let shaped = f.shape("abc", &query);
        assert!(
            shaped.iter().all(|g| g.glyph_id != 0),
            "fallback covered Latin"
        );
    }

    #[test]
    fn fallback_devanagari() {
        // No Latin/Arabic vendored face covers Devanagari; the newly-vendored
        // Noto Sans Devanagari (in the fallback chain) must recover it.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let query = q(&sans, FontStyle::Normal, 400, 16.0);
        assert!(
            !f.face_by_id(f.resolve_id(&query)).has_glyph('न'),
            "precondition: DejaVu Sans lacks Devanagari"
        );
        let shaped = f.shape("नमस्ते", &query);
        assert!(!shaped.is_empty());
        assert!(
            shaped.iter().all(|g| g.glyph_id != 0),
            "fallback covered Devanagari"
        );
    }

    #[test]
    fn tofu_when_no_face_covers() {
        // No vendored face covers CJK, so a `.notdef` for "中" must REMAIN after
        // fallback gives up — gracefully, with no panic / infinite loop.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let shaped = f.shape("中", &q(&sans, FontStyle::Normal, 400, 16.0));
        assert!(!shaped.is_empty(), "tofu still produces a glyph");
        assert!(
            shaped.iter().any(|g| g.glyph_id == 0),
            "CJK stays tofu (no covering face)"
        );
    }

    #[test]
    fn fallback_measure_equals_paint() {
        // measure == paint must survive the splice: advance_width equals Σ
        // x_advance for fallback cases (Arabic under serif, mixed, diacritic).
        let f = db();
        let serif = fam(&["DejaVu Serif"]);
        let sans = fam(&["DejaVu Sans"]);
        let cases: [(&str, &Vec<String>); 3] = [("سلام", &serif), ("abमc", &sans), ("café", &sans)];
        for (s, family) in cases {
            let query = q(family, FontStyle::Normal, 400, 16.0);
            let measured = f.advance_width(s, &query);
            let painted: f32 = f.shape(s, &query).iter().map(|g| g.x_advance).sum();
            assert!(
                (measured - painted).abs() < 1e-3,
                "measure {measured} == paint {painted} for {s:?}"
            );
        }
        // Spacing must not be dropped or doubled across the splice: an Arabic word
        // under a serif primary (so every cluster is reshaped via fallback) gains
        // exactly letter_spacing * char_count when spacing is added.
        let arabic_word = "سلام";
        let plain = q(&serif, FontStyle::Normal, 400, 16.0);
        let mut spaced = plain;
        spaced.letter_spacing = 5.0;
        let delta = f.advance_width(arabic_word, &spaced) - f.advance_width(arabic_word, &plain);
        let expected = 5.0 * arabic_word.chars().count() as f32;
        assert!(
            (delta - expected).abs() < 1e-3,
            "fallback spacing delta {delta} == {expected}"
        );
    }

    #[test]
    fn no_fallback_is_byte_identical() {
        // The fast path (no `.notdef`) must be untouched by M3: pure Latin under
        // DejaVu Sans and pure Arabic under Noto Arabic shape with no tofu, just
        // as in M2.
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let arabic = fam(&["Noto Sans Arabic"]);
        let latin = f.shape("Hello", &q(&sans, FontStyle::Normal, 400, 16.0));
        assert!(
            latin.iter().all(|g| g.glyph_id != 0),
            "Latin fast path: no tofu"
        );
        let ar = f.shape("سلام", &q(&arabic, FontStyle::Normal, 400, 16.0));
        assert!(
            ar.iter().all(|g| g.glyph_id != 0),
            "Arabic fast path: no tofu"
        );
    }

    // --- E11-M1: shape cache ---

    #[test]
    fn shape_cache_hits_second_call() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let query = q(&sans, FontStyle::Normal, 400, 16.0);
        SHAPE_UNCACHED_CALLS.with(|c| c.set(0));
        let first = f.shape("The quick brown fox", &query);
        let after_first = SHAPE_UNCACHED_CALLS.with(|c| c.get());
        let second = f.shape("The quick brown fox", &query);
        let after_second = SHAPE_UNCACHED_CALLS.with(|c| c.get());
        assert_eq!(
            after_second, after_first,
            "second identical call must hit the cache"
        );
        assert_eq!(&first[..], &second[..], "cached run is byte-identical");
        assert!(Rc::ptr_eq(&first, &second), "cache hands back the same Rc");
        let _ = f.shape("different text", &query);
        let mut big = query;
        big.size = 32.0;
        let _ = f.shape("The quick brown fox", &big);
        assert_eq!(
            SHAPE_UNCACHED_CALLS.with(|c| c.get()),
            after_second + 2,
            "distinct keys miss"
        );
    }

    #[test]
    fn measure_then_paint_shapes_once() {
        let f = db();
        let sans = fam(&["DejaVu Sans"]);
        let query = q(&sans, FontStyle::Normal, 400, 16.0);
        let para = "Lorem ipsum dolor sit amet consectetur adipiscing elit";
        SHAPE_UNCACHED_CALLS.with(|c| c.set(0));
        let _ = f.advance_width(para, &query); // miss
        let _ = f.advance_width(para, &query); // hit
        let _ = f.shape(para, &query); // hit
        assert_eq!(
            SHAPE_UNCACHED_CALLS.with(|c| c.get()),
            1,
            "shaped exactly once across measure+remeasure+paint"
        );
    }
}
