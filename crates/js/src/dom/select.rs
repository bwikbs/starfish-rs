//! `querySelector` support: parse a selector string via `starfish-css` (the
//! `sel{}` throwaway-stylesheet trick), then match via the *shared*
//! `starfish_style::matches` so `querySelector` and the cascade stay at exact
//! parity over the full selector subset (E7-M1 §6).

use starfish_css::Selector;

/// Parse a selector string into a selector list. `None` for an empty,
/// unsupported, or invalid selector (the css parser drops it) — the JS layer
/// then returns `null` / an empty list (non-fatal; no `SyntaxError`).
pub(crate) fn parse_selector_list(sel: &str) -> Option<Vec<Selector>> {
    let sheet = starfish_css::parse_stylesheet(&format!("{sel}{{}}"));
    let rule = sheet.rules.into_iter().next()?;
    if rule.selectors.is_empty() {
        None
    } else {
        Some(rule.selectors)
    }
}

/// Whether `element` (an Element node) matches `selector`. Delegates to the
/// shared cascade matcher for parity.
pub(crate) use starfish_style::matches as matches_selector;
