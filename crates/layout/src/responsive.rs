//! Responsive image source selection (E15-M2): `<img srcset>` density/width
//! descriptors + `sizes`, and `<picture>`/`<source>` (`media`/`type`/`srcset`).
//!
//! The CHOSEN url feeds the existing decode + blit pipeline. The single entry
//! point [`resolve_img_src`] is used by BOTH paint's `decode_images` and the
//! box tree's `<img>` branch, so the decoded URL and the blitted URL agree. A
//! plain `<img src=x>` (no srcset, not under `<picture>`) resolves to `x`
//! exactly as the raw `get_attribute("src")` did — byte-identical output.

use starfish_css::parse_media_query;
use starfish_dom::{Document, NodeId};
use starfish_style::{media_matches, Viewport};

/// Fixed device-pixel-ratio. The engine has no real DPR yet; 1.0 keeps a plain
/// density-less page selecting the same source it would without responsiveness.
pub const DEVICE_PIXEL_RATIO: f32 = 1.0;

/// One parsed `srcset` candidate: a url with an optional density (`2x`) or
/// width (`640w`) descriptor. At most one descriptor is set (or neither = 1x).
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    url: String,
    density: Option<f32>,
    width: Option<u32>,
}

/// Parse a `srcset` attribute into candidates. Comma-separated; each entry is
/// `<url> [descriptor]` (the first run of whitespace splits the url from the
/// descriptor). `Nx` → density (f32 > 0), `Nw` → width (u32 > 0); a missing or
/// unrecognized descriptor means 1x (None, None). Malformed entries (empty url,
/// non-positive/invalid descriptor) are skipped. (Data URLs containing commas
/// are not handled — rare; the caller falls back to `src`.)
fn parse_srcset(s: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in s.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut it = entry.split_whitespace();
        let Some(url) = it.next() else { continue };
        let desc = it.next();
        let (density, width) = match desc {
            None => (None, None),
            Some(d) => parse_descriptor(d),
        };
        out.push(Candidate {
            url: url.to_string(),
            density,
            width,
        });
    }
    out
}

/// Parse a single `srcset` descriptor: `Nx` → density, `Nw` → width. An
/// invalid or non-positive value → (None, None) (treated as 1x).
fn parse_descriptor(d: &str) -> (Option<f32>, Option<u32>) {
    if let Some(num) = d.strip_suffix('x') {
        if let Ok(v) = num.parse::<f32>() {
            if v > 0.0 {
                return (Some(v), None);
            }
        }
    } else if let Some(num) = d.strip_suffix('w') {
        if let Ok(v) = num.parse::<u32>() {
            if v > 0 {
                return (None, Some(v));
            }
        }
    }
    (None, None)
}

/// Resolve the effective image width from a `sizes` attribute against `vp`.
/// `sizes` is comma-separated; each entry is `(<media-condition>) <length>` or
/// a bare `<length>` default. The FIRST entry whose media-condition matches
/// (a bare entry always matches) yields its length resolved to px. `None` if no
/// entry matches or `sizes` is absent/unparsable.
fn sizes_width(sizes: &str, vp: Viewport) -> Option<f32> {
    for entry in sizes.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // `(media) length` — split the parenthesized condition from the length.
        if let Some(rest) = entry.strip_prefix('(') {
            if let Some(close) = rest.find(')') {
                let cond = &rest[..close];
                let len = rest[close + 1..].trim();
                if media_matches(&parse_media_query(&format!("({cond})")), vp) {
                    return length_px(len, vp);
                }
                continue;
            }
            // Unbalanced parens → skip this entry.
            continue;
        }
        // Bare length default → always matches.
        return length_px(entry, vp);
    }
    None
}

/// Minimal length parser for `sizes` values: `Npx` → N; `Nvw` → N/100 * vp.width;
/// `N%` → N/100 * vp.width. Anything else → None. (`sizes` lengths are simple.)
fn length_px(s: &str, vp: Viewport) -> Option<f32> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("px") {
        n.trim().parse::<f32>().ok()
    } else if let Some(n) = s.strip_suffix("vw") {
        n.trim().parse::<f32>().ok().map(|v| v / 100.0 * vp.width)
    } else if let Some(n) = s.strip_suffix('%') {
        n.trim().parse::<f32>().ok().map(|v| v / 100.0 * vp.width)
    } else {
        None
    }
}

