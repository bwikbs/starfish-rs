//! `querySelector` support: parse a selector string via `starfish-css` (the
//! `sel{}` throwaway-stylesheet trick), then a tiny right-to-left, backtracking
//! matcher mirroring `starfish-style::matching` over the same selector subset
//! (tag / id / class / compound / descendant / child). Ported (not depended on)
//! to keep this leaf binding decoupled from the style engine — see E4-M2 §5.2.

use starfish_css::{Combinator, Compound, Selector, SelectorPart};
use starfish_dom::{Document, NodeId};

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

/// Whether `element` (an Element node) matches `selector`.
pub(crate) fn matches_selector(doc: &Document, element: NodeId, selector: &Selector) -> bool {
    match last_compound_index(&selector.parts) {
        Some(i) => match_from(doc, element, &selector.parts, i),
        None => false,
    }
}

fn last_compound_index(parts: &[SelectorPart]) -> Option<usize> {
    parts
        .iter()
        .rposition(|p| matches!(p, SelectorPart::Compound(_)))
}

fn compound_at(parts: &[SelectorPart], i: usize) -> &Compound {
    match &parts[i] {
        SelectorPart::Compound(c) => c,
        SelectorPart::Combinator(_) => unreachable!("expected compound at {i}"),
    }
}

fn combinator_at(parts: &[SelectorPart], i: usize) -> &Combinator {
    match &parts[i] {
        SelectorPart::Combinator(c) => c,
        SelectorPart::Compound(_) => unreachable!("expected combinator at {i}"),
    }
}

fn match_from(doc: &Document, node: NodeId, parts: &[SelectorPart], i: usize) -> bool {
    if !compound_matches(doc, node, compound_at(parts, i)) {
        return false;
    }
    if i == 0 {
        return true;
    }
    let comb = combinator_at(parts, i - 1);
    let left = i - 2;
    match comb {
        Combinator::Child => doc
            .parent(node)
            .filter(|p| doc.tag_name(*p).is_some())
            .is_some_and(|p| match_from(doc, p, parts, left)),
        Combinator::Descendant => {
            let mut anc = doc.parent(node);
            while let Some(a) = anc {
                if doc.tag_name(a).is_some() && match_from(doc, a, parts, left) {
                    return true;
                }
                anc = doc.parent(a);
            }
            false
        }
    }
}

fn compound_matches(doc: &Document, element: NodeId, c: &Compound) -> bool {
    if let Some(t) = &c.tag {
        if doc.tag_name(element) != Some(t.as_str()) {
            return false;
        }
    }
    if !c.ids.is_empty() {
        match doc.get_attribute(element, "id") {
            Some(id) => {
                if c.ids.iter().any(|want| want != id) {
                    return false;
                }
            }
            None => return false,
        }
    }
    if !c.classes.is_empty() {
        let attr = doc.get_attribute(element, "class").unwrap_or("");
        for want in &c.classes {
            if !attr.split_ascii_whitespace().any(|cls| cls == want) {
                return false;
            }
        }
    }
    true
}
