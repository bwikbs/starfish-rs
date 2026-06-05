//! starfish-layout (M4) — block + inline flow layout over a styled DOM.
//!
//! Given a [`Document`], a [`StyledTree`] and a viewport width, [`layout`]
//! builds a box tree and computes absolute geometry for every box. The result
//! is a walkable [`LayoutBox`] consumed by M5 (paint). See
//! `docs/design/M4-layout.md`.

mod block;
mod boxtree;
mod dimensions;
mod flex;
mod float;
mod inline;
mod measure;

use starfish_dom::{Document, NodeKind};
use starfish_style::StyledTree;

pub use boxtree::{BoxKind, BoxStyleRef, LayoutBox};
pub use dimensions::{Dimensions, EdgeSizes, Rect};
pub use measure::{DefaultMeasurer, LineMetrics, TextMeasurer};
pub use starfish_dom::{Document as DomDocument, NodeId};
pub use starfish_style::{ComputedStyle, FontWeight};

use block::{layout_absolutes, layout_block};
use boxtree::build_box_tree;
use float::FloatContext;

impl LayoutBox {
    pub fn kind(&self) -> BoxKind {
        self.kind
    }

    pub fn children(&self) -> &[LayoutBox] {
        &self.children
    }

    pub fn dimensions(&self) -> &Dimensions {
        &self.dimensions
    }

    /// `Some(&str)` iff this box is a `TextRun`.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Resolve this box's `ComputedStyle` via the styled tree (None when the
    /// ref node isn't styled — shouldn't happen for real elements).
    pub fn style<'a>(&self, styled: &'a StyledTree) -> Option<&'a ComputedStyle> {
        styled.get(self.style.node())
    }

    /// Pre-order walk over this box and its descendants.
    pub fn walk(&self, f: &mut dyn FnMut(&LayoutBox)) {
        f(self);
        for child in &self.children {
            child.walk(f);
        }
    }
}

/// First element child of the document root (typically `<html>`).
fn root_element(doc: &Document) -> Option<NodeId> {
    let mut cur = doc.first_child(doc.root());
    while let Some(id) = cur {
        if matches!(doc.kind(id), NodeKind::Element(_)) {
            return Some(id);
        }
        cur = doc.next_sibling(id);
    }
    None
}

/// Build the box tree and lay it out against a viewport of the given width. The
/// viewport is the initial containing block: content origin `(0, 0)`, content
/// width `viewport_width`, height growing with content. Returns the root
/// `LayoutBox` (a `BlockContainer` for the root element).
pub fn layout(
    doc: &Document,
    styled: &StyledTree,
    viewport_width: f32,
    measurer: &dyn TextMeasurer,
) -> LayoutBox {
    let Some(root_el) = root_element(doc) else {
        return LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Anonymous(doc.root()));
    };

    let mut root = build_box_tree(doc, styled, root_el);

    let initial_cb = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width: viewport_width, height: 0.0 },
        ..Dimensions::default()
    };

    let mut floats = FloatContext::default();
    layout_block(&mut root, initial_cb, styled, measurer, &mut floats);

    // Phase 2 (§4.2): position abs/fixed boxes against their containing block.
    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: viewport_width,
        height: root.dimensions.content.height,
    };
    layout_absolutes(&mut root, viewport, viewport, styled, measurer);
    root
}

