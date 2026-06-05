//! Selector matching against the DOM (§3). Right-to-left with backtracking.

use starfish_css::{Combinator, Compound, Selector, SelectorPart};
use starfish_dom::{Document, NodeId};

/// Whether `element` (guaranteed an Element node) matches `selector`.
pub(crate) fn matches(doc: &Document, element: NodeId, selector: &Selector) -> bool {
    // Index of the rightmost (subject) compound.
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
        SelectorPart::Combinator(_) => unreachable!("expected compound at index {i}"),
    }
}

fn combinator_at(parts: &[SelectorPart], i: usize) -> &Combinator {
    match &parts[i] {
        SelectorPart::Combinator(c) => c,
        SelectorPart::Compound(_) => unreachable!("expected combinator at index {i}"),
    }
}

/// Match `parts[..=i]` (with `parts[i]` a Compound) ending at `node`.
fn match_from(doc: &Document, node: NodeId, parts: &[SelectorPart], i: usize) -> bool {
    if !compound_matches(doc, node, compound_at(parts, i)) {
        return false;
    }
    if i == 0 {
        return true;
    }
    // parts[i-1] is always a Combinator; parts[i-2] the next Compound.
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

/// All simple selectors in `c` must hold (AND).
fn compound_matches(doc: &Document, element: NodeId, c: &Compound) -> bool {
    // tag (both already lowercased). `universal`/`None` impose no constraint.
    if let Some(t) = &c.tag {
        if doc.tag_name(element) != Some(t.as_str()) {
            return false;
        }
    }
    // ids: each requested id must equal the single id attribute.
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
    // classes: every requested class present in the space-split set.
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
