//! starfish-style — style resolution (M3).
//!
//! Given a [`Document`] and parsed author [`Stylesheet`]s, produce a
//! [`StyledTree`]: one typed [`ComputedStyle`] per element, after selector
//! matching, the cascade, and inheritance. See `docs/design/M3-style.md`.

mod cascade;
mod computed;
mod matching;
mod properties;
mod ua;

use std::collections::HashMap;

use starfish_css::Stylesheet;
use starfish_dom::Document;

pub use computed::{
    BorderStyle, ComputedStyle, Display, FontWeight, Length, LineHeight, TextAlign,
};
pub use starfish_css::Rgba;
pub use starfish_dom::NodeId;

use cascade::{cascade, Origin};
use properties::EmContext;

/// Side table mapping each styled element to its computed style.
#[derive(Debug, Default)]
pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
}

impl StyledTree {
    /// Computed style for a node. Panics if `id` was not styled (a bug — every
    /// element is inserted during the walk).
    pub fn computed(&self, id: NodeId) -> &ComputedStyle {
        &self.styles[&id]
    }

    /// Non-panicking lookup (None for Document/Doctype/Comment/Text nodes).
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&id)
    }
}

/// Build the styled tree: walk the DOM from the root, cascade each element
/// against the UA sheet + the given author stylesheets, applying inheritance.
/// Infallible.
pub fn style_tree(doc: &Document, author_sheets: &[Stylesheet]) -> StyledTree {
    let ua = ua::ua_stylesheet();
    // Precedence-base order: UA first, then author sheets in given order.
    let mut sheets: Vec<(Origin, &Stylesheet)> = vec![(Origin::UserAgent, &ua)];
    for s in author_sheets {
        sheets.push((Origin::Author, s));
    }

    let mut tree = StyledTree::default();
    let parent_initial = ComputedStyle::initial();
    let root_font_size = parent_initial.font_size;

    // Pre-order DFS from the document root over element subtrees.
    let mut root_fs = root_font_size;
    for child in doc.children(doc.root()) {
        style_node(doc, child, &parent_initial, &sheets, &mut root_fs, &mut tree);
    }
    tree
}