/// Convenience wrapper using [`DefaultMeasurer`].
pub fn layout_default(doc: &Document, styled: &StyledTree, viewport_width: f32) -> LayoutBox {
    layout(doc, styled, viewport_width, &DefaultMeasurer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;
    use starfish_style::{style_tree, FontWeight};

    /// Fixed-width measurer: each char advances `per` px regardless of font.
    /// Makes wrap assertions exact.
    struct FixedMeasurer {
        per: f32,
    }
    impl TextMeasurer for FixedMeasurer {
        fn measure(&self, text: &str, _fs: f32, _w: FontWeight) -> f32 {
            text.chars().count() as f32 * self.per
        }
    }

    fn build(html: &str, css: &str) -> (Document, StyledTree) {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let tree = style_tree(&doc, &[sheet]);
        (doc, tree)
    }

    fn find(doc: &Document, tag: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some(tag) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no <{tag}>");
    }

    /// Find the first layout box whose style ref is `id` (pre-order, excluding
    /// LineBox wrappers which borrow the container's style ref).
    fn box_for(b: &LayoutBox, id: NodeId) -> Option<&LayoutBox> {
        if b.style.node() == id && b.kind != BoxKind::LineBox {
            return Some(b);
        }
        for c in &b.children {
            if let Some(found) = box_for(c, id) {
                return Some(found);
            }
        }
        None
    }

    // --- §9.1 block stacking ---

    #[test]
    fn two_divs_stack_y() {
        // No margins to keep arithmetic clean.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>",
            "body{margin:0} div{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(b.dimensions.content.y, a.dimensions.margin_box().y + a.dimensions.margin_box().height);
    }

    #[test]
    fn stacking_adds_margins() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>",
            "body{margin:0} div{margin:10px 0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        // b's content.y = a.margin_box bottom + b.margin.top
        let expected = a.dimensions.margin_box().y + a.dimensions.margin_box().height + 10.0;
        assert_eq!(b.dimensions.content.y, expected);
    }

    #[test]
    fn nested_child_offset_by_parent_padding() {
        let (doc, t) = build(
            "<html><body><div id='outer'><div id='inner'>x</div></div></body></html>",
            "body{margin:0} #outer{margin:0;padding:20px;border:0} #inner{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let outer = box_for(&root, find_id(&doc, "outer")).unwrap();
        let inner = box_for(&root, find_id(&doc, "inner")).unwrap();
        // outer's own content origin is pushed in by its padding (body x=0).
        assert_eq!(outer.dimensions.content.x, 20.0);
        assert_eq!(outer.dimensions.content.y, 20.0);
        // inner (no margin/border/padding) sits at outer's content origin,
        // i.e. offset from outer's border-box by outer's padding-left/top.
        assert_eq!(inner.dimensions.content.x, outer.dimensions.content.x);
        assert_eq!(inner.dimensions.content.y, outer.dimensions.content.y);
        assert_eq!(
            inner.dimensions.content.x,
            outer.dimensions.border_box().x + outer.dimensions.padding.left
        );
    }

    // --- §9.2 width ---

    #[test]
    fn auto_width_fills_minus_body_margin() {
        let (doc, t) = build("<html><body><div id='a'>x</div></body></html>", "div{margin:0}");
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        // body has UA margin 8px each side.
        let body = box_for(&root, find(&doc, "body")).unwrap();
        assert_eq!(body.dimensions.content.width, 800.0 - 16.0);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 800.0 - 16.0);
    }

    #[test]
    fn percent_width() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{width:50%;margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 400.0);
    }

    #[test]
    fn fixed_width_padding_border_right_margin_absorbs() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{width:200px;padding:10px;border:5px solid black;margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 200.0);
        assert_eq!(a.dimensions.padding.left, 10.0);
        assert_eq!(a.dimensions.border.left, 5.0);
        // right margin absorbs underflow = 800 - (200+20+10) = 570
        assert_eq!(a.dimensions.margin.right, 570.0);
        assert_eq!(a.dimensions.margin.left, 0.0);
    }

    #[test]
    fn auto_margin_centers() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0;width:300px} #a{width:100px;margin:0 auto}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 100.0);
        assert_eq!(a.dimensions.margin.left, 100.0);
        assert_eq!(a.dimensions.margin.right, 100.0);
    }

    // --- §9.3 height ---

    #[test]
    fn auto_height_sums_children() {
        let (doc, t) = build(
            "<html><body id='bd'><div id='a'>x</div><div id='b'>y</div></body></html>",
            "body{margin:0} div{margin:0;height:50px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let bd = box_for(&root, find_id(&doc, "bd")).unwrap();
        assert_eq!(bd.dimensions.content.height, 100.0);
    }

    #[test]
    fn explicit_height_overrides() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{height:120px;margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.height, 120.0);
    }

    // --- §9.4 box generation / anonymous block ---

    #[test]
    fn mixed_children_wrap_anonymous() {
        let (doc, t) = build(
            "<html><body><div id='d'><p>a</p>text<p>b</p></div></body></html>",
            "",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        let kinds: Vec<BoxKind> = d.children.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![BoxKind::BlockContainer, BoxKind::AnonymousBlock, BoxKind::BlockContainer]);
        // anonymous block wraps a TextRun (flowed into a LineBox after layout)
        let anon = &d.children[1];
        assert_eq!(anon.children[0].kind, BoxKind::LineBox);
    }

    #[test]
    fn all_inline_no_anonymous() {
        let (doc, t) = build(
            "<html><body><div id='d'><span>a</span> <span>b</span></div></body></html>",
            "",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        // div ran inline layout → its children are LineBoxes, no AnonymousBlock.
        assert!(d.children.iter().all(|c| c.kind == BoxKind::LineBox));
        assert!(!d.children.is_empty());
    }

    #[test]
    fn display_none_removed() {
        let (doc, t) = build(
            "<html><body><div id='d'><p class='hidden'>x</p><p id='y'>y</p></div></body></html>",
            ".hidden{display:none}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        // only one block child (the visible <p>)
        let blocks: Vec<&LayoutBox> = d.children.iter().filter(|c| c.kind == BoxKind::BlockContainer).collect();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].style.node(), find_id(&doc, "y"));
    }

    #[test]
    fn whitespace_only_between_blocks_dropped() {
        let (doc, t) = build(
            "<html><body><div id='d'><p>a</p>   <p>b</p></div></body></html>",
            "",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        // no anonymous block: only the two <p> blocks.
        let kinds: Vec<BoxKind> = d.children.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![BoxKind::BlockContainer, BoxKind::BlockContainer]);
    }

    // --- §9.5 inline wrapping ---

    #[test]
    fn wraps_into_three_lines() {
        // 7 words, each 3 chars; per-char 10px → word=30, space=10.
        // avail width chosen so 3 words fit: 30+10+30+10+30 = 110, +10+30 = 150 > 120.
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb ccc ddd eee fff ggg</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 120.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines: Vec<&LayoutBox> = p.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].children.len(), 3);
        assert_eq!(lines[1].children.len(), 3);
        assert_eq!(lines[2].children.len(), 1);
        // fragment x positions on the first line: 0, 40, 80 (relative to p.x=0)
        assert_eq!(lines[0].children[0].dimensions.content.x, 0.0);
        assert_eq!(lines[0].children[1].dimensions.content.x, 40.0);
        assert_eq!(lines[0].children[2].dimensions.content.x, 80.0);
    }

    #[test]
    fn line_height_normal_and_px() {
        let (doc, t) = build(
            "<html><body><p id='p'>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert_eq!(line.dimensions.content.height, 24.0); // 1.2 * 20

        let (doc2, t2) = build(
            "<html><body><p id='p'>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;line-height:30px}",
        );
        let root2 = layout(&doc2, &t2, 800.0, &DefaultMeasurer);
        let p2 = box_for(&root2, find_id(&doc2, "p")).unwrap();
        let line2 = p2.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert_eq!(line2.dimensions.content.height, 30.0);
    }

    #[test]
    fn overlong_word_on_own_line() {
        let (doc, t) = build(
            "<html><body><p id='p'>aaaaaaaaaa bbb</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        // 10-char word = 100px > avail 50; goes alone, overflowing.
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 50.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines: Vec<&LayoutBox> = p.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].children[0].dimensions.content.width, 100.0);
    }

    #[test]
    fn second_line_y_below_first() {
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb ccc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        // avail 40 → only 1 word per line (30 + 10 + 30 = 70 > 40)
        let root = layout(&doc, &t, 40.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines: Vec<&LayoutBox> = p.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 3);
        let h0 = lines[0].dimensions.content.height;
        assert_eq!(lines[1].dimensions.content.y, lines[0].dimensions.content.y + h0);
    }

    #[test]
    fn text_align_center_offsets() {
        let (doc, t) = build(
            "<html><body><p id='p'>aaa</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;text-align:center}",
        );
        let m = FixedMeasurer { per: 10.0 };
        // word width 30 in avail 100 → slack 70 → offset 35.
        let root = layout(&doc, &t, 100.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert_eq!(line.children[0].dimensions.content.x, 35.0);
    }

    #[test]
    fn adjacent_inline_runs_no_whitespace() {
        // <p>a<span>b</span>c</p> — no source whitespace between pieces, so the
        // three single-char runs must abut: x = 0, N, 2N (N = 10).
        let (doc, t) = build(
            "<html><body><p id='p'>a<span>b</span>c</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        let xs: Vec<f32> = line.children.iter().map(|c| c.dimensions.content.x).collect();
        assert_eq!(xs, vec![0.0, 10.0, 20.0]);
        let texts: Vec<Option<&str>> = line.children.iter().map(|c| c.text()).collect();
        assert_eq!(texts, vec![Some("a"), Some("b"), Some("c")]);
    }

    #[test]
    fn adjacent_inline_runs_with_whitespace() {
        // <p>a <span>b</span> c</p> — a space exists on each side of <span>, so
        // each word advances by an extra space (10px): x = 0, 20, 40.
        let (doc, t) = build(
            "<html><body><p id='p'>a <span>b</span> c</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        let xs: Vec<f32> = line.children.iter().map(|c| c.dimensions.content.x).collect();
        assert_eq!(xs, vec![0.0, 20.0, 40.0]);
    }

    #[test]
    fn nested_percent_width() {
        // outer width:400, inner width:50% → 200 (resolved against outer's 400).
        let (doc, t) = build(
            "<html><body><div id='outer'><div id='inner'>x</div></div></body></html>",
            "body{margin:0} #outer{margin:0;width:400px} #inner{margin:0;width:50%}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let outer = box_for(&root, find_id(&doc, "outer")).unwrap();
        let inner = box_for(&root, find_id(&doc, "inner")).unwrap();
        assert_eq!(outer.dimensions.content.width, 400.0);
        assert_eq!(inner.dimensions.content.width, 200.0);
    }

    #[test]
    fn over_constrained_auto_margins_no_panic() {
        // Explicit width that exactly fills the CB leaves zero slack; both auto
        // margins resolve to 0 (and one-auto likewise). No panic, sane values.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0;width:200px} #a{width:200px;margin:0 auto}",
        );
        let root = layout(&doc, &t, 200.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 200.0);
        assert_eq!(a.dimensions.margin.left, 0.0);
        assert_eq!(a.dimensions.margin.right, 0.0);

        // One auto margin with no slack → that margin is 0.
        let (doc2, t2) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0;width:200px} #a{width:200px;margin-left:auto;margin-right:0}",
        );
        let root2 = layout(&doc2, &t2, 200.0, &DefaultMeasurer);
        let a2 = box_for(&root2, find_id(&doc2, "a")).unwrap();
        assert_eq!(a2.dimensions.content.width, 200.0);
        assert_eq!(a2.dimensions.margin.left, 0.0);
        assert_eq!(a2.dimensions.margin.right, 0.0);
    }

    #[test]
    fn exact_fit_wrap_boundary_stays_on_line() {
        // Two 3-char words (30px each) + one space (10px) = 70 == avail 70.
        // The `<=` boundary keeps the second word on the same line.
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 70.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines: Vec<&LayoutBox> = p.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].children.len(), 2);
        assert_eq!(lines[0].children[1].dimensions.content.x, 40.0);
    }

    #[test]
    fn multibyte_text_places_without_panic() {
        // "héllo wörld" — multibyte chars; FixedMeasurer counts by char, so each
        // word is 5 chars = 50px, space 10px → second word at x = 60.
        let (doc, t) = build(
            "<html><body><p id='p'>héllo wörld</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert_eq!(line.children.len(), 2);
        assert_eq!(line.children[0].text(), Some("héllo"));
        assert_eq!(line.children[1].text(), Some("wörld"));
        assert_eq!(line.children[0].dimensions.content.x, 0.0);
        assert_eq!(line.children[1].dimensions.content.x, 60.0);
    }

    // --- whitespace collapse in text ---

    #[test]
    fn text_run_collapses_whitespace() {
        let (doc, t) = build(
            "<html><body><p id='p'>a    b</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        // two words "a" and "b", collapsed single space between.
        assert_eq!(line.children.len(), 2);
        assert_eq!(line.children[0].text(), Some("a"));
        assert_eq!(line.children[1].text(), Some("b"));
    }

    // --- §9.6 end-to-end smoke ---

    #[test]
    fn smoke_page_has_height_and_ordering() {
        let html = "<html><body>\
            <h1 id='h'>Hello</h1>\
            <p id='p'>some words here that may wrap onto lines</p>\
            <div><p>nested</p></div>\
            </body></html>";
        let (doc, t) = build(html, "body{margin:0}");
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        assert!(root.dimensions.border_box().height > 0.0);
        let h = box_for(&root, find_id(&doc, "h")).unwrap();
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        // heading sits above paragraph.
        assert!(h.dimensions.content.y < p.dimensions.content.y);
        // page height equals root border-box height.
        assert_eq!(root.dimensions.border_box().height, root.dimensions.content.height);
    }

    #[test]
    fn empty_document_returns_zero_box() {
        let doc = parse("");
        let t = style_tree(&doc, &[]);
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        // parse("") still yields an html skeleton; ensure no panic and a block.
        assert_eq!(root.kind, BoxKind::BlockContainer);
    }

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
        panic!("no #{id}");
    }

    // --- E2-M1: inline-block, list markers ---

    /// Collect every fragment of a given kind inside a box subtree (pre-order).
    fn collect_kind<'a>(b: &'a LayoutBox, kind: BoxKind, out: &mut Vec<&'a LayoutBox>) {
        if b.kind == kind {
            out.push(b);
        }
        for c in &b.children {
            collect_kind(c, kind, out);
        }
    }

    #[test]
    fn inline_block_occupies_its_size_on_a_line() {
        // 100x50 inline-block in a wide container → margin-box 100x50, line ≥ 50.
        let (doc, t) = build(
            "<html><body><div id='d'><span id='s'>x</span></div></body></html>",
            "body{margin:0} div{margin:0} #s{display:inline-block;width:100px;height:50px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer);
        let s = box_for(&root, find_id(&doc, "s")).unwrap();
        assert_eq!(s.dimensions.margin_box().width, 100.0);
        assert_eq!(s.dimensions.margin_box().height, 50.0);
        // its enclosing LineBox is at least 50 tall.
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        let line = d.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert!(line.dimensions.content.height >= 50.0);
    }

    #[test]
    fn two_inline_blocks_side_by_side_then_wrap() {
        // Three 100px inline-blocks in a 250px container: two fit on line 0,
        // the third wraps to line 1.
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <span class='ib'>a</span><span class='ib'>b</span><span class='ib'>c</span>\
             </div></body></html>",
            "body{margin:0} div{margin:0} .ib{display:inline-block;width:100px;height:20px}",
        );
        let root = layout(&doc, &t, 250.0, &DefaultMeasurer);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        let lines: Vec<&LayoutBox> = d.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 2);
        // line 0 has two inline-blocks; line 1 has one.
        let mut l0 = Vec::new();
        collect_kind(lines[0], BoxKind::InlineBlock, &mut l0);
        let mut l1 = Vec::new();
        collect_kind(lines[1], BoxKind::InlineBlock, &mut l1);
        assert_eq!(l0.len(), 2);
        assert_eq!(l1.len(), 1);
        // side-by-side: second atom starts ~100 right of the first.
        assert_eq!(l0[0].dimensions.margin_box().x, 0.0);
        assert_eq!(l0[1].dimensions.margin_box().x, 100.0);
    }

    #[test]
    fn inline_block_wraps_when_exceeding_container() {
        // 100px inline-block in an 80px container → on its own line, overflowing.
        let (doc, t) = build(
            "<html><body><div id='d'><span id='s'>x</span></div></body></html>",
            "body{margin:0} div{margin:0} #s{display:inline-block;width:100px;height:20px}",
        );
        let root = layout(&doc, &t, 80.0, &DefaultMeasurer);
        let s = box_for(&root, find_id(&doc, "s")).unwrap();
        assert_eq!(s.dimensions.margin_box().width, 100.0);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        let lines: Vec<&LayoutBox> = d.children.iter().filter(|c| c.kind == BoxKind::LineBox).collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn ul_li_produces_disc_markers() {
        let (doc, t) = build(
            "<html><body><ul><li>a</li><li>b</li></ul></body></html>",
            "body{margin:0}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        let mut markers = Vec::new();
        collect_kind(&root, BoxKind::Marker, &mut markers);
        assert_eq!(markers.len(), 2);
        for mk in &markers {
            assert_eq!(mk.text(), Some("\u{2022}"));
        }
    }

    #[test]
    fn marker_positioned_left_of_content() {
        let (doc, t) = build(
            "<html><body><ul id='u'><li id='l'>a</li></ul></body></html>",
            "body{margin:0}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        let li = box_for(&root, find_id(&doc, "l")).unwrap();
        let line = li.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        let marker = &line.children[0];
        assert_eq!(marker.kind, BoxKind::Marker);
        // marker x is left of the line content origin (in the gutter).
        assert!(marker.dimensions.content.x < line.dimensions.content.x);
    }

    #[test]
    fn ol_produces_decimal_markers() {
        let (doc, t) = build(
            "<html><body><ol><li>x</li><li>y</li><li>z</li></ol></body></html>",
            "body{margin:0}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        let mut markers = Vec::new();
        collect_kind(&root, BoxKind::Marker, &mut markers);
        let texts: Vec<Option<&str>> = markers.iter().map(|mk| mk.text()).collect();
        assert_eq!(texts, vec![Some("1."), Some("2."), Some("3.")]);
    }

    #[test]
    fn list_style_none_hides_marker() {
        let (doc, t) = build(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0} ul{list-style-type:none}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        let mut markers = Vec::new();
        collect_kind(&root, BoxKind::Marker, &mut markers);
        assert!(markers.is_empty());
    }

    #[test]
    fn nested_ol_numbers_restart_per_list() {
        // Ordinals count only the same-parent <li> siblings and restart per
        // direct parent list. The HTML parser now nests the inner <ol> inside
        // the first <li> correctly (see starfish-html nested_list test).
        //
        // <li id='a'> has mixed content (text + a nested <ol>), so it runs the
        // BLOCK path and gets NO marker (§3.2/§6 — marker only on inline-content
        // li). The inner items x/y are pure-inline → markers restart 1./2., and
        // the outer <li id='b'> is "2." (proving the outer ordinal still counts
        // `a` as item 1 even though `a` shows no marker).
        let (doc, t) = build(
            "<html><body><ol id='o'><li id='a'>a<ol id='inner'><li id='x'>x</li>\
             <li id='y'>y</li></ol></li><li id='b'>b</li></ol></body></html>",
            "body{margin:0}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        // Pull the marker text out of one li's own first LineBox (not its
        // descendants') so nesting doesn't confuse the lookup.
        let marker_of = |id: &str| -> Option<String> {
            let li = box_for(&root, find_id(&doc, id)).unwrap();
            let line = li.children.iter().find(|c| c.kind == BoxKind::LineBox)?;
            line.children
                .iter()
                .find(|c| c.kind == BoxKind::Marker)
                .and_then(|m| m.text())
                .map(str::to_string)
        };
        // Mixed-content outer item: no marker generated (block path).
        assert_eq!(marker_of("a"), None);
        // Outer ordinal still counts `a` as #1, so `b` is #2.
        assert_eq!(marker_of("b").as_deref(), Some("2."));
        // Inner list restarts: x=1., y=2.
        assert_eq!(marker_of("x").as_deref(), Some("1."));
        assert_eq!(marker_of("y").as_deref(), Some("2."));
    }

    #[test]
    fn li_with_block_child_has_no_marker() {
        // <li><p>x</p></li> runs the block path; no Marker box is generated, so
        // the <p> isn't pushed down by a phantom marker line.
        let (doc, t) = build(
            "<html><body><ul><li id='l'><p id='p'>x</p></li></ul></body></html>",
            "body{margin:0} ul{margin:0;padding:0} li{margin:0} p{margin:0}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 400.0, &m);
        let mut markers = Vec::new();
        collect_kind(&root, BoxKind::Marker, &mut markers);
        assert!(markers.is_empty(), "block-content li must not get a marker");
        // The <p> sits at the top of the <li>'s content (no phantom marker line).
        let li = box_for(&root, find_id(&doc, "l")).unwrap();
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        assert_eq!(p.dimensions.content.y, li.dimensions.content.y);
    }

    #[test]
    fn text_align_center_shifts_text_and_inline_block_equally() {
        // A centered line with a word + an inline-block: both shift by the same
        // text-align offset (used width includes the atomic's extent).
        let (doc, t) = build(
            "<html><body><div id='d'>aa<span id='s'>b</span></div></body></html>",
            "body{margin:0} div{margin:0;font-size:10px;text-align:center}\
             #s{display:inline-block;width:40px;height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        // word "aa" = 20px, atomic = 40px, no space between → used width 60.
        // avail 200 → slack 140 → center offset 70.
        let root = layout(&doc, &t, 200.0, &m);
        let d = box_for(&root, find_id(&doc, "d")).unwrap();
        let line = d.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        let word = line.children.iter().find(|c| c.kind == BoxKind::TextRun).unwrap();
        let atom = box_for(&root, find_id(&doc, "s")).unwrap();
        // word starts at offset 70; atom margin-box left at offset+20 = 90.
        assert_eq!(word.dimensions.content.x, 70.0);
        assert_eq!(atom.dimensions.margin_box().x, 90.0);
    }

    // --- E2-M2: float / clear ---

    /// LineBoxes inside a box subtree, in document order.
    fn line_boxes(b: &LayoutBox) -> Vec<&LayoutBox> {
        let mut out = Vec::new();
        collect_kind(b, BoxKind::LineBox, &mut out);
        out
    }

    #[test]
    fn left_float_shortens_following_lines() {
        // A 100px-wide, 40px-tall left float, then a <p> of words. Lines whose
        // band overlaps [0,40) start shifted right by 100 and are narrower; a
        // line at y>=40 returns to full width.
        // Float 100px wide, 40px tall (= 2 line bands of 20px). Paragraph of
        // many words so it wraps to 3+ lines; the 3rd line (y=40) is below the
        // float and must return to full width.
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f'>x</div>\
             <p id='p'>aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa aa</p>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f{float:left;width:100px;height:40px;margin:0} \
             p{margin:0;font-size:10px;line-height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 300.0, &m);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines = line_boxes(p);
        assert!(lines.len() >= 3, "got {} lines", lines.len());
        // First line overlaps the float band → starts at p.x + 100, narrower.
        assert_eq!(lines[0].dimensions.content.x, p.dimensions.content.x + 100.0);
        assert_eq!(lines[0].dimensions.content.width, 200.0);
        // A line whose y >= 40 returns to full width at p.x.
        let below = lines
            .iter()
            .find(|l| l.dimensions.content.y >= 40.0)
            .expect("a line below the float");
        assert_eq!(below.dimensions.content.x, p.dimensions.content.x);
        assert_eq!(below.dimensions.content.width, 300.0);
    }

    #[test]
    fn right_float_reduces_line_end() {
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f'>x</div>\
             <p id='p'>aa aa aa aa aa aa</p>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f{float:right;width:80px;height:40px;margin:0} \
             p{margin:0;font-size:10px;line-height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 300.0, &m);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        // float's margin-box right edge sits at the CB content right edge (300).
        assert_eq!(f.dimensions.margin_box().x + f.dimensions.margin_box().width, 300.0);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let lines = line_boxes(p);
        // First line: full left start, reduced width (avail - 80).
        assert_eq!(lines[0].dimensions.content.x, p.dimensions.content.x);
        assert_eq!(lines[0].dimensions.content.width, 220.0);
    }

    #[test]
    fn two_left_floats_second_drops_down() {
        // Two left floats each 200px wide in a 300px CB → second can't fit beside
        // the first and drops below it.
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f1'>x</div><div id='f2'>y</div>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f1{float:left;width:200px;height:30px;margin:0} \
             #f2{float:left;width:200px;height:30px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let f1 = box_for(&root, find_id(&doc, "f1")).unwrap();
        let f2 = box_for(&root, find_id(&doc, "f2")).unwrap();
        assert_eq!(f1.dimensions.content.y, 0.0);
        assert!(
            f2.dimensions.content.y >= f1.dimensions.margin_box().y + f1.dimensions.margin_box().height,
            "f2.y {} should be below f1 bottom {}",
            f2.dimensions.content.y,
            f1.dimensions.margin_box().y + f1.dimensions.margin_box().height
        );
    }

    #[test]
    fn two_wide_left_floats_second_drops_below() {
        // Two left floats each 400px wide in a 300px CB → neither fits beside the
        // other (remaining space goes NEGATIVE), so the second must drop below the
        // first at the CB's left edge rather than overlapping it at (400, 0).
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f1'>x</div><div id='f2'>y</div>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f1{float:left;width:400px;height:30px;margin:0} \
             #f2{float:left;width:400px;height:30px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let f1 = box_for(&root, find_id(&doc, "f1")).unwrap();
        let f2 = box_for(&root, find_id(&doc, "f2")).unwrap();
        // First float placed at the band start.
        assert_eq!(f1.dimensions.content.y, 0.0);
        assert_eq!(f1.dimensions.content.x, 0.0);
        // Second drops to the first float's bottom, back at the left edge.
        assert_eq!(
            f2.dimensions.content.y,
            f1.dimensions.margin_box().y + f1.dimensions.margin_box().height,
            "f2.y {} should equal f1 bottom {}",
            f2.dimensions.content.y,
            f1.dimensions.margin_box().y + f1.dimensions.margin_box().height
        );
        assert_eq!(f2.dimensions.content.x, 0.0);
    }

    #[test]
    fn single_float_wider_than_cb_places_at_left() {
        // A single float wider than the empty CB has nothing below to drop past;
        // it must still be placed at the band's left edge (overflowing) without
        // panicking or hanging.
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f'>x</div>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f{float:left;width:400px;height:30px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.content.y, 0.0);
        assert_eq!(f.dimensions.content.x, 0.0);
    }

    #[test]
    fn clear_both_drops_below_float() {
        // A left float (height 50) then a clear:both div → the cleared div drops
        // below the float's margin-box bottom.
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f'>x</div>\
             <div id='c'>y</div>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f{float:left;width:80px;height:50px;margin:0} \
             #c{clear:both;height:10px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        let c = box_for(&root, find_id(&doc, "c")).unwrap();
        assert!(
            c.dimensions.content.y >= f.dimensions.margin_box().y + f.dimensions.margin_box().height,
            "cleared div y {} below float bottom {}",
            c.dimensions.content.y,
            f.dimensions.margin_box().y + f.dimensions.margin_box().height
        );
    }

    #[test]
    fn non_cleared_sibling_overlaps_float_band() {
        // Control: a NON-cleared block sibling after a float starts at the float's
        // top y (block boxes span full width, only their lines wrap).
        let (doc, t) = build(
            "<html><body><div id='d'>\
             <div id='f'>x</div>\
             <div id='n'>y</div>\
             </div></body></html>",
            "body{margin:0} #d{margin:0} \
             #f{float:left;width:80px;height:50px;margin:0} \
             #n{height:10px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let n = box_for(&root, find_id(&doc, "n")).unwrap();
        assert_eq!(n.dimensions.content.y, 0.0);
    }

    // --- E2-M2: position relative / absolute / fixed ---

    #[test]
    fn relative_shifts_paint_only_sibling_reserved() {
        // #a is relative left:20 top:10; #b follows. #a paints offset; #b's y is
        // the same as if #a had no offset (space reserved).
        let css_rel = "body{margin:0} #a{position:relative;left:20px;top:10px;height:30px;margin:0} #b{height:10px;margin:0}";
        let css_base = "body{margin:0} #a{height:30px;margin:0} #b{height:10px;margin:0}";
        let html = "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>";

        let (doc, t) = build(html, css_rel);
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();

        let (doc0, t0) = build(html, css_base);
        let root0 = layout(&doc0, &t0, 300.0, &DefaultMeasurer);
        let a0 = box_for(&root0, find_id(&doc0, "a")).unwrap();
        let b0 = box_for(&root0, find_id(&doc0, "b")).unwrap();

        // a shifted by (20,10) vs the baseline position.
        assert_eq!(a.dimensions.content.x, a0.dimensions.content.x + 20.0);
        assert_eq!(a.dimensions.content.y, a0.dimensions.content.y + 10.0);
        // b unmoved (space reserved by a's pre-translation slot).
        assert_eq!(b.dimensions.content.y, b0.dimensions.content.y);
    }

    #[test]
    fn absolute_within_relative_parent() {
        // #c{absolute;top:10;left:15;w30;h20} inside #p{relative;padding:5}.
        // c.margin_box top-left == p.padding_box origin + (15,10). c does not
        // advance p's in-flow height: a static sibling sits at top of p.
        let (doc, t) = build(
            "<html><body><div id='p'>\
             <div id='s'>s</div>\
             <div id='c'>c</div>\
             </div></body></html>",
            "body{margin:0} #p{position:relative;padding:5px;margin:0} \
             #s{height:8px;margin:0} \
             #c{position:absolute;top:10px;left:15px;width:30px;height:20px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let c = box_for(&root, find_id(&doc, "c")).unwrap();
        let s = box_for(&root, find_id(&doc, "s")).unwrap();
        let pad = p.dimensions.padding_box();
        assert_eq!(c.dimensions.margin_box().x, pad.x + 15.0);
        assert_eq!(c.dimensions.margin_box().y, pad.y + 10.0);
        assert_eq!(c.dimensions.content.width, 30.0);
        // static sibling sits at the top of p's content (abs c did not advance).
        assert_eq!(s.dimensions.content.y, p.dimensions.content.y);
    }

    #[test]
    fn absolute_no_positioned_ancestor_uses_viewport() {
        let (doc, t) = build(
            "<html><body><div id='c'>c</div></body></html>",
            "body{margin:0} #c{position:absolute;top:0;left:0;width:20px;height:20px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let c = box_for(&root, find_id(&doc, "c")).unwrap();
        assert_eq!(c.dimensions.margin_box().x, 0.0);
        assert_eq!(c.dimensions.margin_box().y, 0.0);
    }

    #[test]
    fn fixed_positioned_against_viewport() {
        // #f{fixed;top:5;right:5;width:20} inside a relative parent → still uses
        // the viewport, so its margin-box right edge == viewport_width - 5.
        let (doc, t) = build(
            "<html><body><div id='p'>\
             <div id='f'>f</div>\
             </div></body></html>",
            "body{margin:0} #p{position:relative;margin:20px;padding:10px} \
             #f{position:fixed;top:5px;right:5px;width:20px;height:20px;margin:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.margin_box().x + f.dimensions.margin_box().width, 300.0 - 5.0);
        assert_eq!(f.dimensions.margin_box().y, 5.0);
    }

    #[test]
    fn float_does_not_advance_parent_height_no_regression_inflow() {
        // Sanity: an in-flow-only doc lays out identically whether or not the M2
        // float machinery is present (regression guard within layout).
        let html = "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>";
        let (doc, t) = build(html, "body{margin:0} div{margin:0;height:20px}");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 20.0);
    }

    // --- E2-M3: flexbox ---

    /// Three children of a 300-wide flex container with the given per-child CSS.
    fn flex3(container_css: &str, item_css: &str) -> (Document, StyledTree) {
        build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div>\
             </div></body></html>",
            &format!(
                "body{{margin:0}} #f{{margin:0;display:flex;width:300px;height:100px;{container_css}}} \
                 #a,#b,#c{{margin:0;{item_css}}}"
            ),
        )
    }

    fn flex_xs(root: &LayoutBox, doc: &Document) -> Vec<f32> {
        ["a", "b", "c"]
            .iter()
            .map(|id| box_for(root, find_id(doc, id)).unwrap().dimensions.content.x)
            .collect()
    }
    fn flex_ws(root: &LayoutBox, doc: &Document) -> Vec<f32> {
        ["a", "b", "c"]
            .iter()
            .map(|id| box_for(root, find_id(doc, id)).unwrap().dimensions.content.width)
            .collect()
    }

    #[test]
    fn flex_three_items_split_equally() {
        let (doc, t) = flex3("", "flex:1 1 0");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        assert_eq!(flex_ws(&root, &doc), vec![100.0, 100.0, 100.0]);
        assert_eq!(flex_xs(&root, &doc), vec![0.0, 100.0, 200.0]);
    }

    #[test]
    fn flex_three_items_split_with_gap() {
        // gap:30 → (300 - 60)/3 = 80; x = 0, 110, 220.
        let (doc, t) = flex3("gap:30px", "flex:1 1 0");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        assert_eq!(flex_ws(&root, &doc), vec![80.0, 80.0, 80.0]);
        assert_eq!(flex_xs(&root, &doc), vec![0.0, 110.0, 220.0]);
    }

    #[test]
    fn flex_grow_1_vs_2() {
        // Container 300, two items basis 0, grow 1 and 2 → 100 and 200.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:300px;height:50px} \
             #a{margin:0;flex:1 1 0} #b{margin:0;flex:2 1 0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.width, 100.0);
        assert_eq!(b.dimensions.content.width, 200.0);
        assert_eq!(b.dimensions.content.x, 100.0);
    }

    #[test]
    fn flex_shrink_proportional() {
        // Container 100, two items width 80, shrink 1 → each shrinks 30 → 50, 50.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:100px;height:50px} \
             #a{margin:0;width:80px} #b{margin:0;width:80px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.width, 50.0);
        assert_eq!(b.dimensions.content.width, 50.0);
    }

    #[test]
    fn flex_shrink_zero_keeps_size() {
        // Container 100, A width 80 shrink 0, B width 80 shrink 1 → A 80, B 20.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:100px;height:50px} \
             #a{margin:0;width:80px;flex-shrink:0} #b{margin:0;width:80px;flex-shrink:1}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.width, 80.0);
        assert_eq!(b.dimensions.content.width, 20.0);
    }

    #[test]
    fn justify_content_center() {
        // Container 300, two items width 50, no grow → lead 100 → x 100, 150.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:300px;height:50px;justify-content:center} \
             #a{margin:0;width:50px} #b{margin:0;width:50px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.x, 100.0);
        assert_eq!(b.dimensions.content.x, 150.0);
    }

    #[test]
    fn justify_content_space_between() {
        // Container 300, three items width 50 → between 75 → x 0, 125, 250.
        let (doc, t) = flex3("justify-content:space-between", "width:50px");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        assert_eq!(flex_xs(&root, &doc), vec![0.0, 125.0, 250.0]);
    }

    #[test]
    fn justify_content_space_around() {
        // Container 300, two items width 50 → leftover 200, between 100, lead 50.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:300px;height:50px;justify-content:space-around} \
             #a{margin:0;width:50px} #b{margin:0;width:50px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.x, 50.0);
        assert_eq!(b.dimensions.content.x, 200.0);
    }

    #[test]
    fn justify_content_space_evenly() {
        // Container 300, two items width 50 → between 200/3, lead 200/3.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:300px;height:50px;justify-content:space-evenly} \
             #a{margin:0;width:50px} #b{margin:0;width:50px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert!((a.dimensions.content.x - 200.0 / 3.0).abs() < 0.01);
        assert!((b.dimensions.content.x - (200.0 / 3.0 + 50.0 + 200.0 / 3.0)).abs() < 0.01);
    }

    #[test]
    fn align_items_center_cross() {
        // Row container height 100, item height 40 → content.y = 30 (centered).
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:200px;height:100px;align-items:center} \
             #a{margin:0;width:50px;height:40px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.y, 30.0);
    }

    #[test]
    fn align_items_stretch_default() {
        // Row container height 100. Item A auto height → fills 100; B height 40.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:200px;height:100px} \
             #a{margin:0;width:50px} #b{margin:0;width:50px;height:40px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.height, 100.0);
        assert_eq!(b.dimensions.content.height, 40.0);
    }

    #[test]
    fn align_self_override() {
        // Container align-items flex-start; A overrides to flex-end → A at bottom.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:200px;height:100px;align-items:flex-start} \
             #a{margin:0;width:50px;height:40px;align-self:flex-end} \
             #b{margin:0;width:50px;height:40px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        // A flex-end: y = 100 - 40 = 60. B flex-start: y = 0.
        assert_eq!(a.dimensions.content.y, 60.0);
        assert_eq!(b.dimensions.content.y, 0.0);
    }

    #[test]
    fn flex_direction_column_stacks() {
        // Column, three items height 30, gap 0 → y = 0, 30, 60; container h 90.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-direction:column;width:200px} \
             #a,#b,#c{margin:0;height:30px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let ys: Vec<f32> = ["a", "b", "c"]
            .iter()
            .map(|id| box_for(&root, find_id(&doc, id)).unwrap().dimensions.content.y)
            .collect();
        assert_eq!(ys, vec![0.0, 30.0, 60.0]);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.content.height, 90.0);
    }

    #[test]
    fn flex_direction_row_reverse() {
        // Two items width 50 in container 300 → first DOM child sits at the right.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-direction:row-reverse;width:300px;height:50px} \
             #a{margin:0;width:50px} #b{margin:0;width:50px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        // row-reverse: first DOM child (a) at the main-start = right end.
        assert!(a.dimensions.content.x > b.dimensions.content.x);
        assert_eq!(a.dimensions.content.x, 250.0);
        assert_eq!(b.dimensions.content.x, 200.0);
    }

    #[test]
    fn flex_direction_column_reverse_auto_height() {
        // column-reverse, auto height, two items height 30 → first DOM child (a) at
        // the bottom, b above it; container content height 60; both Ys non-negative.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-direction:column-reverse;width:200px} \
             #a{margin:0;height:30px} #b{margin:0;height:30px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        // Reversed: a (first DOM child) sits at the bottom, b above.
        assert_eq!(a.dimensions.content.y, 30.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        assert!(a.dimensions.content.y >= 0.0);
        assert!(b.dimensions.content.y >= 0.0);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.content.height, 60.0);
    }

    #[test]
    fn flex_direction_column_reverse_explicit_height() {
        // column-reverse with explicit height: items still mirror against the
        // container main size, anchored to the bottom (regression guard).
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-direction:column-reverse;width:200px;height:100px} \
             #a{margin:0;height:30px} #b{margin:0;height:30px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        // Mirrored against 100px: a at bottom (70), b above it (40).
        assert_eq!(a.dimensions.content.y, 70.0);
        assert_eq!(b.dimensions.content.y, 40.0);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.content.height, 100.0);
    }

    #[test]
    fn flex_direction_row_reverse_unaffected() {
        // Sanity: row-reverse main axis is the definite content width; positions
        // unchanged by the column-reverse fix.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-direction:row-reverse;width:300px;height:50px} \
             #a{margin:0;width:50px} #b{margin:0;width:50px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.x, 250.0);
        assert_eq!(b.dimensions.content.x, 200.0);
    }

    #[test]
    fn flex_wrap_second_line() {
        // Container 250, three items width 100, wrap → two on line 0, third wraps.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;flex-wrap:wrap;width:250px} \
             #a,#b,#c{margin:0;width:100px;height:20px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        let c = box_for(&root, find_id(&doc, "c")).unwrap();
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 100.0);
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        // Third wraps to line 1.
        assert_eq!(c.dimensions.content.x, 0.0);
        assert_eq!(c.dimensions.content.y, 20.0);
        let f = box_for(&root, find_id(&doc, "f")).unwrap();
        assert_eq!(f.dimensions.content.height, 40.0);
    }

    #[test]
    fn nested_flex() {
        // Outer row 300 with one flex:1 item that is itself display:flex with two
        // flex:1 children → outer item 300; inner children each 150.
        let (doc, t) = build(
            "<html><body><div id='outer'>\
             <div id='item'><div id='x'>x</div><div id='y'>y</div></div>\
             </div></body></html>",
            "body{margin:0} #outer{margin:0;display:flex;width:300px;height:50px} \
             #item{margin:0;flex:1 1 0;display:flex;height:50px} \
             #x{margin:0;flex:1 1 0} #y{margin:0;flex:1 1 0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let item = box_for(&root, find_id(&doc, "item")).unwrap();
        let x = box_for(&root, find_id(&doc, "x")).unwrap();
        let y = box_for(&root, find_id(&doc, "y")).unwrap();
        assert_eq!(item.dimensions.content.width, 300.0);
        assert_eq!(x.dimensions.content.width, 150.0);
        assert_eq!(y.dimensions.content.width, 150.0);
        assert_eq!(x.dimensions.content.x, 0.0);
        assert_eq!(y.dimensions.content.x, 150.0);
    }

    #[test]
    fn flex_basis_explicit_vs_auto() {
        // A flex-basis:120px (no grow/shrink) keeps 120; B width:60 keeps 60.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:300px;height:50px} \
             #a{margin:0;flex:0 0 120px} #b{margin:0;width:60px;flex-shrink:0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.width, 120.0);
        assert_eq!(b.dimensions.content.width, 60.0);
        assert_eq!(b.dimensions.content.x, 120.0);
    }

    #[test]
    fn inline_flex_lays_out_children() {
        // An inline-flex container is an atomic inline; its two flex:1 children
        // still split its width.
        let (doc, t) = build(
            "<html><body><div id='wrap'><span id='f'>\
             <div id='a'>a</div><div id='b'>b</div></span></div></body></html>",
            "body{margin:0} #wrap{margin:0} \
             #f{margin:0;display:inline-flex;width:200px;height:30px} \
             #a{margin:0;flex:1 1 0} #b{margin:0;flex:1 1 0}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.width, 100.0);
        assert_eq!(b.dimensions.content.width, 100.0);
    }

    #[test]
    fn flex_no_regression_normal_block() {
        // A normal block/inline doc lays out identically regardless of the flex
        // machinery (regression guard).
        let html = "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>";
        let (doc, t) = build(html, "body{margin:0} div{margin:0;height:20px}");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 20.0);
        assert_eq!(a.dimensions.content.width, 300.0);
    }
}