/// Pick the best candidate url for `vp` + `dpr`, or `None` if `cands` is empty.
///
/// WIDTH mode (any candidate has a `w` descriptor): the effective width is
/// `sizes_width(sizes, vp)` (else `vp.width`); `target = eff * dpr`. Choose the
/// candidate with `width >= target` having the smallest width; if none reaches
/// the target, the largest width.
///
/// DENSITY mode (otherwise): missing density counts as 1.0. Choose density
/// `>= dpr` with the smallest density; if none, the largest density.
fn select_candidate(
    cands: Vec<Candidate>,
    sizes: Option<&str>,
    vp: Viewport,
    dpr: f32,
) -> Option<String> {
    if cands.is_empty() {
        return None;
    }
    let width_mode = cands.iter().any(|c| c.width.is_some());
    if width_mode {
        let eff = sizes.and_then(|s| sizes_width(s, vp)).unwrap_or(vp.width);
        let target = eff * dpr;
        // Candidates without a `w` descriptor have no width → ignored here.
        let widthed: Vec<&Candidate> = cands.iter().filter(|c| c.width.is_some()).collect();
        let above = widthed
            .iter()
            .filter(|c| c.width.unwrap() as f32 >= target)
            .min_by_key(|c| c.width.unwrap());
        let chosen = above.or_else(|| widthed.iter().max_by_key(|c| c.width.unwrap()));
        chosen.map(|c| c.url.clone())
    } else {
        let density = |c: &Candidate| c.density.unwrap_or(1.0);
        let above = cands
            .iter()
            .filter(|c| density(c) >= dpr)
            .min_by(|a, b| density(a).partial_cmp(&density(b)).unwrap());
        let chosen = above.or_else(|| {
            cands
                .iter()
                .max_by(|a, b| density(a).partial_cmp(&density(b)).unwrap())
        });
        chosen.map(|c| c.url.clone())
    }
}

