//! The cascade (§4): collect matching declarations, order by precedence, apply.

use starfish_css::{Declaration, PseudoElement, Specificity, Stylesheet};
use starfish_dom::{Document, NodeId};

use crate::computed::{ComputedStyle, Content};
use crate::matching::matches;
use crate::properties::{apply_declaration, resolve_content, EmContext};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    UserAgent,
    Author,
}

struct MatchedDecl<'a> {
    origin: Origin,
    /// Declaration came from the element's inline `style=""` attribute. Inline
    /// style outranks any author selector (normal declarations), so it sorts
    /// above non-inline author declarations regardless of specificity.
    inline: bool,
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
                // A pseudo-element selector (`div::before`) applies to the
                // pseudo, not the element — those declarations are cascaded
                // separately by `cascade_pseudo`. Skip them here so they don't
                // leak onto the originating element.
                if sel.pseudo_element().is_none() && matches(doc, element, sel) {
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
                    inline: false,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // Inline `style=""` attribute: parsed as one Author rule whose declarations
    // outrank every author selector (normal declarations). Held in a local so
    // its declarations can be borrowed for the lifetime of the cascade. `!important`
    // inside the inline style is honored via `decl.important` + `origin_rank`.
    let inline_sheet = doc
        .get_attribute(element, "style")
        .filter(|s| !s.trim().is_empty())
        .map(|s| starfish_css::parse_stylesheet(&format!("*{{{s}}}")));
    if let Some(sheet) = &inline_sheet {
        if let Some(rule) = sheet.rules.first() {
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: Origin::Author,
                    inline: true,
                    specificity: Specificity { a: 0, b: 0, c: 0 },
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // Ascending sort: applied first → last wins (stable preserves source order
    // already encoded, but key includes it explicitly). `inline` orders just
    // above non-inline within the same origin/importance rank.
    matched.sort_by_key(|m| {
        (
            origin_rank(m.origin, m.declaration.important),
            m.inline,
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

/// Cascade the `::before`/`::after` pseudo-element of `element` for one `side`
/// (E7-M2). Only rules whose subject selector targets this pseudo-element side
/// (and that match `element`) are considered. The pseudo is a child of the
/// element, so it inherits from `element_style`. Returns `Some((style, text))`
/// iff a box-generating `content` (a `Text`, including `""`) was resolved;
/// `None` for no matching rule, no `content`, or `content:none/normal`.
pub(crate) fn cascade_pseudo(
    doc: &Document,
    element: NodeId,
    side: PseudoElement,
    element_style: &ComputedStyle,
    sheets: &[(Origin, &Stylesheet)],
    ctx: EmContext,
) -> Option<(ComputedStyle, String)> {
    let mut matched: Vec<MatchedDecl> = Vec::new();
    let mut source_order = 0usize;

    for (origin, sheet) in sheets {
        for rule in &sheet.rules {
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if sel.pseudo_element() == Some(side) && matches(doc, element, sel) {
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
                    inline: false,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }
    if matched.is_empty() {
        return None; // no ::side rule at all → no box.
    }

    // Base = inherit from the originating element (the pseudo is its child). The
    // pseudo's `em` basis is the element's font-size (the pseudo inherits it).
    let mut style = element_style.inherit_from();
    let pctx = EmContext {
        parent_font_size: element_style.font_size,
        root_font_size: ctx.root_font_size,
    };

    matched.sort_by_key(|m| {
        (
            origin_rank(m.origin, m.declaration.important),
            m.inline,
            m.specificity,
            m.source_order,
        )
    });

    // font-size first (so `em` resolves), then the rest; `content` resolved from
    // the winning (last) `content` declaration.
    let mut content = Content::Normal;
    let mut border_color_set = false;
    for m in matched.iter().filter(|m| m.declaration.name == "font-size") {
        apply_declaration(&mut style, m.declaration, pctx);
    }
    for m in matched.iter().filter(|m| m.declaration.name != "font-size") {
        if m.declaration.name == "content" {
            content = resolve_content(doc, element, m.declaration);
        } else if apply_declaration(&mut style, m.declaration, pctx) {
            border_color_set = true;
        }
    }
    if !border_color_set {
        style.border_color = style.color;
    }

    match content {
        Content::Text(s) => Some((style, s)),
        Content::None | Content::Normal => None,
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

    /// A `::before` rule's declarations apply to the pseudo, NOT the element —
    /// the general cascade must skip pseudo-element selectors.
    #[test]
    fn pseudo_element_rule_does_not_leak_to_element() {
        let doc = parse("<p>x</p>");
        let p = find_p(&doc);
        let author =
            parse_stylesheet("p { color: #000000 } p::before { color: #ff0000; content: \"y\" }");
        let sheets = [(Origin::Author, &author)];
        let ctx = EmContext { parent_font_size: 16.0, root_font_size: 16.0 };
        let mut style = ComputedStyle::initial();
        cascade(&doc, p, &sheets, ctx, &mut style);
        // The element keeps black; the red is the pseudo's, not the element's.
        assert_eq!(style.color, Rgba { r: 0, g: 0, b: 0, a: 255 });
    }

    fn find_p(doc: &Document) -> NodeId {
        let mut stack = doc.children(doc.root());
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some("p") {
                return n;
            }
            stack.extend(doc.children(n));
        }
        panic!("<p>");
    }

    /// Inline `style=""` declarations beat an author selector rule.
    #[test]
    fn inline_style_beats_author() {
        let doc = parse("<p style='color:#00ff00'>x</p>");
        let p = find_p(&doc);
        let author = parse_stylesheet("p { color: #ff0000 }");
        let sheets = [(Origin::Author, &author)];
        let ctx = EmContext { parent_font_size: 16.0, root_font_size: 16.0 };
        let mut style = ComputedStyle::initial();
        cascade(&doc, p, &sheets, ctx, &mut style);
        assert_eq!(style.color, Rgba { r: 0, g: 255, b: 0, a: 255 });
    }

    /// An author `!important` still beats a normal inline declaration.
    #[test]
    fn author_important_beats_inline_normal() {
        let doc = parse("<p style='color:#00ff00'>x</p>");
        let p = find_p(&doc);
        let author = parse_stylesheet("p { color: #ff0000 !important }");
        let sheets = [(Origin::Author, &author)];
        let ctx = EmContext { parent_font_size: 16.0, root_font_size: 16.0 };
        let mut style = ComputedStyle::initial();
        cascade(&doc, p, &sheets, ctx, &mut style);
        assert_eq!(style.color, Rgba { r: 255, g: 0, b: 0, a: 255 });
    }
}

