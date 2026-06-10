//! `@supports` condition evaluation (E24-M2).

use starfish_css::SupportsCondition;

use crate::properties::declaration_supported;
use crate::Viewport;

/// Whether a `@supports` condition holds. `Unknown` never matches.
///
/// NOTE: this deviates from CSS's 3-valued logic for negated unknowns — the
/// spec keeps `not <general-enclosed>` false, while here `Not(Unknown)`
/// evaluates to true. Acceptable for the supported condition subset.
pub(crate) fn supports_matches(cond: &SupportsCondition, vp: Viewport) -> bool {
    match cond {
        SupportsCondition::Decl(d) => declaration_supported(d, vp),
        SupportsCondition::Not(inner) => !supports_matches(inner, vp),
        SupportsCondition::And(list) => list.iter().all(|c| supports_matches(c, vp)),
        SupportsCondition::Or(list) => list.iter().any(|c| supports_matches(c, vp)),
        SupportsCondition::Unknown => false,
    }
}