fn style_node(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    sheets: &[(Origin, &Stylesheet)],
    root_font_size: &mut f32,
    tree: &mut StyledTree,
) {
    // Only element nodes are styled; descend through their children.
    if doc.tag_name(node).is_none() {
        return;
    }

    let mut style = parent_style.inherit_from();
    let ctx = EmContext {
        parent_font_size: parent_style.font_size,
        root_font_size: *root_font_size,
    };
    cascade(doc, node, sheets, ctx, &mut style);

    // The first styled element (the root element, e.g. <html>) defines `rem`.
    if doc.tag_name(node) == Some("html") {
        *root_font_size = style.font_size;
    }

    for child in doc.children(node) {
        style_node(doc, child, &style, sheets, root_font_size, tree);
    }

    tree.styles.insert(node, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_dom::NodeKind;
    use starfish_html::parse;

    /// Find the first element with the given tag in document order.
    fn find(doc: &Document, tag: &str) -> NodeId {
        find_opt(doc, tag).unwrap_or_else(|| panic!("no <{tag}> found"))
    }

    fn find_opt(doc: &Document, tag: &str) -> Option<NodeId> {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some(tag) {
                return Some(n);
            }
            // push children (reverse for document order isn't required here)
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        None
    }

    /// Find the first element with a given id.
    fn find_id(doc: &Document, id: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.get_attribute(n, "id") == Some(id) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no element with id={id}")
    }

    fn red() -> Rgba {
        Rgba { r: 255, g: 0, b: 0, a: 255 }
    }
    fn blue() -> Rgba {
        Rgba { r: 0, g: 0, b: 255, a: 255 }
    }
    fn green() -> Rgba {
        Rgba { r: 0, g: 128, b: 0, a: 255 }
    }
    fn black() -> Rgba {
        Rgba { r: 0, g: 0, b: 0, a: 255 }
    }

    fn style(html: &str, css: &str) -> (Document, StyledTree) {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let tree = style_tree(&doc, &[sheet]);
        (doc, tree)
    }

    // --- §9.1 selector matching (exercised end-to-end via the cascade) ---

    #[test]
    fn match_tag() {
        let (doc, t) = style("<p>hi</p>", "p { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_class() {
        let (doc, t) = style("<p class='x'>hi</p>", ".x { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_id() {
        let (doc, t) = style("<p id='m'>hi</p>", "#m { color: red }");
        assert_eq!(t.computed(find_id(&doc, "m")).color, red());
    }

    #[test]
    fn match_compound() {
        let (doc, t) = style(
            "<p class='a b' id='m'>hi</p>",
            "p.a.b#m { color: red } p.a.d { color: blue }",
        );
        // p.a.b#m matches; p.a.d does not.
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_multiclass_subset() {
        let (doc, t) = style("<p class='a b c'>hi</p>", ".a.c { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
        let (doc2, t2) = style("<p class='a b c'>hi</p>", ".a.d { color: red }");
        // .a.d does not match → stays initial black.
        assert_eq!(t2.computed(find(&doc2, "p")).color, black());
    }

    #[test]
    fn match_descendant() {
        let (doc, t) = style(
            "<div><section><p>hi</p></section></div>",
            "div p { color: red }",
        );
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn no_match_descendant_without_ancestor() {
        let (doc, t) = style("<section><p>hi</p></section>", "div p { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, black());
    }

    #[test]
    fn descendant_backtracking() {
        // Selector `x > .b .c`. DOM has two `.b` ancestors of `.c`: the nearest
        // (inner) `.b` is NOT a direct child of `x`, so a greedy matcher that
        // commits to it for the `x > .b` segment would fail. A correct matcher
        // must backtrack to the outer `.b`, which IS `x`'s child → matches.
        let html = "<x><div class='b'><div class='wrap'>\
            <div class='b'><div class='c'>z</div></div>\
            </div></div></x>";
        let (doc, t) = style(html, "x > .b .c { color: red }");
        assert_eq!(t.computed(find_class(&doc, "c")).color, red());
    }

    #[test]
    fn descendant_backtracking_negative() {
        // Same shape as `descendant_backtracking` but the outer `.b` no longer
        // has an `x` parent, so no `.b` ancestor of `.c` is a child of `x`.
        // Even with backtracking, the selector must NOT match.
        let html = "<div class='b'><div class='wrap'>\
            <div class='b'><div class='c'>z</div></div>\
            </div></div>";
        let (doc, t) = style(html, "x > .b .c { color: red }");
        assert_eq!(t.computed(find_class(&doc, "c")).color, black());
    }

    fn find_class(doc: &Document, cls: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(c) = doc.get_attribute(n, "class") {
                if c.split_ascii_whitespace().any(|x| x == cls) {
                    return n;
                }
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no .{cls}")
    }

    #[test]
    fn child_combinator() {
        // div > p matches direct child only.
        let (doc, t) = style(
            "<div><p id='direct'>a</p><section><p id='grand'>b</p></section></div>",
            "div > p { color: red }",
        );
        assert_eq!(t.computed(find_id(&doc, "direct")).color, red());
        assert_eq!(t.computed(find_id(&doc, "grand")).color, black());
    }

    #[test]
    fn rule_max_specificity_over_selectors() {
        // A rule with selectors `p` and `p.win` — its specificity for a matching
        // .win element is the max, so it beats a competing lower-spec rule that
        // comes later in source order.
        let (doc, t) = style(
            "<p class='win'>x</p>",
            "p, p.win { color: red } p { color: blue }",
        );
        // first rule (max spec 0,1,1) beats later plain `p` (0,0,1).
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    // --- §9.2 cascade ---

    #[test]
    fn source_order_tiebreak() {
        let (doc, t) = style("<p>x</p>", "p { color: red } p { color: blue }");
        assert_eq!(t.computed(find(&doc, "p")).color, blue());
    }

    #[test]
    fn specificity_beats_order() {
        let (doc, t) = style(
            "<p id='m' class='c'>x</p>",
            "#m { color: red } .c { color: blue } p { color: green }",
        );
        // #id (1,0,0) wins regardless of order.
        assert_eq!(t.computed(find_id(&doc, "m")).color, red());
    }

    #[test]
    fn important_beats_higher_specificity() {
        let (doc, t) = style(
            "<p id='m' class='c'>x</p>",
            "#m { color: red } .c { color: blue !important }",
        );
        assert_eq!(t.computed(find_id(&doc, "m")).color, blue());
    }

    #[test]
    fn explicit_border_color_not_clobbered_by_current_color() {
        // color and border-color differ; the currentColor fallback must NOT
        // overwrite the explicitly-set border-color.
        let (doc, t) = style(
            "<p>x</p>",
            "p { color: red; border-color: blue }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.color, red());
        assert_eq!(p.border_color, blue());
    }

    #[test]
    fn author_overrides_ua_display() {
        let (doc, t) = style("<div>x</div>", "div { display: inline }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Inline);
    }

    // --- §9.3 inheritance ---

    #[test]
    fn color_inherits() {
        let (doc, t) = style(
            "<body><div><span>hi</span></div></body>",
            "body { color: red }",
        );
        assert_eq!(t.computed(find(&doc, "span")).color, red());
    }

    #[test]
    fn background_not_inherited() {
        let (doc, t) = style(
            "<body><span>hi</span></body>",
            "body { background-color: red }",
        );
        let transparent = Rgba { r: 0, g: 0, b: 0, a: 0 };
        assert_eq!(t.computed(find(&doc, "span")).background_color, transparent);
    }

    #[test]
    fn font_size_inherits_and_em_resolves() {
        let (doc, t) = style(
            "<div><p>hi</p></div>",
            "div { font-size: 20px } p { margin: 2em }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.font_size, 20.0);
        // 2em against p's own font-size (20) = 40.
        assert_eq!(p.margin_top, Length::Px(40.0));
    }

    #[test]
    fn font_size_percent_resolves_against_parent() {
        let (doc, t) = style(
            "<div><p>hi</p></div>",
            "div { font-size: 20px } p { font-size: 150% }",
        );
        assert_eq!(t.computed(find(&doc, "p")).font_size, 30.0);
    }

    #[test]
    fn rem_resolves_against_root() {
        let (doc, t) = style(
            "<html><body><p>hi</p></body></html>",
            "html { font-size: 10px } p { width: 3rem }",
        );
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(30.0));
    }

    // --- §9.4 property parsing ---

    #[test]
    fn margin_shorthand_forms() {
        let (doc, t) = style("<p>x</p>", "p { margin: 10px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.margin_top, Length::Px(10.0));
        assert_eq!(p.margin_left, Length::Px(10.0));

        let (doc2, t2) = style("<p>x</p>", "p { margin: 1px 2px }");
        let p2 = t2.computed(find(&doc2, "p"));
        assert_eq!(p2.margin_top, Length::Px(1.0));
        assert_eq!(p2.margin_bottom, Length::Px(1.0));
        assert_eq!(p2.margin_right, Length::Px(2.0));
        assert_eq!(p2.margin_left, Length::Px(2.0));

        let (doc3, t3) = style("<p>x</p>", "p { margin: 1px 2px 3px 4px }");
        let p3 = t3.computed(find(&doc3, "p"));
        assert_eq!(p3.margin_top, Length::Px(1.0));
        assert_eq!(p3.margin_right, Length::Px(2.0));
        assert_eq!(p3.margin_bottom, Length::Px(3.0));
        assert_eq!(p3.margin_left, Length::Px(4.0));
    }

    #[test]
    fn padding_auto_becomes_zero() {
        let (doc, t) = style("<p>x</p>", "p { padding: auto }");
        assert_eq!(t.computed(find(&doc, "p")).padding_top, Length::Px(0.0));
    }

    #[test]
    fn width_percent_auto_zero() {
        let (doc, t) = style("<p>x</p>", "p { width: 50% }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Percent(50.0));
        let (doc2, t2) = style("<p>x</p>", "p { width: auto }");
        assert_eq!(t2.computed(find(&doc2, "p")).width, Length::Auto);
        let (doc3, t3) = style("<p>x</p>", "p { height: 0 }");
        assert_eq!(t3.computed(find(&doc3, "p")).height, Length::Px(0.0));
    }

    #[test]
    fn border_shorthand() {
        let (doc, t) = style("<p>x</p>", "p { border: 2px solid red }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.border_top_width, 2.0);
        assert_eq!(p.border_left_width, 2.0);
        assert_eq!(p.border_style, BorderStyle::Solid);
        assert_eq!(p.border_color, red());
    }

    #[test]
    fn font_weight_values() {
        let (doc, t) = style("<p>x</p>", "p { font-weight: bold }");
        assert_eq!(t.computed(find(&doc, "p")).font_weight, FontWeight(700));
        let (doc2, t2) = style("<p>x</p>", "p { font-weight: 300 }");
        assert_eq!(t2.computed(find(&doc2, "p")).font_weight, FontWeight(300));
    }

    #[test]
    fn line_height_forms() {
        let (doc, t) = style("<p>x</p>", "p { line-height: 1.5 }");
        assert_eq!(t.computed(find(&doc, "p")).line_height, LineHeight::Number(1.5));
        let (doc2, t2) = style("<p>x</p>", "p { line-height: 20px }");
        assert_eq!(t2.computed(find(&doc2, "p")).line_height, LineHeight::Px(20.0));
        let (doc3, t3) = style("<p>x</p>", "p { line-height: normal }");
        assert_eq!(t3.computed(find(&doc3, "p")).line_height, LineHeight::Normal);
    }

    #[test]
    fn font_family_list() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { font-family: \"Helvetica Neue\", Arial, sans-serif }",
        );
        assert_eq!(
            t.computed(find(&doc, "p")).font_family,
            vec!["Helvetica Neue", "Arial", "sans-serif"]
        );
    }

    #[test]
    fn color_hex_and_transparent_bg() {
        let (doc, t) = style("<p>x</p>", "p { color: #00ff00 }");
        assert_eq!(
            t.computed(find(&doc, "p")).color,
            Rgba { r: 0, g: 255, b: 0, a: 255 }
        );
        let (doc2, t2) = style("<p>x</p>", "p { background-color: transparent }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).background_color,
            Rgba { r: 0, g: 0, b: 0, a: 0 }
        );
    }

    #[test]
    fn unknown_and_bogus_ignored() {
        let (doc, t) = style("<p>x</p>", "p { zoom: 2; width: bogus }");
        // width unchanged from initial Auto; no panic.
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    // --- §9.5 UA sheet + smoke ---

    #[test]
    fn ua_display_defaults() {
        let (doc, t) = style("<div><span>hi</span></div>", "");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Block);
        assert_eq!(t.computed(find(&doc, "span")).display, Display::Inline);
        // both inherit black + 16px.
        assert_eq!(t.computed(find(&doc, "span")).color, black());
        assert_eq!(t.computed(find(&doc, "span")).font_size, 16.0);
    }

    #[test]
    fn ua_head_none() {
        let doc = parse("<html><head><title>t</title></head><body>x</body></html>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "head")).display, Display::None);
        if let Some(title) = find_opt(&doc, "title") {
            assert_eq!(t.computed(title).display, Display::None);
        }
    }

    #[test]
    fn ua_body_margin() {
        let doc = parse("<body>x</body>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "body")).margin_top, Length::Px(8.0));
    }

    #[test]
    fn ua_p_margin_with_author_color() {
        let (doc, t) = style("<p>hi</p>", "p { color: blue }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.display, Display::Block);
        // UA margin 16px 0 retained.
        assert_eq!(p.margin_top, Length::Px(16.0));
        assert_eq!(p.margin_left, Length::Px(0.0));
        assert_eq!(p.color, blue());
    }

    #[test]
    fn ua_bold_for_strong() {
        let (doc, t) = style("<strong>x</strong>", "");
        assert_eq!(t.computed(find(&doc, "strong")).font_weight, FontWeight(700));
    }

    #[test]
    fn text_node_has_no_entry() {
        let (doc, t) = style("<p>hi</p>", "");
        let p = find(&doc, "p");
        let text = doc
            .children(p)
            .into_iter()
            .find(|c| matches!(doc.kind(*c), NodeKind::Text(_)))
            .expect("text node");
        assert!(t.get(text).is_none());
    }

    #[test]
    fn smoke_small_page() {
        let html = "<html><body>\
            <h1 id='title'>Hello</h1>\
            <div class='box'><p>Para</p><ul><li><a href='#'>link</a></li></ul></div>\
            </body></html>";
        let css = "body { font-family: Arial } \
                   .box { background-color: #ff0000; border: 1px solid #0000ff } \
                   p { color: green } a { color: blue }";
        let (doc, t) = style(html, css);

        let h1 = t.computed(find_id(&doc, "title"));
        assert_eq!(h1.display, Display::Block);
        assert_eq!(h1.font_size, 32.0); // UA h1
        assert_eq!(h1.font_weight, FontWeight(700));

        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.color, green());
        assert_eq!(p.display, Display::Block);
        assert_eq!(p.font_family, vec!["Arial"]); // inherited from body

        let box_el = find_class(&doc, "box");
        let b = t.computed(box_el);
        assert_eq!(b.background_color, red());
        assert_eq!(b.border_top_width, 1.0);
        assert_eq!(b.border_style, BorderStyle::Solid);
        assert_eq!(b.border_color, blue());

        let a = t.computed(find(&doc, "a"));
        assert_eq!(a.display, Display::Inline);
        assert_eq!(a.color, blue());

        let ul = t.computed(find(&doc, "ul"));
        assert_eq!(ul.padding_left, Length::Px(40.0));
    }
}
