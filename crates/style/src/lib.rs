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
    AlignItems, AlignSelf, Background, BorderStyle, BoxShadow, Clear, ComputedStyle, Direction,
    Display, FlexDirection, FlexWrap, Float, FontStyle, FontWeight, GradientStop, GridLine,
    GridPlacement, JustifyContent, Length, LengthPct, LineHeight, LinearGradient, ListStylePosition,
    ListStyleType, Position, TextAlign, TextDecorationLine, TextTransform, TrackSize, TransformFn,
    UnicodeBidi, WhiteSpace,
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
        assert_eq!(
            t.computed(find(&doc, "span")).background,
            Background::Color(transparent)
        );
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
    fn font_style_values() {
        let (doc, t) = style("<p>x</p>", "p { font-style: italic }");
        assert_eq!(t.computed(find(&doc, "p")).font_style, FontStyle::Italic);
        let (doc2, t2) = style("<p>x</p>", "p { font-style: oblique }");
        assert_eq!(t2.computed(find(&doc2, "p")).font_style, FontStyle::Oblique);
        let (doc3, t3) = style("<p>x</p>", "p { font-style: normal }");
        assert_eq!(t3.computed(find(&doc3, "p")).font_style, FontStyle::Normal);
        // `oblique 14deg` → Oblique (angle ignored).
        let (doc4, t4) = style("<p>x</p>", "p { font-style: oblique 14deg }");
        assert_eq!(t4.computed(find(&doc4, "p")).font_style, FontStyle::Oblique);
    }

    #[test]
    fn font_style_inherited() {
        // a child <span> with no font-style under an italic <p> is Italic.
        let (doc, t) = style("<p>a<span>b</span></p>", "p { font-style: italic }");
        assert_eq!(t.computed(find(&doc, "span")).font_style, FontStyle::Italic);
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
            t2.computed(find(&doc2, "p")).background,
            Background::Color(Rgba { r: 0, g: 0, b: 0, a: 0 })
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
        assert_eq!(b.background, Background::Color(red()));
        assert_eq!(b.border_top_width, 1.0);
        assert_eq!(b.border_style, BorderStyle::Solid);
        assert_eq!(b.border_color, blue());

        let a = t.computed(find(&doc, "a"));
        assert_eq!(a.display, Display::Inline);
        assert_eq!(a.color, blue());

        let ul = t.computed(find(&doc, "ul"));
        assert_eq!(ul.padding_left, Length::Px(40.0));
    }

    // --- E2-M1: text-decoration / list-style ---

    #[test]
    fn text_decoration_underline() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
        assert!(!d.contains(TextDecorationLine::OVERLINE));
        assert!(!d.contains(TextDecorationLine::LINE_THROUGH));
    }

    #[test]
    fn text_decoration_line_combines() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-line: underline overline }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
        assert!(d.contains(TextDecorationLine::OVERLINE));
        assert!(!d.contains(TextDecorationLine::LINE_THROUGH));
    }

    #[test]
    fn text_decoration_none() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: none }");
        assert!(t.computed(find(&doc, "p")).text_decoration_line.is_none());
    }

    #[test]
    fn text_decoration_shorthand_ignores_color_style() {
        // `underline` line keyword honored; `solid`/color ignored (M1).
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline solid red }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
    }

    #[test]
    fn list_style_type_values() {
        let (doc, t) = style("<ul><li>a</li></ul>", "li { list-style-type: square }");
        assert_eq!(t.computed(find(&doc, "li")).list_style_type, ListStyleType::Square);
        let (doc2, t2) = style("<ul><li>a</li></ul>", "li { list-style-type: none }");
        assert_eq!(t2.computed(find(&doc2, "li")).list_style_type, ListStyleType::None);
    }

    #[test]
    fn list_style_shorthand_type() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style: circle }");
        assert_eq!(t.computed(find(&doc, "ul")).list_style_type, ListStyleType::Circle);
    }

    #[test]
    fn ua_list_style_defaults() {
        let (doc, t) = style("<ul><li>a</li></ul><ol><li>b</li></ol>", "");
        assert_eq!(t.computed(find(&doc, "ul")).list_style_type, ListStyleType::Disc);
        assert_eq!(t.computed(find(&doc, "ol")).list_style_type, ListStyleType::Decimal);
    }

    #[test]
    fn list_style_inherits_to_li() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style-type: square }");
        // <li> has no own list-style-type → inherits the <ul>'s computed Square.
        assert_eq!(t.computed(find(&doc, "li")).list_style_type, ListStyleType::Square);
    }

    #[test]
    fn text_decoration_not_inherited() {
        let (doc, t) = style(
            "<p>a<span>b</span></p>",
            "p { text-decoration: underline }",
        );
        // span does not inherit the parent's text-decoration-line.
        assert!(t.computed(find(&doc, "span")).text_decoration_line.is_none());
    }

    // --- E2-M2: position / float / clear / offsets ---

    #[test]
    fn position_values() {
        use Position::*;
        for (kw, want) in [
            ("static", Static),
            ("relative", Relative),
            ("absolute", Absolute),
            ("fixed", Fixed),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ position: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).position, want);
        }
    }

    #[test]
    fn position_bogus_stays_static() {
        let (doc, t) = style("<p>x</p>", "p { position: sticky-ish }");
        assert_eq!(t.computed(find(&doc, "p")).position, Position::Static);
    }

    #[test]
    fn float_values() {
        let (doc, t) = style("<p>x</p>", "p { float: left }");
        assert_eq!(t.computed(find(&doc, "p")).float, Float::Left);
        let (doc2, t2) = style("<p>x</p>", "p { float: right }");
        assert_eq!(t2.computed(find(&doc2, "p")).float, Float::Right);
        let (doc3, t3) = style("<p>x</p>", "p { float: none }");
        assert_eq!(t3.computed(find(&doc3, "p")).float, Float::None);
    }

    #[test]
    fn clear_both() {
        let (doc, t) = style("<p>x</p>", "p { clear: both }");
        assert_eq!(t.computed(find(&doc, "p")).clear, Clear::Both);
    }

    #[test]
    fn offset_lengths_incl_negative_and_percent() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { top: 10px; left: -5px; right: 50%; bottom: auto }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.top, Length::Px(10.0));
        assert_eq!(p.left, Length::Px(-5.0));
        assert_eq!(p.right, Length::Percent(50.0));
        assert_eq!(p.bottom, Length::Auto);
    }

    #[test]
    fn positioning_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { position: relative; float: left; clear: both; top: 5px }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.position, Position::Static);
        assert_eq!(p.float, Float::None);
        assert_eq!(p.clear, Clear::None);
        assert_eq!(p.top, Length::Auto);
    }

    // --- E2-M3: flex ---

    #[test]
    fn display_flex_and_inline_flex() {
        let (doc, t) = style("<div>x</div>", "div { display: flex }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Flex);
        let (doc2, t2) = style("<div>x</div>", "div { display: inline-flex }");
        assert_eq!(t2.computed(find(&doc2, "div")).display, Display::InlineFlex);
    }

    #[test]
    fn flex_direction_and_bogus_default() {
        let (doc, t) = style("<div>x</div>", "div { flex-direction: column-reverse }");
        assert_eq!(
            t.computed(find(&doc, "div")).flex_direction,
            FlexDirection::ColumnReverse
        );
        // bogus → keeps initial Row.
        let (doc2, t2) = style("<div>x</div>", "div { flex-direction: sideways }");
        assert_eq!(t2.computed(find(&doc2, "div")).flex_direction, FlexDirection::Row);
    }

    #[test]
    fn flex_wrap_wrap_and_wrap_reverse_deferred() {
        let (doc, t) = style("<div>x</div>", "div { flex-wrap: wrap }");
        assert_eq!(t.computed(find(&doc, "div")).flex_wrap, FlexWrap::Wrap);
        // wrap-reverse deferred → ignored, stays Nowrap.
        let (doc2, t2) = style("<div>x</div>", "div { flex-wrap: wrap-reverse }");
        assert_eq!(t2.computed(find(&doc2, "div")).flex_wrap, FlexWrap::Nowrap);
    }

    #[test]
    fn justify_content_values() {
        let (doc, t) = style("<div>x</div>", "div { justify-content: space-between }");
        assert_eq!(
            t.computed(find(&doc, "div")).justify_content,
            JustifyContent::SpaceBetween
        );
        let (doc2, t2) = style("<div>x</div>", "div { justify-content: space-evenly }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).justify_content,
            JustifyContent::SpaceEvenly
        );
    }

    #[test]
    fn align_items_and_self() {
        let (doc, t) = style("<div>x</div>", "div { align-items: center }");
        assert_eq!(t.computed(find(&doc, "div")).align_items, AlignItems::Center);
        let (doc2, t2) = style("<div>x</div>", "div { align-self: flex-end }");
        assert_eq!(t2.computed(find(&doc2, "div")).align_self, AlignSelf::FlexEnd);
        // default align-self is Auto.
        let (doc3, t3) = style("<div>x</div>", "div { color: red }");
        assert_eq!(t3.computed(find(&doc3, "div")).align_self, AlignSelf::Auto);
    }

    #[test]
    fn flex_longhands() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { flex-grow: 2; flex-shrink: 0; flex-basis: 100px }",
        );
        let d = t.computed(find(&doc, "div"));
        assert_eq!(d.flex_grow, 2.0);
        assert_eq!(d.flex_shrink, 0.0);
        assert_eq!(d.flex_basis, Length::Px(100.0));
        // defaults.
        let (doc2, t2) = style("<div>x</div>", "div { color: red }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(d2.flex_grow, 0.0);
        assert_eq!(d2.flex_shrink, 1.0);
        assert_eq!(d2.flex_basis, Length::Auto);
    }

    #[test]
    fn flex_shorthand_forms() {
        let (doc, t) = style("<div>x</div>", "div { flex: none }");
        let d = t.computed(find(&doc, "div"));
        assert_eq!((d.flex_grow, d.flex_shrink, d.flex_basis), (0.0, 0.0, Length::Auto));

        let (doc2, t2) = style("<div>x</div>", "div { flex: auto }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!((d2.flex_grow, d2.flex_shrink, d2.flex_basis), (1.0, 1.0, Length::Auto));

        // single number = grow; omitted basis defaults to 0.
        let (doc3, t3) = style("<div>x</div>", "div { flex: 1 }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!((d3.flex_grow, d3.flex_shrink, d3.flex_basis), (1.0, 1.0, Length::Px(0.0)));

        let (doc4, t4) = style("<div>x</div>", "div { flex: 2 3 40px }");
        let d4 = t4.computed(find(&doc4, "div"));
        assert_eq!((d4.flex_grow, d4.flex_shrink, d4.flex_basis), (2.0, 3.0, Length::Px(40.0)));
    }

    #[test]
    fn gap_shorthand_and_longhands() {
        let (doc, t) = style("<div>x</div>", "div { gap: 10px }");
        let d = t.computed(find(&doc, "div"));
        assert_eq!(d.row_gap, Length::Px(10.0));
        assert_eq!(d.column_gap, Length::Px(10.0));

        let (doc2, t2) = style("<div>x</div>", "div { gap: 5px 8px }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(d2.row_gap, Length::Px(5.0));
        assert_eq!(d2.column_gap, Length::Px(8.0));

        let (doc3, t3) = style("<div>x</div>", "div { column-gap: 12px }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!(d3.row_gap, Length::Px(0.0));
        assert_eq!(d3.column_gap, Length::Px(12.0));
    }

    // --- E2-M5: background gradient / border-radius / box-shadow / opacity ---

    fn gradient(html: &str, css: &str, tag: &str) -> LinearGradient {
        let (doc, t) = style(html, css);
        match &t.computed(find(&doc, tag)).background {
            Background::Gradient(g) => g.clone(),
            other => panic!("expected gradient, got {other:?}"),
        }
    }

    #[test]
    fn gradient_to_right_two_stops() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(to right, red, blue) }",
            "div",
        );
        assert_eq!(g.angle_deg, 90.0);
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].color, red());
        assert_eq!(g.stops[1].color, blue());
    }

    #[test]
    fn gradient_angle_deg() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(45deg, #000, #fff) }",
            "div",
        );
        assert_eq!(g.angle_deg, 45.0);
        assert_eq!(g.stops.len(), 2);
    }

    #[test]
    fn gradient_default_direction_to_bottom() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(red, lime, blue) }",
            "div",
        );
        assert_eq!(g.angle_deg, 180.0);
        assert_eq!(g.stops.len(), 3);
    }

    #[test]
    fn gradient_with_rgba_and_percent_stops() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(90deg, rgba(255,0,0,0.5) 0%, blue 100%) }",
            "div",
        );
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].color, Rgba { r: 255, g: 0, b: 0, a: 128 });
        assert_eq!(g.stops[0].pos, Some(0.0));
        assert_eq!(g.stops[1].pos, Some(1.0));
    }

    #[test]
    fn background_solid_no_regression() {
        let (doc, t) = style("<div>x</div>", "div { background: red }");
        assert_eq!(t.computed(find(&doc, "div")).background, Background::Color(red()));
    }

    #[test]
    fn border_radius_shorthand_forms() {
        let (doc, t) = style("<div>x</div>", "div { border-radius: 8px }");
        assert_eq!(t.computed(find(&doc, "div")).border_radius, [8.0; 4]);
        let (doc2, t2) = style("<div>x</div>", "div { border-radius: 1px 2px }");
        assert_eq!(t2.computed(find(&doc2, "div")).border_radius, [1.0, 2.0, 1.0, 2.0]);
        let (doc3, t3) = style("<div>x</div>", "div { border-radius: 1px 2px 3px }");
        assert_eq!(t3.computed(find(&doc3, "div")).border_radius, [1.0, 2.0, 3.0, 2.0]);
        let (doc4, t4) = style("<div>x</div>", "div { border-radius: 1px 2px 3px 4px }");
        assert_eq!(t4.computed(find(&doc4, "div")).border_radius, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn box_shadow_forms() {
        let (doc, t) = style("<div>x</div>", "div { box-shadow: 2px 3px 4px 1px #000 }");
        assert_eq!(
            t.computed(find(&doc, "div")).box_shadow,
            Some(BoxShadow { offset_x: 2.0, offset_y: 3.0, blur: 4.0, spread: 1.0, color: black() })
        );
        let (doc2, t2) = style("<div>x</div>", "div { box-shadow: 2px 2px red }");
        let s = t2.computed(find(&doc2, "div")).box_shadow.unwrap();
        assert_eq!((s.blur, s.spread), (0.0, 0.0));
        assert_eq!(s.color, red());
        let (doc3, t3) = style("<div>x</div>", "div { box-shadow: none }");
        assert_eq!(t3.computed(find(&doc3, "div")).box_shadow, None);
    }

    #[test]
    fn opacity_clamps() {
        let (doc, t) = style("<div>x</div>", "div { opacity: 0.5 }");
        assert_eq!(t.computed(find(&doc, "div")).opacity, 0.5);
        let (doc2, t2) = style("<div>x</div>", "div { opacity: 2 }");
        assert_eq!(t2.computed(find(&doc2, "div")).opacity, 1.0);
        let (doc3, t3) = style("<div>x</div>", "div { opacity: -1 }");
        assert_eq!(t3.computed(find(&doc3, "div")).opacity, 0.0);
    }

    #[test]
    fn visual_effects_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { border-radius: 10px; opacity: 0.5; box-shadow: 1px 1px #000; \
             background: linear-gradient(red, blue) }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.border_radius, [0.0; 4]);
        assert_eq!(p.opacity, 1.0);
        assert_eq!(p.box_shadow, None);
        assert_eq!(p.background, Background::Color(Rgba { r: 0, g: 0, b: 0, a: 0 }));
    }

    #[test]
    fn flex_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: flex; flex-grow: 3; align-items: center; gap: 10px }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.flex_grow, 0.0);
        assert_eq!(p.align_items, AlignItems::Stretch);
        assert_eq!(p.row_gap, Length::Px(0.0));
        assert_ne!(p.display, Display::Flex);
    }

    // --- E5-M1: grid ---

    #[test]
    fn display_grid_and_inline_grid() {
        let (doc, t) = style("<div>x</div>", "div { display: grid }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Grid);
        let (doc2, t2) = style("<div>x</div>", "div { display: inline-grid }");
        assert_eq!(t2.computed(find(&doc2, "div")).display, Display::InlineGrid);
    }

    #[test]
    fn grid_template_columns_px() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: 100px 100px }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Px(100.0)]
        );
    }

    #[test]
    fn grid_template_columns_fr() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: 1fr 1fr 1fr }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Fr(1.0), Fr(1.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_repeat() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: repeat(3, 1fr) }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Fr(1.0), Fr(1.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_repeat_multi() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(2, 100px 1fr) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Fr(1.0), Px(100.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_mixed() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: 100px 1fr auto }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Fr(1.0), Auto]
        );
    }

    #[test]
    fn grid_template_auto_fill_ignored() {
        // auto-fill (non-integer repeat count) → declaration dropped → initial [].
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(auto-fill, 100px) }",
        );
        assert!(t.computed(find(&doc, "div")).grid_template_columns.is_empty());
    }

    #[test]
    fn grid_template_rows_none() {
        let (doc, t) = style("<div>x</div>", "div { grid-template-rows: none }");
        assert!(t.computed(find(&doc, "div")).grid_template_rows.is_empty());
    }

    #[test]
    fn grid_column_range() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-column: 1 / 3 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine { start: Line(1), end: Line(3) }
        );
    }

    #[test]
    fn grid_column_span() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-column: span 2 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine { start: Span(2), end: Auto }
        );
    }

    #[test]
    fn grid_row_single_and_negative() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-row: 2 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_row,
            GridLine { start: Line(2), end: Auto }
        );
        let (doc2, t2) = style("<div>x</div>", "div { grid-column: -1 }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).grid_column,
            GridLine { start: Line(-1), end: Auto }
        );
    }

    #[test]
    fn grid_column_longhands() {
        use GridPlacement::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-column-start: 2; grid-column-end: 4 }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine { start: Line(2), end: Line(4) }
        );
    }

    #[test]
    fn grid_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: grid; grid-template-columns: 1fr 1fr; grid-column: 1 / 3 }",
        );
        let p = t.computed(find(&doc, "p"));
        assert!(p.grid_template_columns.is_empty());
        assert_eq!(p.grid_column, GridLine::AUTO);
        assert_ne!(p.display, Display::Grid);
    }

    // --- E5-M2: grid alignment + named areas ---

    #[test]
    fn grid_justify_items_values() {
        let cases = [
            ("center", AlignItems::Center),
            ("start", AlignItems::FlexStart),
            ("end", AlignItems::FlexEnd),
            ("left", AlignItems::FlexStart),
            ("right", AlignItems::FlexEnd),
        ];
        for (kw, expect) in cases {
            let (doc, t) = style("<div>x</div>", &format!("div {{ justify-items: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "div")).justify_items, expect);
        }
    }

    #[test]
    fn grid_align_items_end_on_grid() {
        let (doc, t) = style("<div>x</div>", "div { align-items: end }");
        assert_eq!(t.computed(find(&doc, "div")).align_items, AlignItems::FlexEnd);
    }

    #[test]
    fn grid_justify_self_and_align_self() {
        let (doc, t) = style("<div>x</div>", "div { justify-self: stretch }");
        assert_eq!(t.computed(find(&doc, "div")).justify_self, AlignSelf::Stretch);
        let (doc2, t2) = style("<div>x</div>", "div {}");
        assert_eq!(t2.computed(find(&doc2, "div")).justify_self, AlignSelf::Auto);
        let (doc3, t3) = style("<div>x</div>", "div { align-self: center }");
        assert_eq!(t3.computed(find(&doc3, "div")).align_self, AlignSelf::Center);
    }

    #[test]
    fn grid_align_content_values() {
        let (doc, t) = style("<div>x</div>", "div { align-content: space-between }");
        assert_eq!(
            t.computed(find(&doc, "div")).align_content,
            JustifyContent::SpaceBetween
        );
        let (doc2, t2) = style("<div>x</div>", "div { align-content: center }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).align_content,
            JustifyContent::Center
        );
    }

    #[test]
    fn grid_template_areas_basic() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-areas: \"a a\" \"b c\" }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_areas,
            vec![
                vec!["a".to_string(), "a".to_string()],
                vec!["b".to_string(), "c".to_string()],
            ]
        );
    }

    #[test]
    fn grid_template_areas_dot_and_none() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-areas: \"h h\" \".\" \"f f\" }",
        );
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.grid_template_areas[1], vec![".".to_string()]);
        let (doc2, t2) = style("<div>x</div>", "div { grid-template-areas: none }");
        assert!(t2.computed(find(&doc2, "div")).grid_template_areas.is_empty());
    }

    #[test]
    fn grid_area_name_form() {
        let (doc, t) = style("<div>x</div>", "div { grid-area: header }");
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.grid_area_name, Some("header".to_string()));
        assert_eq!(c.grid_row, GridLine::AUTO);
        assert_eq!(c.grid_column, GridLine::AUTO);
    }

    #[test]
    fn grid_area_four_line_form() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-area: 1 / 2 / 3 / 4 }");
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.grid_row, GridLine { start: Line(1), end: Line(3) });
        assert_eq!(c.grid_column, GridLine { start: Line(2), end: Line(4) });
        assert_eq!(c.grid_area_name, None);
    }

    #[test]
    fn grid_align_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: grid; justify-items: center; \
             grid-template-areas: \"a a\"; grid-area: foo }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.justify_items, AlignItems::Stretch);
        assert_eq!(p.grid_area_name, None);
        assert!(p.grid_template_areas.is_empty());
    }

    // --- E5-M3: 2D transforms ---

    use computed::{LengthPct, TransformFn};

    fn xform(html: &str, css: &str, tag: &str) -> Vec<TransformFn> {
        let (doc, t) = style(html, css);
        t.computed(find(&doc, tag)).transform.clone()
    }

    #[test]
    fn transform_translate_two_and_axis() {
        let f = xform("<div>x</div>", "div { transform: translate(20px, 10px) }", "div");
        assert_eq!(f, vec![TransformFn::Translate(LengthPct::Px(20.0), LengthPct::Px(10.0))]);
        let fx = xform("<div>x</div>", "div { transform: translateX(5px) }", "div");
        assert_eq!(fx, vec![TransformFn::Translate(LengthPct::Px(5.0), LengthPct::Px(0.0))]);
        let fy = xform("<div>x</div>", "div { transform: translateY(5px) }", "div");
        assert_eq!(fy, vec![TransformFn::Translate(LengthPct::Px(0.0), LengthPct::Px(5.0))]);
    }

    #[test]
    fn transform_translate_percent() {
        let f = xform("<div>x</div>", "div { transform: translate(50%, 100%) }", "div");
        assert_eq!(
            f,
            vec![TransformFn::Translate(LengthPct::Percent(50.0), LengthPct::Percent(100.0))]
        );
    }

    #[test]
    fn transform_scale_forms() {
        assert_eq!(
            xform("<div>x</div>", "div { transform: scale(2) }", "div"),
            vec![TransformFn::Scale(2.0, 2.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scale(2, 3) }", "div"),
            vec![TransformFn::Scale(2.0, 3.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scaleX(2) }", "div"),
            vec![TransformFn::Scale(2.0, 1.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scaleY(4) }", "div"),
            vec![TransformFn::Scale(1.0, 4.0)]
        );
    }

    #[test]
    fn transform_rotate_units() {
        let want = std::f32::consts::FRAC_PI_2;
        for css in [
            "div { transform: rotate(90deg) }",
            "div { transform: rotate(0.25turn) }",
            "div { transform: rotate(1.5708rad) }",
            "div { transform: rotate(100grad) }",
        ] {
            let f = xform("<div>x</div>", css, "div");
            match f.as_slice() {
                [TransformFn::Rotate(r)] => assert!((r - want).abs() < 1e-3, "{css}: {r}"),
                other => panic!("{css}: {other:?}"),
            }
        }
    }

    #[test]
    fn transform_skew() {
        let f = xform("<div>x</div>", "div { transform: skewX(10deg) }", "div");
        match f.as_slice() {
            [TransformFn::Skew(ax, ay)] => {
                assert!((ax - 10.0f32.to_radians()).abs() < 1e-4);
                assert_eq!(*ay, 0.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn transform_matrix() {
        let f = xform("<div>x</div>", "div { transform: matrix(1,0,0,1,30,40) }", "div");
        assert_eq!(f, vec![TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 30.0, 40.0])]);
    }

    #[test]
    fn transform_multiple_in_order() {
        let f = xform("<div>x</div>", "div { transform: translate(10px) rotate(45deg) }", "div");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], TransformFn::Translate(LengthPct::Px(10.0), LengthPct::Px(0.0)));
        match f[1] {
            TransformFn::Rotate(r) => assert!((r - 45.0f32.to_radians()).abs() < 1e-4),
            ref o => panic!("{o:?}"),
        }
    }

    #[test]
    fn transform_none_and_bad_fn() {
        let (doc, t) = style("<div>x</div>", "div { transform: none }");
        assert!(t.computed(find(&doc, "div")).transform.is_empty());
        // unknown function → skipped → empty → leaves initial (empty).
        let (doc2, t2) = style("<div>x</div>", "div { transform: perspective(100px) foo(1) }");
        assert!(t2.computed(find(&doc2, "div")).transform.is_empty());
    }

    #[test]
    fn transform_origin_forms() {
        let (doc, t) = style("<div>x</div>", "div { transform-origin: top left }");
        assert_eq!(
            t.computed(find(&doc, "div")).transform_origin,
            (LengthPct::Percent(0.0), LengthPct::Percent(0.0))
        );
        let (doc2, t2) = style("<div>x</div>", "div { transform-origin: center }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
        let (doc3, t3) = style("<div>x</div>", "div { transform-origin: 10px 20px }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).transform_origin,
            (LengthPct::Px(10.0), LengthPct::Px(20.0))
        );
        // default initial = center.
        let (doc4, t4) = style("<div>x</div>", "div { color: red }");
        assert_eq!(
            t4.computed(find(&doc4, "div")).transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }

    // --- E6-M3: direction / unicode-bidi / spacing / transform / white-space ---

    #[test]
    fn direction_rtl_and_inherits() {
        let (doc, t) = style("<div><p>x</p></div>", "div { direction: rtl }");
        assert_eq!(t.computed(find(&doc, "div")).direction, Direction::Rtl);
        // child inherits.
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Rtl);
    }

    #[test]
    fn unicode_bidi_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { unicode-bidi: bidi-override }",
        );
        assert_eq!(t.computed(find(&doc, "div")).unicode_bidi, UnicodeBidi::BidiOverride);
        // NOT inherited → child stays Normal.
        assert_eq!(t.computed(find(&doc, "p")).unicode_bidi, UnicodeBidi::Normal);
    }

    #[test]
    fn letter_word_spacing_lengths() {
        let (doc, t) = style("<p>x</p>", "p { letter-spacing: 5px; word-spacing: 3px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.letter_spacing, 5.0);
        assert_eq!(p.word_spacing, 3.0);
        // normal → 0.
        let (doc2, t2) = style("<p>x</p>", "p { letter-spacing: normal }");
        assert_eq!(t2.computed(find(&doc2, "p")).letter_spacing, 0.0);
        // 2em against 16px font = 32.
        let (doc3, t3) = style("<p>x</p>", "p { font-size: 16px; letter-spacing: 2em }");
        assert_eq!(t3.computed(find(&doc3, "p")).letter_spacing, 32.0);
    }

    #[test]
    fn spacing_inherits() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { letter-spacing: 4px; word-spacing: 6px }",
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.letter_spacing, 4.0);
        assert_eq!(s.word_spacing, 6.0);
    }

    #[test]
    fn text_transform_values_and_inherit() {
        for (kw, want) in [
            ("uppercase", TextTransform::Uppercase),
            ("lowercase", TextTransform::Lowercase),
            ("capitalize", TextTransform::Capitalize),
            ("none", TextTransform::None),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ text-transform: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).text_transform, want);
        }
        // inherited.
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { text-transform: uppercase }",
        );
        assert_eq!(t.computed(find(&doc, "span")).text_transform, TextTransform::Uppercase);
    }

    #[test]
    fn white_space_values_and_inherit() {
        for (kw, want) in [
            ("normal", WhiteSpace::Normal),
            ("pre", WhiteSpace::Pre),
            ("nowrap", WhiteSpace::Nowrap),
            ("pre-wrap", WhiteSpace::PreWrap),
            ("pre-line", WhiteSpace::PreLine),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ white-space: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).white_space, want);
        }
        // inherited.
        let (doc, t) = style("<div><span>x</span></div>", "div { white-space: pre }");
        assert_eq!(t.computed(find(&doc, "span")).white_space, WhiteSpace::Pre);
    }

    #[test]
    fn transform_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { transform: rotate(45deg); transform-origin: 0 0 }",
        );
        let p = t.computed(find(&doc, "p"));
        assert!(p.transform.is_empty());
        assert_eq!(
            p.transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }
}
