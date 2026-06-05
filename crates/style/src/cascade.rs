//! The cascade (§4): collect matching declarations, order by precedence, apply.

use starfish_css::{Declaration, Specificity, Stylesheet};
use starfish_dom::{Document, NodeId};

use crate::computed::ComputedStyle;
use crate::matching::matches;
use crate::properties::{apply_declaration, EmContext};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    UserAgent,
    Author,
}

struct MatchedDecl<'a> {
    origin: Origin,
    specificity: Specificity,
    source_order: usize,
    declaration: &'a Declaration,
}

/// Origin × importance rank, low→high precedence (§4.3 step 1):
/// UA-normal < Author-normal < Author-important < UA-important.
fn origin_rank(origin: Origin, important: bool) -> u8 {
    match (origin, important) {
        (Origin::UserAgent, false) => 0,
        (Origin::Author, false) => 1,
        (Origin::Author, true) => 2,
        (Origin::UserAgent, true) => 3,
    }
}

/// Cascade matching declarations from `sheets` (each tagged with an origin, in
/// precedence-base order) onto `style`, seeded by inheritance/initial.
pub(crate) fn cascade(
    doc: &Document,
    element: NodeId,
    sheets: &[(Origin, &Stylesheet)],
    ctx: EmContext,
    style: &mut ComputedStyle,
) {
    let mut matched: Vec<MatchedDecl> = Vec::new();
    let mut source_order = 0usize;

    for (origin, sheet) in sheets {
        for rule in &sheet.rules {
            // Max specificity among this rule's matching selectors.
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if matches(doc, element, sel) {
                    best = Some(match best {
                        Some(b) => b.max(sel.specificity),
                        None => sel.specificity,
                    });
                }
            }
            let Some(spec) = best else { continue };
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: *origin,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // Ascending sort: applied first → last wins (stable preserves source order
    // already encoded, but key includes it explicitly).
    matched.sort_by_key(|m| {
        (
            origin_rank(m.origin, m.declaration.important),
            m.specificity,
            m.source_order,
        )
    });

    // Apply font-size first so `em` on other lengths resolves against this
    // element's own computed font-size (§5.3).
    let mut border_color_set = false;
    for m in matched.iter().filter(|m| m.declaration.name == "font-size") {
        apply_declaration(style, m.declaration, ctx);
    }
    for m in matched.iter().filter(|m| m.declaration.name != "font-size") {
        if apply_declaration(style, m.declaration, ctx) {
            border_color_set = true;
        }
    }

    // currentColor: if no border color was explicitly given, it follows the
    // element's computed `color` (§1.3).
    if !border_color_set {
        style.border_color = style.color;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::Rgba;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;

    /// UA-important outranks author-important (origin_rank 3 > 2): when both an
    /// UA and an author sheet set the same property `!important`, UA wins.
    #[test]
    fn ua_important_beats_author_important() {
        let doc = parse("<p>x</p>");
        let p = doc
            .children(doc.root())
            .into_iter()
            .find(|n| doc.tag_name(*n) == Some("p"))
            .or_else(|| {
                // Walk to find the <p> regardless of html/body wrapping.
                let mut stack = doc.children(doc.root());
                while let Some(n) = stack.pop() {
                    if doc.tag_name(n) == Some("p") {
                        return Some(n);
                    }
                    stack.extend(doc.children(n));
                }
                None
            })
            .expect("<p>");

        let ua = parse_stylesheet("p { color: #ff0000 !important }");
        let author = parse_stylesheet("p { color: #0000ff !important }");
        let sheets = [(Origin::UserAgent, &ua), (Origin::Author, &author)];
        let ctx = EmContext { parent_font_size: 16.0, root_font_size: 16.0 };

        let mut style = ComputedStyle::initial();
        cascade(&doc, p, &sheets, ctx, &mut style);
        assert_eq!(style.color, Rgba { r: 255, g: 0, b: 0, a: 255 });
    }
}
