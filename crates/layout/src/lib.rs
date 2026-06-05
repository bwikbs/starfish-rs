//! starfish-layout (M4) — block + inline flow layout over a styled DOM.
//!
//! Given a [`Document`], a [`StyledTree`] and a viewport width, [`layout`]
//! builds a box tree and computes absolute geometry for every box. The result
//! is a walkable [`LayoutBox`] consumed by M5 (paint). See
//! `docs/design/M4-layout.md`.

mod block;
mod boxtree;
mod dimensions;
mod inline;
mod measure;

use starfish_dom::{Document, NodeKind};
use starfish_style::StyledTree;

pub use boxtree::{BoxKind, BoxStyleRef, LayoutBox};
pub use dimensions::{Dimensions, EdgeSizes, Rect};
pub use measure::{DefaultMeasurer, LineMetrics, TextMeasurer};
pub use starfish_dom::{Document as DomDocument, NodeId};
pub use starfish_style::{ComputedStyle, FontWeight};

use block::layout_block;
use boxtree::build_box_tree;

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

    layout_block(&mut root, initial_cb, styled, measurer);
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
}