/// Whether a `<source type>` names an image MIME this engine can decode.
fn is_supported_image_mime(t: &str) -> bool {
    matches!(
        t.trim().to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

/// If `img_id` is inside a `<picture>`, walk the `<source>` children BEFORE it
/// in document order; the first source that (a) has a supported/absent `type`,
/// (b) has a matching/absent `media`, and (c) yields a candidate from its
/// `srcset` wins. Returns `None` if `img_id` is not under `<picture>` or no
/// source produced a url.
fn picture_source_url(doc: &Document, img_id: NodeId, vp: Viewport, dpr: f32) -> Option<String> {
    let parent = doc.parent(img_id)?;
    if doc.tag_name(parent) != Some("picture") {
        return None;
    }
    for child in doc.children(parent) {
        if child == img_id {
            break; // only sources BEFORE the <img> apply.
        }
        if doc.tag_name(child) != Some("source") {
            continue;
        }
        if let Some(t) = doc.get_attribute(child, "type") {
            if !is_supported_image_mime(t) {
                continue;
            }
        }
        if let Some(m) = doc.get_attribute(child, "media") {
            if !media_matches(&parse_media_query(m), vp) {
                continue;
            }
        }
        let Some(srcset) = doc.get_attribute(child, "srcset") else {
            continue; // a matching <source> with no srcset → try the next.
        };
        let sizes = doc.get_attribute(child, "sizes");
        if let Some(url) = select_candidate(parse_srcset(srcset), sizes, vp, dpr) {
            return Some(url);
        }
        // Matching source but no candidate → continue to the next source.
    }
    None
}

/// Resolve the url an `<img>` should decode + blit, for `vp` + `dpr`:
/// 1. a winning `<picture>`/`<source>` url, else
/// 2. the `<img srcset>` best candidate, else
/// 3. the raw `<img src>` (byte-identical fallback; `None` if absent → no box).
///
/// Used identically by paint's `decode_images` and the box tree's `<img>`
/// branch, so the decoded url == the blitted url.
pub fn resolve_img_src(doc: &Document, img_id: NodeId, vp: Viewport, dpr: f32) -> Option<String> {
    if let Some(url) = picture_source_url(doc, img_id, vp, dpr) {
        return Some(url);
    }
    if let Some(srcset) = doc.get_attribute(img_id, "srcset") {
        let sizes = doc.get_attribute(img_id, "sizes");
        if let Some(url) = select_candidate(parse_srcset(srcset), sizes, vp, dpr) {
            return Some(url);
        }
    }
    doc.get_attribute(img_id, "src").map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: f32) -> Viewport {
        Viewport::from_width(w)
    }

    #[test]
    fn parse_srcset_density_width_bare() {
        let c = parse_srcset("a.png 1x, b.png 2x");
        assert_eq!(
            c[0],
            Candidate {
                url: "a.png".into(),
                density: Some(1.0),
                width: None
            }
        );
        assert_eq!(
            c[1],
            Candidate {
                url: "b.png".into(),
                density: Some(2.0),
                width: None
            }
        );

        let c = parse_srcset("s.png 320w, l.png 640w");
        assert_eq!(
            c[0],
            Candidate {
                url: "s.png".into(),
                density: None,
                width: Some(320)
            }
        );
        assert_eq!(
            c[1],
            Candidate {
                url: "l.png".into(),
                density: None,
                width: Some(640)
            }
        );

        // bare url (no descriptor) → 1x.
        let c = parse_srcset("only.png");
        assert_eq!(
            c[0],
            Candidate {
                url: "only.png".into(),
                density: None,
                width: None
            }
        );

        // malformed descriptor → treated as 1x, not skipped.
        let c = parse_srcset("x.png foo, , y.png 0x");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "x.png");
        assert_eq!((c[0].density, c[0].width), (None, None));
        assert_eq!((c[1].density, c[1].width), (None, None));
    }

    #[test]
    fn select_candidate_density() {
        let cands = || parse_srcset("a.png 1x, b.png 2x");
        // dpr 1 → smallest density >= 1 → 1x.
        assert_eq!(
            select_candidate(cands(), None, vp(800.0), 1.0).as_deref(),
            Some("a.png")
        );
        // dpr 2 → smallest density >= 2 → 2x.
        assert_eq!(
            select_candidate(cands(), None, vp(800.0), 2.0).as_deref(),
            Some("b.png")
        );
        // dpr 3 → none >= 3 → largest density (2x).
        assert_eq!(
            select_candidate(cands(), None, vp(800.0), 3.0).as_deref(),
            Some("b.png")
        );
    }

    #[test]
    fn select_candidate_width_with_sizes() {
        let cands = || parse_srcset("s.png 300w, m.png 500w, l.png 640w");
        let sizes = "(max-width:600px) 100vw, 50vw";
        // vp 500: media matches → 100vw = 500; target 500*1 = 500 → smallest >= 500 = 500 (m).
        assert_eq!(
            select_candidate(cands(), Some(sizes), vp(500.0), 1.0).as_deref(),
            Some("m.png")
        );
        // vp 800: media (max-width:600) fails → default 50vw = 400; target 400 →
        // smallest >= 400 = 500 (m).
        assert_eq!(
            select_candidate(cands(), Some(sizes), vp(800.0), 1.0).as_deref(),
            Some("m.png")
        );
        // no sizes → eff = vp.width.
        // vp 280: target 280 → smallest >= 280 = 300 (s).
        assert_eq!(
            select_candidate(cands(), None, vp(280.0), 1.0).as_deref(),
            Some("s.png")
        );
    }

    #[test]
    fn select_candidate_empty() {
        assert_eq!(select_candidate(Vec::new(), None, vp(800.0), 1.0), None);
    }

    #[test]
    fn sizes_width_parsing() {
        let v = vp(1000.0);
        // first matching condition wins.
        assert_eq!(sizes_width("(max-width:600px) 100vw, 50vw", v), Some(500.0)); // default 50vw.
        assert_eq!(sizes_width("(min-width:600px) 25vw, 50vw", v), Some(250.0)); // first matches.
                                                                                 // bare default.
        assert_eq!(sizes_width("300px", v), Some(300.0));
        // percent against width.
        assert_eq!(sizes_width("40%", v), Some(400.0));
        // unparsable → None.
        assert_eq!(sizes_width("auto", v), None);
    }

    #[test]
    fn is_supported_image_mime_basic() {
        assert!(is_supported_image_mime("image/png"));
        assert!(is_supported_image_mime("  IMAGE/JPEG "));
        assert!(is_supported_image_mime("image/webp"));
        assert!(!is_supported_image_mime("image/avif"));
        assert!(!is_supported_image_mime("text/html"));
    }
}
