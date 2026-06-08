//! `@media` query evaluation (E13-M3). A query is a comma-separated list of
//! conditions (OR); a condition is a media type plus `and`-joined features,
//! optionally negated. Evaluated against the render [`Viewport`].

use starfish_css::{MediaCondition, MediaFeature, MediaQuery, MediaType, Orientation};

use crate::Viewport;

/// Whether `q` matches `vp`. Comma-separated conditions are OR'd; empty
/// conditions never match.
pub(crate) fn media_matches(q: &MediaQuery, vp: Viewport) -> bool {
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
        // Unknown / unmodelled feature → never matches.
        MediaFeature::Unknown => false,
    }
}
