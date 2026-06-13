//! `@media` query evaluation (E13-M3). A query is a comma-separated list of
//! conditions (OR); a condition is a media type plus `and`-joined features,
//! optionally negated. Evaluated against the render [`Viewport`].

use starfish_css::{
    MediaCondition, MediaFeature, MediaQuery, MediaType, Orientation, RangeAxis, RangeFeature,
};

use crate::Viewport;

/// Whether `q` matches `vp`. Comma-separated conditions are OR'd; empty
/// conditions never match.
pub fn media_matches(q: &MediaQuery, vp: Viewport) -> bool {
    q.conditions.iter().any(|c| condition_matches(c, vp))
}

/// One condition: the media type must apply (screen-only engine: All/Screen →
/// true, Print → false) AND every feature must match; then `not` flips it.
fn condition_matches(c: &MediaCondition, vp: Viewport) -> bool {
    let type_ok = match c.media_type {
        MediaType::All | MediaType::Screen => true,
        MediaType::Print => false,
    };
    let ok = type_ok && c.features.iter().all(|f| feature_matches(f, vp));
    if c.negated {
        !ok
    } else {
        ok
    }
}

fn feature_matches(f: &MediaFeature, vp: Viewport) -> bool {
    match f {
        MediaFeature::MinWidth(v) => vp.width >= *v,
        MediaFeature::MaxWidth(v) => vp.width <= *v,
        MediaFeature::MinHeight(v) => vp.height >= *v,
        MediaFeature::MaxHeight(v) => vp.height <= *v,
        MediaFeature::Orientation(Orientation::Portrait) => vp.height > vp.width,
        MediaFeature::Orientation(Orientation::Landscape) => vp.width >= vp.height,
        // E27-M1 range syntax.
        MediaFeature::Range(r) => range_matches(r, vp),
        // Unknown / unmodelled feature → never matches.
        MediaFeature::Unknown => false,
    }
}

/// Evaluate a range feature against the viewport (E27-M1).
fn range_matches(r: &RangeFeature, vp: Viewport) -> bool {
    let v = match r.axis {
        RangeAxis::Width => vp.width,
        RangeAxis::Height => vp.height,
    };
    let lo_ok = r.lower.is_none_or(|b| {
        if b.inclusive {
            v >= b.value
        } else {
            v > b.value
        }
    });
    let hi_ok = r.upper.is_none_or(|b| {
        if b.inclusive {
            v <= b.value
        } else {
            v < b.value
        }
    });
    lo_ok && hi_ok
}
