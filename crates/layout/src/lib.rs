//! starfish-layout (M4) — block + inline flow layout over a styled DOM.
//!
//! Given a [`Document`], a [`StyledTree`] and a viewport width, [`layout`]
//! builds a box tree and computes absolute geometry for every box. The result
//! is a walkable [`LayoutBox`] consumed by M5 (paint). See
//! `docs/design/M4-layout.md`.

mod block;
mod boxtree;
mod cache;
mod dimensions;
mod flex;
mod float;
mod grid;
mod inline;
mod measure;
mod table;

use starfish_dom::{Document, NodeKind};
use starfish_style::StyledTree;

pub use boxtree::{parse_view_box, BoxKind, BoxStyleRef, LayoutBox, ViewBox};
pub use dimensions::{Dimensions, EdgeSizes, Rect};
pub use measure::{
    extra_spacing, DefaultMeasurer, FontQuery, ImageSource, LineMetrics, NoImages, TextMeasurer,
};
pub use starfish_dom::{Document as DomDocument, NodeId};
pub use starfish_style::{ComputedStyle, FontStyle, FontWeight};

use block::{layout_absolutes, layout_block};
use boxtree::build_box_tree;
use cache::LayoutCache;
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
        match self.style {
            // E7-M2: generated boxes resolve via the pseudo side table.
            BoxStyleRef::Generated { origin, side } => styled.pseudo_style(origin, side),
            _ => styled.get(self.style.node()),
        }
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
    images: &dyn ImageSource,
) -> LayoutBox {
    let Some(root_el) = root_element(doc) else {
        return LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Anonymous(doc.root()));
    };

    let mut root = build_box_tree(doc, styled, root_el);

    let initial_cb = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width: viewport_width, height: 0.0 },
        ..Dimensions::default()
    };

    // E12-M1: one cache shared across this whole layout() pass (stack-local).
    let cache = LayoutCache::default();
    let mut floats = FloatContext::default();
    layout_block(&mut root, initial_cb, styled, doc, measurer, images, &mut floats, &cache);

    // Phase 2 (§4.2): position abs/fixed boxes against their containing block.
    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: viewport_width,
        height: root.dimensions.content.height,
    };
    layout_absolutes(&mut root, viewport, viewport, styled, doc, measurer, images, &cache);
    root
}

/// Convenience wrapper using [`DefaultMeasurer`] and no images.
pub fn layout_default(doc: &Document, styled: &StyledTree, viewport_width: f32) -> LayoutBox {
    layout(doc, styled, viewport_width, &DefaultMeasurer, &NoImages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;
    use starfish_style::style_tree;

    /// Fixed-width measurer: each char advances `per` px regardless of font.
    /// Makes wrap assertions exact.
    struct FixedMeasurer {
        per: f32,
    }
    impl TextMeasurer for FixedMeasurer {
        fn measure(&self, text: &str, _font: &FontQuery) -> f32 {
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 400.0);
    }

    #[test]
    fn fixed_width_padding_border_right_margin_absorbs() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{width:200px;padding:10px;border:5px solid black;margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let bd = box_for(&root, find_id(&doc, "bd")).unwrap();
        assert_eq!(bd.dimensions.content.height, 100.0);
    }

    #[test]
    fn explicit_height_overrides() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{height:120px;margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 120.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert_eq!(line.dimensions.content.height, 24.0); // 1.2 * 20

        let (doc2, t2) = build(
            "<html><body><p id='p'>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;line-height:30px}",
        );
        let root2 = layout(&doc2, &t2, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 50.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 40.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 100.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 200.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 200.0);
        assert_eq!(a.dimensions.margin.left, 0.0);
        assert_eq!(a.dimensions.margin.right, 0.0);

        // One auto margin with no slack → that margin is 0.
        let (doc2, t2) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0;width:200px} #a{width:200px;margin-left:auto;margin-right:0}",
        );
        let root2 = layout(&doc2, &t2, 200.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 70.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 250.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 80.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 400.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &m, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();

        let (doc0, t0) = build(html, css_base);
        let root0 = layout(&doc0, &t0, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        assert_eq!(flex_ws(&root, &doc), vec![100.0, 100.0, 100.0]);
        assert_eq!(flex_xs(&root, &doc), vec![0.0, 100.0, 200.0]);
    }

    #[test]
    fn flex_three_items_split_with_gap() {
        // gap:30 → (300 - 60)/3 = 80; x = 0, 110, 220.
        let (doc, t) = flex3("gap:30px", "flex:1 1 0");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.x, 100.0);
        assert_eq!(b.dimensions.content.x, 150.0);
    }

    #[test]
    fn justify_content_space_between() {
        // Container 300, three items width 50 → between 75 → x 0, 125, 250.
        let (doc, t) = flex3("justify-content:space-between", "width:50px");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
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
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 20.0);
        assert_eq!(a.dimensions.content.width, 300.0);
    }

    // --- E2-M4: <img> replaced elements ---

    /// Stub image source: `"img"` is a 40×20 image; everything else is broken.
    struct StubImages;
    impl ImageSource for StubImages {
        fn intrinsic_size(&self, src: &str) -> Option<(f32, f32)> {
            if src == "img" {
                Some((40.0, 20.0))
            } else {
                None
            }
        }
    }

    /// The single `Image` box in a tree.
    fn image_box(b: &LayoutBox) -> &LayoutBox {
        let mut v = Vec::new();
        collect_kind(b, BoxKind::Image, &mut v);
        assert_eq!(v.len(), 1, "expected exactly one Image box");
        v[0]
    }

    #[test]
    fn img_intrinsic_size_no_attrs() {
        let (doc, t) = build(
            "<html><body><p id='p'><img src='img'></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 40.0);
        assert_eq!(img.dimensions.content.height, 20.0);
        // its enclosing LineBox is at least the image height tall.
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        assert!(line.dimensions.content.height >= 20.0);
    }

    #[test]
    fn img_both_attrs_used_verbatim() {
        let (doc, t) = build(
            "<html><body><p><img src='img' width='100' height='50'></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 100.0);
        assert_eq!(img.dimensions.content.height, 50.0);
    }

    #[test]
    fn img_one_attr_scales_preserving_aspect() {
        // width=80 with intrinsic 40×20 (aspect 2:1) → height 40.
        let (doc, t) = build(
            "<html><body><p><img src='img' width='80'></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 80.0);
        assert_eq!(img.dimensions.content.height, 40.0);
    }

    #[test]
    fn img_height_attr_scales_width() {
        // height=10 with intrinsic 40×20 → width 20.
        let (doc, t) = build(
            "<html><body><p><img src='img' height='10'></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 20.0);
        assert_eq!(img.dimensions.content.height, 10.0);
    }

    #[test]
    fn img_broken_no_attrs_is_zero_box() {
        let (doc, t) = build(
            "<html><body><p><img src='missing'></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 0.0);
        assert_eq!(img.dimensions.content.height, 0.0);
    }

    #[test]
    fn img_css_width_overrides_attr() {
        // CSS width 64px wins over the HTML width=10 attr; aspect → height 32.
        let (doc, t) = build(
            "<html><body><p><img id='i' src='img' width='10'></p></body></html>",
            "body{margin:0} p{margin:0} #i{width:64px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 64.0);
        assert_eq!(img.dimensions.content.height, 32.0);
    }

    #[test]
    fn img_inline_flows_in_line_with_text() {
        // <p>hi <img w20 h20> there</p> — the image is an atom between the words.
        let (doc, t) = build(
            "<html><body><p id='p'>hi <img src='img' width='20' height='20'> there</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &StubImages);
        let p = box_for(&root, find_id(&doc, "p")).unwrap();
        let line = p.children.iter().find(|c| c.kind == BoxKind::LineBox).unwrap();
        // line has: "hi" TextRun, Image atom, "there" TextRun (3 fragments).
        let img = line.children.iter().find(|c| c.kind == BoxKind::Image).unwrap();
        assert_eq!(img.dimensions.content.width, 20.0);
        // image sits to the right of "hi" (which is 2 chars * 10 = 20 wide).
        let hi = line.children.iter().find(|c| c.text() == Some("hi")).unwrap();
        assert!(img.dimensions.content.x >= hi.dimensions.content.x + hi.dimensions.content.width);
    }

    #[test]
    fn img_display_block_on_own_line() {
        // A display:block img between two paragraphs lands alone on its line.
        let (doc, t) = build(
            "<html><body><div id='d'><p>a</p><img src='img'><p>b</p></div></body></html>",
            "body{margin:0} div{margin:0} p{margin:0} img{display:block}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        // it has its intrinsic size and didn't panic.
        assert_eq!(img.dimensions.content.width, 40.0);
        assert_eq!(img.dimensions.content.height, 20.0);
    }

    #[test]
    fn img_without_src_produces_no_box() {
        let (doc, t) = build(
            "<html><body><p><img></p></body></html>",
            "body{margin:0} p{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let mut v = Vec::new();
        collect_kind(&root, BoxKind::Image, &mut v);
        assert!(v.is_empty(), "img without src should produce no Image box");
    }

    // --- E5-M1: grid layout ---

    /// x/y of a grid item by id.
    fn item_xy(root: &LayoutBox, doc: &Document, id: &str) -> (f32, f32) {
        let b = box_for(root, find_id(doc, id)).unwrap();
        (b.dimensions.content.x, b.dimensions.content.y)
    }
    fn item_wh(root: &LayoutBox, doc: &Document, id: &str) -> (f32, f32) {
        let b = box_for(root, find_id(doc, id)).unwrap();
        (b.dimensions.content.width, b.dimensions.content.height)
    }

    #[test]
    fn grid_fixed_2x2() {
        // 100px 100px cols, 50px 50px rows, 4 items, row-major auto-placement.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div>\
             <div id='c'>c</div><div id='d'>d</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px 50px;gap:0} \
             #a,#b,#c,#d{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a"), (0.0, 0.0));
        assert_eq!(item_xy(&root, &doc, "b"), (100.0, 0.0));
        assert_eq!(item_xy(&root, &doc, "c"), (0.0, 50.0));
        assert_eq!(item_xy(&root, &doc, "d"), (100.0, 50.0));
        assert_eq!(item_wh(&root, &doc, "a"), (100.0, 50.0));
    }

    #[test]
    fn grid_fr_thirds() {
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:1fr 1fr 1fr;gap:0} #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 100.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 200.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 100.0);
    }

    #[test]
    fn grid_fr_thirds_with_column_gap() {
        // (300 - 2*30)/3 = 80; x = 0, 110, 220.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:1fr 1fr 1fr;column-gap:30px;row-gap:0} #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 110.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 220.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 80.0);
    }

    #[test]
    fn grid_repeat_equals_fr() {
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:repeat(3, 1fr);gap:0} #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "b").0, 100.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 200.0);
    }

    #[test]
    fn grid_px_then_fr_fills() {
        // 100px 1fr in a 300 container → col0 100, col1 200.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:100px 1fr;gap:0} #a,#b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "b").0, 100.0);
        assert_eq!(item_wh(&root, &doc, "b").0, 200.0);
    }

    #[test]
    fn grid_explicit_column_span() {
        // grid-column: 1/3 spans two 100px tracks + one 10px interior gap = 210.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:320px;\
             grid-template-columns:100px 100px 100px;column-gap:10px;row-gap:0} \
             #a{margin:0;grid-column:1 / 3}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 210.0);
    }

    #[test]
    fn grid_row_placement_explicit() {
        // item explicitly placed in row 2 → y = row0 height + row_gap.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;grid-template-rows:40px 40px;row-gap:10px;column-gap:0} \
             #a{margin:0;grid-row:2}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        // a in row 2 (col 0): y = 40 + 10 = 50.
        assert_eq!(item_xy(&root, &doc, "a").1, 50.0);
    }

    #[test]
    fn grid_row_span_two() {
        // single item spanning two explicit rows → height = 40+40+gap = 90.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='s'>s</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;grid-template-rows:40px 40px;row-gap:10px;column-gap:0} \
             #s{margin:0;grid-row:span 2}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        // s spans rows 1-2: height = 40+40+10 = 90.
        assert_eq!(item_wh(&root, &doc, "s").1, 90.0);
    }

    #[test]
    fn grid_auto_placement_wraps_implicit_row() {
        // 2 cols, 3 items, none placed → item2 wraps to a new implicit row.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:repeat(2, 100px);grid-template-rows:30px;gap:0} \
             #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a"), (0.0, 0.0));
        assert_eq!(item_xy(&root, &doc, "b"), (100.0, 0.0));
        // c wraps to the implicit second row at x=0.
        assert_eq!(item_xy(&root, &doc, "c").0, 0.0);
        assert!(item_xy(&root, &doc, "c").1 >= 30.0);
    }

    #[test]
    fn grid_auto_skips_explicitly_placed_cell() {
        // A is explicitly at col 2 row 1; B,C auto → B at (col0,row0), C wraps.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='b'>b</div><div id='a'>a</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:30px;gap:0} \
             #a{margin:0;grid-column:2;grid-row:1} #b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        // A explicit at col1 row0.
        assert_eq!(item_xy(&root, &doc, "a"), (100.0, 0.0));
        // B fills (col0,row0) skipping A's cell.
        assert_eq!(item_xy(&root, &doc, "b"), (0.0, 0.0));
        // C wraps to implicit row.
        assert!(item_xy(&root, &doc, "c").1 >= 30.0);
    }

    #[test]
    fn grid_gap_adds_spacing() {
        // 100px 100px cols, column-gap 20, row-gap 10, 50px rows.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div>\
             <div id='c'>c</div><div id='d'>d</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:220px;\
             grid-template-columns:100px 100px;grid-template-rows:50px 50px;\
             column-gap:20px;row-gap:10px} #a,#b,#c,#d{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "b").0, 120.0);
        assert_eq!(item_xy(&root, &doc, "c").1, 60.0);
        assert_eq!(item_xy(&root, &doc, "d"), (120.0, 60.0));
    }

    #[test]
    fn grid_container_auto_height() {
        // No explicit container height → height = sum rows + gaps.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;grid-template-rows:30px 30px;row-gap:10px;column-gap:0} \
             #a,#b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let g = box_for(&root, find_id(&doc, "g")).unwrap();
        assert_eq!(g.dimensions.content.height, 70.0);
    }

    #[test]
    fn grid_implicit_row_auto_height() {
        // 1 col, 2 items each content height 25 (1 line at fs 25) → rows 25 each.
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;gap:0} \
             #a,#b{margin:0;font-size:25px;line-height:25px}",
        );
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let g = box_for(&root, find_id(&doc, "g")).unwrap();
        // two implicit auto rows of 25 each → container height 50.
        assert_eq!(g.dimensions.content.height, 50.0);
        assert_eq!(item_xy(&root, &doc, "b").1, 25.0);
    }

    #[test]
    fn grid_auto_column_max_content() {
        // auto 1fr, container 300, item in col0 with text "abcdef" at per=10 → 60.
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>abcdef</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:auto 1fr;gap:0} \
             #a,#b{margin:0;font-size:10px;line-height:10px}",
        );
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        assert_eq!(item_wh(&root, &doc, "a").0, 60.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 60.0);
        assert_eq!(item_wh(&root, &doc, "b").0, 240.0);
    }

    #[test]
    fn grid_percent_track() {
        // 50% 50% in a 300 container → 150 each.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:50% 50%;gap:0} #a,#b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_wh(&root, &doc, "a").0, 150.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 150.0);
    }

    #[test]
    fn grid_nested() {
        // Outer 1 col of 200px; the single item is itself a grid of 1fr 1fr →
        // inner items each 100 wide.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='inner'><div id='x'>x</div><div id='y'>y</div></div>\
             </div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:200px;gap:0} \
             #inner{margin:0;display:grid;grid-template-columns:1fr 1fr;gap:0} \
             #x,#y{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_wh(&root, &doc, "x").0, 100.0);
        assert_eq!(item_xy(&root, &doc, "y").0, 100.0);
        assert_eq!(item_wh(&root, &doc, "y").0, 100.0);
    }

    #[test]
    fn grid_no_regression_non_grid_page() {
        // A non-grid page must lay out identically (regression guard).
        let html = "<html><body><div id='a'>x</div><div id='b'>y</div></body></html>";
        let (doc, t) = build(html, "body{margin:0} div{margin:0;height:20px}");
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        let b = box_for(&root, find_id(&doc, "b")).unwrap();
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 20.0);
    }

    // --- E5-M2: grid alignment + named areas ---

    #[test]
    fn grid_justify_align_items_center() {
        // 100x50 single cell, item 40x20, centered both axes.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;grid-template-rows:50px;gap:0;\
             justify-items:center;align-items:center} \
             #a{margin:0;width:40px;height:20px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a"), (30.0, 15.0));
        assert_eq!(item_wh(&root, &doc, "a"), (40.0, 20.0));
    }

    #[test]
    fn grid_align_self_overrides_container() {
        // container align-items:start; item a overrides to end (height 20) → y=30;
        // sibling b without align-self sits at y=0.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px;gap:0;\
             align-items:start} \
             #a{margin:0;height:20px;align-self:end} \
             #b{margin:0;height:20px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").1, 30.0);
        assert_eq!(item_xy(&root, &doc, "b").1, 0.0);
    }

    #[test]
    fn grid_justify_self_start_vs_stretch_default() {
        // item a justify-self:start width:30 → width 30 at x=0; item b default
        // (justify-items initial stretch) → fills its cell (100).
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px;gap:0} \
             #a{margin:0;width:30px;justify-self:start} #b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 30.0);
        assert_eq!(item_wh(&root, &doc, "b").0, 100.0);
    }

    #[test]
    fn grid_justify_self_end() {
        // item end-aligned in a 100px cell, width 40 → x = 60.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;\
             grid-template-columns:100px;grid-template-rows:50px;gap:0} \
             #a{margin:0;width:40px;justify-self:end}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 60.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 40.0);
    }

    #[test]
    fn grid_auto_width_non_stretch_both_axes() {
        // Auto-width item (no explicit width) with padding, non-stretch on BOTH
        // the inline (justify-self:start) and block (align-self:start) axes, in a
        // cell larger than its content. The block-axis path runs
        // `measure_item_height`, whose internal `layout_block` re-resolves the
        // auto width and clobbers `content.width`; it must be re-pinned to the
        // justify-resolved intrinsic width.
        //
        // text "a" at per=10 → intrinsic content width 10 (padding sits outside
        // the content box). Without the re-pin the block-axis measurement
        // clobbers it down to 5 (cb 15 − padding 10). The default-stretch sibling
        // must still fill its cell.
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px;gap:0} \
             #a{margin:0;padding:5px;font-size:10px;line-height:10px;\
             justify-self:start;align-self:start} #b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let (aw, ah) = item_wh(&root, &doc, "a");
        // content.width is the justify-resolved intrinsic width (10, the text's
        // own extent) — not the cell width (100) and not the clobbered
        // auto-layout width (5).
        assert_eq!(aw, 10.0);
        // content.height is the natural content height, not the stretched cell
        // height (50): align-self:start was honored.
        assert!(ah > 0.0 && ah < 50.0, "non-stretch height should be intrinsic, got {ah}");
        // No regression: the default (stretch/stretch) sibling still fills its cell.
        assert_eq!(item_wh(&root, &doc, "b"), (100.0, 50.0));
    }

    #[test]
    fn grid_stretch_default_no_regression() {
        // No alignment props → E5-M1 stretch behavior (items fill 100x50).
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div>\
             <div id='c'>c</div><div id='d'>d</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px 50px;gap:0} \
             #a,#b,#c,#d{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_wh(&root, &doc, "a"), (100.0, 50.0));
        assert_eq!(item_wh(&root, &doc, "d"), (100.0, 50.0));
    }

    #[test]
    fn grid_justify_content_space_between() {
        // 3 tracks of 50 in a 300 container, gap 0, space-between → extra 150,
        // between = 75 → x = 0, 125, 250.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:50px 50px 50px;gap:0;justify-content:space-between} \
             #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 125.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 250.0);
        assert_eq!(item_wh(&root, &doc, "a").0, 50.0);
    }

    #[test]
    fn grid_justify_content_center() {
        // 3x50 tracks, 300 container, center → lead 75 → x = 75, 125, 175.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:50px 50px 50px;gap:0;justify-content:center} \
             #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 75.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 125.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 175.0);
    }

    #[test]
    fn grid_justify_content_end() {
        // 3x50 tracks, 300 container, end → lead 150 → x = 150, 200, 250.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:50px 50px 50px;gap:0;justify-content:end} \
             #a,#b,#c{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 150.0);
        assert_eq!(item_xy(&root, &doc, "c").0, 250.0);
    }

    #[test]
    fn grid_align_content_center_definite_height() {
        // 200px tall container, 2x50 rows, row-gap 0, align-content:center →
        // extra 100, lead 50 → row0 y = 50, row1 y = 100.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:100px;height:200px;\
             grid-template-columns:100px;grid-template-rows:50px 50px;gap:0;\
             align-content:center} #a,#b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").1, 50.0);
        assert_eq!(item_xy(&root, &doc, "b").1, 100.0);
    }

    #[test]
    fn grid_justify_content_fr_no_distribution() {
        // fr tracks already fill → extra ≈ 0, space-between is a no-op.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:300px;\
             grid-template-columns:1fr 1fr;gap:0;justify-content:space-between} \
             #a,#b{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a").0, 0.0);
        assert_eq!(item_xy(&root, &doc, "b").0, 150.0);
    }

    #[test]
    fn grid_named_area_placement() {
        // areas "a b" / "a c" → a spans rows 0..2 col0; b at (100,0); c at (100,30).
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:30px 30px;gap:0;\
             grid-template-areas:\"a b\" \"a c\"} \
             #a{margin:0;grid-area:a} #b{margin:0;grid-area:b} #c{margin:0;grid-area:c}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a"), (0.0, 0.0));
        assert_eq!(item_wh(&root, &doc, "a"), (100.0, 60.0));
        assert_eq!(item_xy(&root, &doc, "b"), (100.0, 0.0));
        assert_eq!(item_wh(&root, &doc, "b"), (100.0, 30.0));
        assert_eq!(item_xy(&root, &doc, "c"), (100.0, 30.0));
    }

    #[test]
    fn grid_named_area_multi_column() {
        // header spans both columns of the top row → width 200.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='hd'>h</div><div id='main'>m</div><div id='side'>s</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:30px 30px;gap:0;\
             grid-template-areas:\"hd hd\" \"main side\"} \
             #hd{margin:0;grid-area:hd} #main{margin:0;grid-area:main} \
             #side{margin:0;grid-area:side}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "hd"), (0.0, 0.0));
        assert_eq!(item_wh(&root, &doc, "hd").0, 200.0);
        assert_eq!(item_xy(&root, &doc, "main"), (0.0, 30.0));
        assert_eq!(item_xy(&root, &doc, "side"), (100.0, 30.0));
    }

    #[test]
    fn grid_named_area_missing_auto_places() {
        // grid-area:nope is unknown → auto-placement; layout completes, valid cell.
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:30px;gap:0;\
             grid-template-areas:\"x y\"} \
             #a{margin:0;grid-area:nope}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        // falls to first free cell (0,0).
        assert_eq!(item_xy(&root, &doc, "a"), (0.0, 0.0));
    }

    #[test]
    fn grid_fixed_row_no_room_advances() {
        // a occupies whole row 0 (cols 0..2); b is locked to row 1 (grid-row:1)
        // with auto column → no free col → must advance to implicit row 1, not
        // overlap a at (0,0). Assert b.y == 30 (row 1).
        let (doc, t) = build(
            "<html><body><div id='g'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:30px;gap:0} \
             #a{margin:0;grid-row:1;grid-column:1 / 3} #b{margin:0;grid-row:1}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        assert_eq!(item_xy(&root, &doc, "a"), (0.0, 0.0));
        assert_eq!(item_xy(&root, &doc, "b").1, 30.0);
    }

    // --- E6-M3: white-space wrapping + bidi + spacing ---

    /// The LineBoxes of element `id`'s inline content.
    fn lines_of<'a>(root: &'a LayoutBox, doc: &Document, id: &str) -> Vec<&'a LayoutBox> {
        let p = box_for(root, find_id(doc, id)).unwrap();
        p.children
            .iter()
            .filter(|c| c.kind == BoxKind::LineBox)
            .collect()
    }

    #[test]
    fn nowrap_does_not_wrap() {
        // Same long string that wraps under normal → one line under nowrap.
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb ccc ddd eee fff ggg</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:nowrap}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 120.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines.len(), 1, "nowrap must not wrap");
    }

    #[test]
    fn pre_preserves_newline_two_lines() {
        let (doc, t) = build(
            "<html><body><p id='p'>a\nb</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:pre}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines.len(), 2, "pre \\n → two lines");
        assert_eq!(lines[0].children[0].text(), Some("a"));
        assert_eq!(lines[1].children[0].text(), Some("b"));
    }

    #[test]
    fn pre_no_soft_wrap_even_when_long() {
        // A long line under pre stays on one line (no soft wrap).
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb ccc ddd eee</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:pre}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 50.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines.len(), 1, "pre overflows, no soft wrap");
    }

    #[test]
    fn pre_preserves_leading_spaces() {
        // "  ab" under pre: the first fragment's x reflects the 2 leading spaces.
        let (doc, t) = build(
            "<html><body><p id='p'>  ab</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:pre}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frag = &lines[0].children[0];
        assert_eq!(frag.text(), Some("ab"));
        // 2 leading spaces * 10px = 20px indent.
        assert_eq!(frag.dimensions.content.x, 20.0);
    }

    #[test]
    fn pre_wrap_preserves_and_wraps() {
        // pre-wrap: preserved \n splits, and long content still soft-wraps.
        let (doc, t) = build(
            "<html><body><p id='p'>aa bb cc dd\nee</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:pre-wrap}",
        );
        let m = FixedMeasurer { per: 10.0 };
        // band fits ~2 words/line.
        let root = layout(&doc, &t, 60.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        // first segment soft-wraps to >1 line; the \n forces another line for "ee".
        assert!(lines.len() >= 3, "pre-wrap wraps + preserves \\n: {}", lines.len());
        assert_eq!(lines.last().unwrap().children[0].text(), Some("ee"));
    }

    #[test]
    fn pre_line_collapses_spaces_keeps_newline() {
        let (doc, t) = build(
            "<html><body><p id='p'>a   b\nc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;white-space:pre-line}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines.len(), 2, "pre-line keeps \\n");
        // spaces collapsed: line 0 is "a" + "b" (2 frags), the gap = 1 space.
        assert_eq!(lines[0].children.len(), 2);
        assert_eq!(lines[0].children[1].dimensions.content.x, 20.0); // "a"(10) + space(10)
    }

    #[test]
    fn text_transform_uppercase_in_box() {
        let (doc, t) = build(
            "<html><body><p id='p'>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;text-transform:uppercase}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines[0].children[0].text(), Some("HI"));
    }

    #[test]
    fn letter_spacing_widens_word_width() {
        let (doc, t) = build(
            "<html><body><p id='p'>abc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;letter-spacing:5px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        // FixedMeasurer ignores spacing, but DefaultMeasurer/real ones add it.
        // Use the default measurer path to assert the additive width.
        let root2 = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let lines2 = lines_of(&root2, &doc, "p");
        let w_spaced = lines2[0].children[0].dimensions.content.width;
        // base "abc" via DefaultMeasurer = 3*0.5*10 = 15; + 3*5 = 30.
        assert_eq!(w_spaced, 30.0);
        // and the FixedMeasurer line exists (smoke).
        assert_eq!(lines[0].children[0].text(), Some("abc"));
    }

    #[test]
    fn hebrew_line_reverses_to_visual_order() {
        // A pure-Hebrew word in an LTR block. Post E10-M2 the fragment text is
        // kept in LOGICAL order (real shaping infers RTL + emits glyphs visually
        // itself); a single word has no neighbours to reorder, so its placement
        // is unchanged (left edge of the line, x == l_start).
        let hebrew = "\u{05D0}\u{05D1}\u{05D2}"; // אבג
        let html = format!("<html><body><p id='p'>{hebrew}</p></body></html>");
        let (doc, t) = build(&html, "body{margin:0} p{margin:0;font-size:10px}");
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frag = &lines[0].children[0];
        assert_eq!(frag.text(), Some(hebrew), "RTL word text stays logical");
        assert_eq!(frag.dimensions.content.x, lines[0].dimensions.content.x);
    }

    #[test]
    fn rtl_base_right_aligns_ltr_word() {
        // direction:rtl on an LTR word → the word sits at the right edge.
        let (doc, t) = build(
            "<html><body><p id='p'>abc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;direction:rtl}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frag = &lines[0].children[0];
        // width 30; container content width ~ 200 → x near right edge.
        let line_right = lines[0].dimensions.content.x + lines[0].dimensions.content.width;
        let frag_right = frag.dimensions.content.x + frag.dimensions.content.width;
        assert!(
            (frag_right - line_right).abs() < 1.0,
            "RTL base puts the word at the right: frag_right={frag_right} line_right={line_right}"
        );
    }

    #[test]
    fn mixed_bidi_visual_order() {
        // "abc אבג 123" base LTR. Per the bidi algorithm the visual order is
        // abc (left), 123, then the Hebrew run (right) — the trailing number
        // stays an LTR run between the latin and the Hebrew. Post E10-M2 the
        // fragment TEXT is logical (shaping handles glyph visual order), so we
        // assert the fragment x order + logical text per slot.
        let hebrew = "\u{05D0}\u{05D1}\u{05D2}";
        let html = format!("<html><body><p id='p'>abc {hebrew} 123</p></body></html>");
        let (doc, t) = build(&html, "body{margin:0} p{margin:0;font-size:10px}");
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frags = &lines[0].children;
        assert_eq!(frags.len(), 3);
        let mut by_x: Vec<&LayoutBox> = frags.iter().collect();
        by_x.sort_by(|a, b| {
            a.dimensions.content.x.partial_cmp(&b.dimensions.content.x).unwrap()
        });
        // abc is leftmost; the Hebrew run is rightmost (text stays logical).
        assert_eq!(by_x[0].text(), Some("abc"));
        assert_eq!(by_x[2].text(), Some(hebrew));
        // the middle fragment is the LTR number run.
        assert_eq!(by_x[1].text(), Some("123"));
    }

    #[test]
    fn bidi_override_reverses_whole_line() {
        // direction:rtl + unicode-bidi:bidi-override. Post E10-M2 the override
        // reverses run/WORD order (not glyphs) and keeps logical text; shaping
        // handles intra-word glyph order. A single LTR word "abc" thus keeps its
        // logical text and right-aligns (RTL base). The two-word ordering case is
        // covered by `bidi_override_multiword_spacing_preserved`.
        let (doc, t) = build(
            "<html><body><p id='p'>abc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;direction:rtl;unicode-bidi:bidi-override}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frag = &lines[0].children[0];
        assert_eq!(frag.text(), Some("abc"), "bidi-override keeps logical word text");
        // RTL base right-aligns the single word.
        let line_right = lines[0].dimensions.content.x + lines[0].dimensions.content.width;
        let frag_right = frag.dimensions.content.x + frag.dimensions.content.width;
        assert!((frag_right - line_right).abs() < 1.0);
    }

    #[test]
    fn rtl_two_words_preserve_interword_space() {
        // direction:rtl with two Hebrew words: the inter-word space must survive
        // the bidi reversal (regression for the dropped-gap bug). Visual order is
        // [second word | space | first word]; the two fragments must not touch.
        let aa = "\u{05D0}\u{05D0}"; // אא (2 chars → 20px)
        let bb = "\u{05D1}\u{05D1}"; // בב (2 chars → 20px)
        let html = format!("<html><body><p id='p'>{aa} {bb}</p></body></html>");
        let (doc, t) = build(&html, "body{margin:0} p{margin:0;font-size:10px;direction:rtl}");
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frags = &lines[0].children;
        assert_eq!(frags.len(), 2);
        // Sort by x: left fragment then right fragment in visual order.
        let mut by_x: Vec<&LayoutBox> = frags.iter().collect();
        by_x.sort_by(|a, b| {
            a.dimensions.content.x.partial_cmp(&b.dimensions.content.x).unwrap()
        });
        let left = &by_x[0].dimensions.content;
        let right = &by_x[1].dimensions.content;
        let gap = right.x - (left.x + left.width);
        // One LTR space width (10px) preserved between the two words.
        assert_eq!(gap, 10.0, "RTL inter-word space must be preserved, got gap={gap}");
    }

    #[test]
    fn rtl_three_words_preserve_both_gaps() {
        // A three-word RTL run must keep BOTH inter-word spaces (10px each).
        let w0 = "\u{05D0}\u{05D0}"; // אא
        let w1 = "\u{05D1}\u{05D1}"; // בב
        let w2 = "\u{05D2}\u{05D2}"; // גג
        let html = format!("<html><body><p id='p'>{w0} {w1} {w2}</p></body></html>");
        let (doc, t) = build(&html, "body{margin:0} p{margin:0;font-size:10px;direction:rtl}");
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frags = &lines[0].children;
        assert_eq!(frags.len(), 3);
        let mut by_x: Vec<&LayoutBox> = frags.iter().collect();
        by_x.sort_by(|a, b| {
            a.dimensions.content.x.partial_cmp(&b.dimensions.content.x).unwrap()
        });
        // Both boundaries between visually-adjacent fragments hold a 10px space.
        for pair in by_x.windows(2) {
            let l = &pair[0].dimensions.content;
            let r = &pair[1].dimensions.content;
            let gap = r.x - (l.x + l.width);
            assert_eq!(gap, 10.0, "each RTL inter-word gap is one space, got {gap}");
        }
    }

    #[test]
    fn ltr_two_words_unchanged() {
        // NO-REGRESSION: plain LTR "aa bb" → x=0 and x=30 (20px word + 10px space).
        let (doc, t) = build(
            "<html><body><p id='p'>aa bb</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines[0].children[0].dimensions.content.x, 0.0);
        assert_eq!(lines[0].children[1].dimensions.content.x, 30.0);
    }

    #[test]
    fn rtl_single_word_unchanged() {
        // NO-REGRESSION: a single RTL word right-aligns; no spurious gap.
        let aa = "\u{05D0}\u{05D0}";
        let html = format!("<html><body><p id='p'>{aa}</p></body></html>");
        let (doc, t) = build(&html, "body{margin:0} p{margin:0;font-size:10px;direction:rtl}");
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines[0].children.len(), 1);
        let frag = &lines[0].children[0].dimensions.content;
        let line_right = lines[0].dimensions.content.x + lines[0].dimensions.content.width;
        assert!((frag.x + frag.width - line_right).abs() < 1.0);
    }

    #[test]
    fn bidi_override_multiword_spacing_preserved() {
        // direction:rtl + bidi-override on two LTR words: whole line reverses to
        // visual "bb aa" but the inter-word space must remain (10px gap).
        let (doc, t) = build(
            "<html><body><p id='p'>aa bb</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px;direction:rtl;unicode-bidi:bidi-override}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 200.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        let frags = &lines[0].children;
        assert_eq!(frags.len(), 2);
        let mut by_x: Vec<&LayoutBox> = frags.iter().collect();
        by_x.sort_by(|a, b| {
            a.dimensions.content.x.partial_cmp(&b.dimensions.content.x).unwrap()
        });
        let l = &by_x[0].dimensions.content;
        let r = &by_x[1].dimensions.content;
        let gap = r.x - (l.x + l.width);
        assert_eq!(gap, 10.0, "bidi-override multiword spacing must be preserved, got {gap}");
    }

    #[test]
    fn ltr_no_special_props_unchanged() {
        // NO-REGRESSION: a plain LTR line lays out identically (frag text + x).
        let (doc, t) = build(
            "<html><body><p id='p'>aaa bbb ccc</p></body></html>",
            "body{margin:0} p{margin:0;font-size:10px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 800.0, &m, &NoImages);
        let lines = lines_of(&root, &doc, "p");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].children[0].dimensions.content.x, 0.0);
        assert_eq!(lines[0].children[1].dimensions.content.x, 40.0);
        assert_eq!(lines[0].children[2].dimensions.content.x, 80.0);
        assert_eq!(lines[0].children[0].text(), Some("aaa"));
    }

    // --- E7-M2: ::before / ::after generated boxes ---

    use boxtree::build_box_tree;
    use starfish_style::{PseudoElement, Rgba};

    /// First text in a box subtree, pre-order.
    fn first_text(b: &LayoutBox) -> Option<&str> {
        if let Some(t) = b.text() {
            return Some(t);
        }
        b.children.iter().find_map(first_text)
    }

    #[test]
    fn before_prepends_generated_box() {
        let (doc, t) = build("<body><div>hi</div></body>", "div::before { content: \"\u{2192} \" }");
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let div = box_for(&root, find(&doc, "div")).unwrap();
        let first = &div.children[0];
        assert_eq!(first.kind, BoxKind::InlineBox);
        assert!(matches!(first.style, BoxStyleRef::Generated { side: PseudoElement::Before, .. }));
        assert_eq!(first.children[0].kind, BoxKind::TextRun);
        assert_eq!(first.children[0].text(), Some("\u{2192} "));
        // the div's own text follows.
        assert!(div.children[1..].iter().any(|c| first_text(c) == Some("hi")));
    }

    #[test]
    fn after_appends_generated_box() {
        let (doc, t) = build("<body><a>link</a></body>", "a::after { content: \" \u{2197}\" }");
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let a = box_for(&root, find(&doc, "a")).unwrap();
        let last = a.children.last().unwrap();
        assert_eq!(last.kind, BoxKind::InlineBox);
        assert!(matches!(last.style, BoxStyleRef::Generated { side: PseudoElement::After, .. }));
        assert_eq!(last.children[0].text(), Some(" \u{2197}"));
    }

    #[test]
    fn content_none_no_generated_box() {
        // No ::before rule at all → no extra child (no-regression).
        let (doc, t) = build("<body><div>hi</div></body>", "div { color: red }");
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let div = box_for(&root, find(&doc, "div")).unwrap();
        assert!(div
            .children
            .iter()
            .all(|c| !matches!(c.style, BoxStyleRef::Generated { .. })));
    }

    #[test]
    fn empty_content_box_without_textrun() {
        let (doc, t) = build("<body><div>hi</div></body>", "div::before { content: \"\" }");
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let div = box_for(&root, find(&doc, "div")).unwrap();
        let first = &div.children[0];
        assert!(matches!(first.style, BoxStyleRef::Generated { .. }));
        assert_eq!(first.kind, BoxKind::InlineBox);
        assert!(first.children.is_empty(), "empty content → no TextRun child");
    }

    #[test]
    fn generated_box_resolves_pseudo_style() {
        let (doc, t) = build(
            "<body><div>hi</div></body>",
            "div::before { content: \"x\"; color: #ff0000 }",
        );
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let div = box_for(&root, find(&doc, "div")).unwrap();
        let gen = &div.children[0];
        let s = gen.style(&t).expect("generated box style resolves");
        assert_eq!(s.color, Rgba { r: 255, g: 0, b: 0, a: 255 });
    }

    #[test]
    fn attr_content_in_box() {
        let (doc, t) = build(
            "<body><div data-x='NEW'>hi</div></body>",
            "[data-x]::before { content: attr(data-x) }",
        );
        let root = build_box_tree(&doc, &t, find(&doc, "body"));
        let div = box_for(&root, find(&doc, "div")).unwrap();
        assert_eq!(div.children[0].children[0].text(), Some("NEW"));
    }

    #[test]
    fn img_has_no_pseudo_box() {
        // <img> is a replaced element → no generated content child.
        let (doc, t) = build(
            "<div><img src='a.png'></div>",
            "img::before { content: \"x\" }",
        );
        let root = build_box_tree(&doc, &t, find(&doc, "div"));
        let img = box_for(&root, find(&doc, "img")).unwrap();
        assert_eq!(img.kind, BoxKind::Image);
        assert!(img.children.is_empty());
    }

    // --- E7-M3: table layout ---

    /// Lay out `html`+`css` against a fixed measurer (per=10) and a wide
    /// viewport; return the root box. `body`/`table`/cells get zero margins so
    /// table geometry is clean.
    fn table_layout(html: &str, css: &str) -> (Document, LayoutBox) {
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(html, css);
        let root = layout(&doc, &t, 1000.0, &m, &NoImages);
        (doc, root)
    }

    fn cell_box<'a>(root: &'a LayoutBox, doc: &Document, id: &str) -> &'a LayoutBox {
        box_for(root, find_id(doc, id)).unwrap()
    }

    #[test]
    fn table_2x2_no_spacing() {
        // Each cell content "abcde" = 50 wide, height 20 (one line).
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td id='b'>abcde</td></tr>\
            <tr><td id='c'>abcde</td><td id='d'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        let c = cell_box(&root, &doc, "c").dimensions.border_box();
        let d = cell_box(&root, &doc, "d").dimensions.border_box();
        assert_eq!((a.x, a.y, a.width), (0.0, 0.0, 50.0));
        assert_eq!((b.x, b.y, b.width), (50.0, 0.0, 50.0));
        assert_eq!((c.x, c.y), (0.0, 20.0));
        assert_eq!((d.x, d.y), (50.0, 20.0));
        // Table content height = 40.
        let table = box_for(&root, find(&doc, "table")).unwrap();
        assert_eq!(table.dimensions.content.height, 40.0);
    }

    #[test]
    fn table_column_sizes_to_widest_cell() {
        // col 0: "ab"(20) and "abcdef"(60) → col 0 = 60.
        let html = "<html><body><table>\
            <tr><td id='a'>ab</td></tr>\
            <tr><td id='b'>abcdef</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0}";
        let (doc, root) = table_layout(html, css);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        assert_eq!(a.width, 60.0); // narrow cell stretched to column width
        assert_eq!(b.width, 60.0);
    }

    #[test]
    fn table_border_spacing_gaps() {
        // 2×2, content 50 wide / 20 tall, border-spacing 10.
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td id='b'>abcde</td></tr>\
            <tr><td id='c'>abcde</td><td id='d'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:10px} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        let c = cell_box(&root, &doc, "c").dimensions.border_box();
        assert_eq!((a.x, a.y), (10.0, 10.0));
        assert_eq!(b.x, 10.0 + 50.0 + 10.0); // 70
        assert_eq!(c.y, 10.0 + 20.0 + 10.0); // 40
        let table = box_for(&root, find(&doc, "table")).unwrap();
        // height = 20+20 + 10*3 = 70; width = 50+50 + 10*3 = 130.
        assert_eq!(table.dimensions.content.height, 70.0);
        assert_eq!(table.dimensions.content.width, 130.0);
    }

    #[test]
    fn table_border_spacing_two_lengths() {
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td id='b'>abcde</td></tr>\
            <tr><td id='c'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:4px 12px} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        let c = cell_box(&root, &doc, "c").dimensions.border_box();
        assert_eq!(b.x, 4.0 + 50.0 + 4.0); // horizontal uses 4
        assert_eq!(c.y, 12.0 + 20.0 + 12.0); // vertical uses 12
    }

    #[test]
    fn table_colspan_two() {
        // Row 0: one colspan=2 cell. Row 1: two cells each 40 wide.
        let html = "<html><body><table>\
            <tr><td id='span' colspan='2'>x</td></tr>\
            <tr><td id='l'>abcd</td><td id='r'>abcd</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:6px} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let s = cell_box(&root, &doc, "span").dimensions.border_box();
        let l = cell_box(&root, &doc, "l").dimensions.border_box();
        let r = cell_box(&root, &doc, "r").dimensions.border_box();
        // cols 40,40; spanning cell width = 40+40+6 = 86, at x=6.
        assert_eq!(s.x, 6.0);
        assert_eq!(s.width, 86.0);
        assert_eq!(l.x, 6.0);
        assert_eq!(r.x, 6.0 + 40.0 + 6.0); // 52
    }

    #[test]
    fn table_rowspan_two() {
        // Col 0 cell rowspan=2 (height 60). Col 1 two cells each height 25.
        let html = "<html><body><table>\
            <tr><td id='span' rowspan='2'>x</td><td id='t'>top</td></tr>\
            <tr><td id='b'>bot</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:25px} \
                   #span{line-height:60px}";
        let (doc, root) = table_layout(html, css);
        let s = cell_box(&root, &doc, "span").dimensions.border_box();
        let t = cell_box(&root, &doc, "t").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        // The rowspan cell spans both rows → its height covers both row heights.
        assert!(s.height >= 60.0, "rowspan height {}", s.height);
        // Row 1 cell sits below row 0 cell; col 0 slot in row 1 is occupied.
        assert!(b.y >= t.y + t.height, "b.y {} t {}", b.y, t.y + t.height);
        // col-1 cells are in column 1 (right of the rowspan cell).
        assert!(t.x > s.x);
        assert_eq!(b.x, t.x);
    }

    #[test]
    fn table_rowspan_overhang_single_row_no_panic() {
        // A rowspan=2 cell in a 1-row table overhangs the bottom. The span must
        // clamp to the single existing row — no index-out-of-bounds panic.
        let html = "<html><body><table>\
            <tr><td id='x' rowspan='2'>x</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let x = cell_box(&root, &doc, "x").dimensions.border_box();
        // Renders into the single row (height 20), not 0, no panic.
        assert_eq!(x.y, 0.0);
        assert!(x.height >= 20.0, "height {}", x.height);
    }

    #[test]
    fn table_rowspan_overhang_last_row_no_panic() {
        // A rowspan=3 cell starting in the last row of a 2-row table overhangs.
        // The span clamps to the last existing row — no panic.
        let html = "<html><body><table>\
            <tr><td id='a'>a</td></tr>\
            <tr><td id='b' rowspan='3'>b</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        // b is in row 1, below a; clamped to the single last row.
        assert!(b.y >= a.y + a.height, "b.y {} a {}", b.y, a.y + a.height);
        assert!(b.height >= 20.0, "height {}", b.height);
    }

    #[test]
    fn table_rowspan_two_in_range_spans_both_rows() {
        // Regression: an in-range rowspan=2 (2-row table, cell in row 0 spanning
        // both rows) still covers both row heights + interior spacing.
        let html = "<html><body><table>\
            <tr><td id='span' rowspan='2'>x</td><td id='t'>top</td></tr>\
            <tr><td id='b'>bot</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:8px} \
                   td{padding:0;border:0;line-height:25px}";
        let (doc, root) = table_layout(html, css);
        let s = cell_box(&root, &doc, "span").dimensions.border_box();
        // Two rows of 25 each + one interior v-spacing of 8 = 58.
        assert_eq!(s.height, 25.0 + 8.0 + 25.0);
        assert_eq!(s.y, 8.0); // outer top spacing
    }

    #[test]
    fn table_deeply_nested_no_stack_overflow() {
        // 40 levels of table-in-cell-in-table-... must not overflow the stack;
        // beyond MAX_TABLE_NESTING it degrades to plain block layout.
        let mut html = String::from("<html><body>");
        for _ in 0..40 {
            html.push_str("<table><tr><td>");
        }
        html.push('x');
        for _ in 0..40 {
            html.push_str("</td></tr></table>");
        }
        html.push_str("</body></html>");
        let css = "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0}";
        // Must complete without panicking / overflowing.
        let (_doc, root) = table_layout(&html, css);
        assert!(root.dimensions.content.height >= 0.0);
    }

    #[test]
    fn table_ragged_rows_no_panic() {
        // Row 0 has 3 cells, row 1 has 2 → column count 3, no panic.
        let html = "<html><body><table>\
            <tr><td>a</td><td>b</td><td id='c'>c</td></tr>\
            <tr><td>d</td><td>e</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0}";
        let (doc, root) = table_layout(html, css);
        // Column 2 exists from row 0's third cell.
        let c = cell_box(&root, &doc, "c").dimensions.border_box();
        assert!(c.x > 0.0);
    }

    #[test]
    fn table_set_width_distributes() {
        // width:300; two columns preferred 50 each → each grows to 150.
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td id='b'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;width:300px;border-spacing:0} \
                   td{padding:0;border:0}";
        let (doc, root) = table_layout(html, css);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        assert_eq!(a.width, 150.0);
        assert_eq!(b.width, 150.0);
        assert_eq!(a.x, 0.0);
        assert_eq!(b.x, 150.0);
    }

    #[test]
    fn table_auto_shrink_to_fit() {
        // Two columns pref 50 and 80, border-spacing 0 → table width = 130.
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td id='b'>abcdefgh</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0}";
        let (doc, root) = table_layout(html, css);
        let table = box_for(&root, find(&doc, "table")).unwrap();
        assert_eq!(table.dimensions.content.width, 130.0);
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let b = cell_box(&root, &doc, "b").dimensions.border_box();
        assert_eq!(a.width, 50.0);
        assert_eq!(b.width, 80.0);
    }

    #[test]
    fn table_height_with_vertical_spacing() {
        // Single row, cell height 30, border-spacing 0 5 → height = 30 + 5*2 = 40.
        let html = "<html><body><table>\
            <tr><td id='a'>x</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0 5px} \
                   td{padding:0;border:0;line-height:30px}";
        let (doc, root) = table_layout(html, css);
        let table = box_for(&root, find(&doc, "table")).unwrap();
        assert_eq!(table.dimensions.content.height, 40.0);
    }

    #[test]
    fn table_thead_tbody_row_groups() {
        // thead/tbody wrap rows; cells still collected and placed.
        let html = "<html><body><table>\
            <thead><tr><th id='h'>abcde</th></tr></thead>\
            <tbody><tr><td id='d'>abcde</td></tr></tbody>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   th,td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let h = cell_box(&root, &doc, "h").dimensions.border_box();
        let d = cell_box(&root, &doc, "d").dimensions.border_box();
        assert_eq!((h.x, h.y), (0.0, 0.0));
        assert_eq!(d.x, 0.0);
        assert_eq!(d.y, 20.0); // second row, below header
    }

    #[test]
    fn table_anonymous_cell_fixup() {
        // <table> with <td> directly (no <tr>) → one synthetic row, no panic.
        let html = "<html><body>\
            <div id='t' style='display:table'>\
              <div id='c' style='display:table-cell'>abcde</div>\
            </div></body></html>";
        let css = "body{margin:0} #t{margin:0;border-spacing:0} #c{padding:0;border:0}";
        let (doc, root) = table_layout(html, css);
        let c = cell_box(&root, &doc, "c").dimensions.border_box();
        assert_eq!((c.x, c.y), (0.0, 0.0));
        assert_eq!(c.width, 50.0);
    }

    #[test]
    fn table_row_box_sized_for_background() {
        // A <tr> box gets a non-zero content rect (so its background paints).
        let html = "<html><body><table>\
            <tr id='row'><td>abcde</td><td>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let row = box_for(&root, find_id(&doc, "row")).unwrap();
        assert_eq!(row.dimensions.content.height, 20.0);
        // two columns of 50 = 100 (interior spacing 0).
        assert_eq!(row.dimensions.content.width, 100.0);
    }

    #[test]
    fn table_cell_border_padding_in_geometry() {
        // A cell with border+padding: its border box fills the slot, content box
        // is inset by border+padding.
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:6px;border:2px solid black;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let cell = cell_box(&root, &doc, "a");
        assert_eq!(cell.dimensions.border.left, 2.0);
        assert_eq!(cell.dimensions.padding.left, 6.0);
        // content width 50 + padding 12 + border 4 = 66 border box.
        let bb = cell.dimensions.border_box();
        assert_eq!(bb.width, 66.0);
        assert_eq!((bb.x, bb.y), (0.0, 0.0));
    }

    #[test]
    fn nested_table_in_cell() {
        // Outer 1×1 table whose cell contains an inner display:table.
        let html = "<html><body><table>\
            <tr><td id='outer'>\
              <table><tr><td id='in1'>abcde</td><td id='in2'>abcde</td></tr></table>\
            </td></tr></table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let in1 = cell_box(&root, &doc, "in1").dimensions.border_box();
        let in2 = cell_box(&root, &doc, "in2").dimensions.border_box();
        // Inner cells laid out via recursive dispatch.
        assert_eq!(in1.width, 50.0);
        assert_eq!(in2.x, 50.0);
        // Outer cell is at least as tall as the inner table (one row of 20).
        let outer = cell_box(&root, &doc, "outer");
        assert!(outer.dimensions.content.height >= 20.0);
    }

    // --- E7-M3 regression: inline layout idempotence (inter-word space) ---
    //
    // A table cell / flex|grid item is laid out by `layout_block` MULTIPLE times
    // (its container measures column width, row height, then finally positions
    // it). The first pass replaces the box's `TextRun` child with `LineBox`
    // word-fragments whose inter-word space lives only as the positional x-gap
    // between fragments. A naive second pass would collapse that gap, rendering
    // "Red apples" as "Redapples". These guard that re-layout is idempotent.

    /// Ordered (text, content.x, content.width) of every `TextRun` under `b`.
    fn text_frags(b: &LayoutBox) -> Vec<(String, f32, f32)> {
        let mut runs: Vec<&LayoutBox> = Vec::new();
        collect_kind(b, BoxKind::TextRun, &mut runs);
        runs.iter()
            .map(|r| {
                (
                    r.text().unwrap_or("").to_string(),
                    r.dimensions.content.x,
                    r.dimensions.content.width,
                )
            })
            .collect()
    }

    /// Assert two consecutive words keep a one-space gap (per=10 → space = 10).
    fn assert_one_space_gap(frags: &[(String, f32, f32)], first: &str, second: &str) {
        let i = frags.iter().position(|(t, ..)| t == first).unwrap_or_else(|| {
            panic!("word {first:?} not found in {frags:?}");
        });
        let j = frags.iter().position(|(t, ..)| t == second).unwrap_or_else(|| {
            panic!("word {second:?} not found in {frags:?}");
        });
        let (_, fx, fw) = frags[i];
        let (_, sx, _) = frags[j];
        let gap = sx - (fx + fw);
        assert!(
            (gap - 10.0).abs() < 0.01,
            "expected one-space (10px) gap between {first:?} and {second:?}, got {gap} ({frags:?})",
        );
    }

    #[test]
    fn table_cell_keeps_inter_word_space() {
        // `<td>Red apples</td>` must keep the space → "Red" and "apples" stay a
        // full space apart, not abutting (the E7-M3 non-idempotence bug).
        let html = "<html><body><table>\
            <tr><td id='c'>Red apples</td></tr></table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        let cell = cell_box(&root, &doc, "c");
        let frags = text_frags(cell);
        assert_eq!(
            frags.iter().map(|(t, ..)| t.as_str()).collect::<Vec<_>>(),
            vec!["Red", "apples"],
        );
        assert_one_space_gap(&frags, "Red", "apples");
        // Cell content spans both words + the space: 30 + 10 + 60 = 100 wide.
        assert_eq!(cell.dimensions.content.width, 100.0);
    }

    #[test]
    fn table_multi_column_cells_keep_spaces() {
        // Two multi-word cells in one row; both keep their inter-word space.
        let html = "<html><body><table>\
            <tr><td id='a'>Red apples</td><td id='b'>Ripe bananas</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        assert_one_space_gap(&text_frags(cell_box(&root, &doc, "a")), "Red", "apples");
        assert_one_space_gap(&text_frags(cell_box(&root, &doc, "b")), "Ripe", "bananas");
    }

    #[test]
    fn table_colspan_cell_keeps_spaces() {
        // A multi-word cell that spans two columns still keeps its space.
        let html = "<html><body><table>\
            <tr><td id='span' colspan='2'>Red apples</td></tr>\
            <tr><td>abcd</td><td>abcd</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:6px} \
                   td{padding:0;border:0;line-height:20px}";
        let (doc, root) = table_layout(html, css);
        assert_one_space_gap(&text_frags(cell_box(&root, &doc, "span")), "Red", "apples");
    }

    #[test]
    fn div_multi_word_space_unaffected() {
        // Regression: a plain block (single layout pass) is unchanged.
        let (doc, t) = build(
            "<html><body><div id='c'>Red apples</div></body></html>",
            "body{margin:0} div{margin:0;line-height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 1000.0, &m, &NoImages);
        assert_one_space_gap(&text_frags(box_for(&root, find_id(&doc, "c")).unwrap()), "Red", "apples");
    }

    #[test]
    fn flex_item_keeps_inter_word_space() {
        // Flex items are also re-laid-out by the flex algorithm (measure + place)
        // → guard the same idempotence bug.
        let (doc, t) = build(
            "<html><body><div id='f'><div id='c'>Red apples</div></div></body></html>",
            "body{margin:0} #f{display:flex} div{margin:0;line-height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 1000.0, &m, &NoImages);
        assert_one_space_gap(&text_frags(box_for(&root, find_id(&doc, "c")).unwrap()), "Red", "apples");
    }

    #[test]
    fn grid_item_keeps_inter_word_space() {
        // Grid items are re-laid-out (place + measure_item_width/height) → guard.
        let (doc, t) = build(
            "<html><body><div id='g'><div id='c'>Red apples</div></div></body></html>",
            "body{margin:0} #g{display:grid;grid-template-columns:200px} \
             div{margin:0;line-height:20px}",
        );
        let m = FixedMeasurer { per: 10.0 };
        let root = layout(&doc, &t, 1000.0, &m, &NoImages);
        assert_one_space_gap(&text_frags(box_for(&root, find_id(&doc, "c")).unwrap()), "Red", "apples");
    }

    // --- E9-M1: <svg> replaced box sizing ---

    #[test]
    fn svg_box_sized_from_attrs_inline_replaced() {
        let (doc, t) = build(
            "<html><body><svg width='100' height='100'><rect/></svg></body></html>",
            "body{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let svg = box_for(&root, find(&doc, "svg")).unwrap();
        assert_eq!(svg.kind, BoxKind::Svg);
        assert!(svg.is_inline_level());
        assert!(svg.children.is_empty(), "no child boxes for svg subtree");
        assert_eq!(svg.dimensions.content.width, 100.0);
        assert_eq!(svg.dimensions.content.height, 100.0);
    }

    #[test]
    fn svg_box_default_300x150() {
        let (doc, t) = build(
            "<html><body><svg><rect/></svg></body></html>",
            "body{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let svg = box_for(&root, find(&doc, "svg")).unwrap();
        assert_eq!(svg.dimensions.content.width, 300.0);
        assert_eq!(svg.dimensions.content.height, 150.0);
    }

    #[test]
    fn svg_box_from_viewbox_only() {
        let (doc, t) = build(
            "<html><body><svg viewBox='0 0 40 20'><rect/></svg></body></html>",
            "body{margin:0}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let svg = box_for(&root, find(&doc, "svg")).unwrap();
        assert_eq!(svg.dimensions.content.width, 40.0);
        assert_eq!(svg.dimensions.content.height, 20.0);
    }

    // --- E12-M1: intrinsic-measurement memoization ---

    /// A uniform 3×3 table measures each of its 9 cells multiple times across the
    /// column / row / final passes. With the cache, each distinct
    /// `(cell node, MeasureKind, width)` is computed exactly once, so the count of
    /// *uncached* measure passes is far below the naive (per-call) total. The
    /// geometry must be byte-identical to the un-cached layout (a couple of cell
    /// boxes are checked here; the wider table suite is the full guard).
    #[test]
    fn measure_cache_dedupes_multipass() {
        use crate::cache::MEASURE_UNCACHED_CALLS;
        let html = "<html><body><table>\
            <tr><td id='a'>abcde</td><td>abcde</td><td>abcde</td></tr>\
            <tr><td>abcde</td><td id='e'>abcde</td><td>abcde</td></tr>\
            <tr><td>abcde</td><td>abcde</td><td id='i'>abcde</td></tr>\
            </table></body></html>";
        let css = "body{margin:0} table{margin:0;border-spacing:0} \
                   td{padding:0;border:0;line-height:20px}";
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(html, css);

        MEASURE_UNCACHED_CALLS.with(|c| c.set(0));
        let root = layout(&doc, &t, 1000.0, &m, &NoImages);
        let uncached = MEASURE_UNCACHED_CALLS.with(|c| c.get());

        // 9 cells, each measured for width (TableCellWidth @ avail) and outer
        // height (TableCellHeight @ cell_w) → 2 distinct memo keys per cell, so
        // exactly 18 uncached passes. Observed-and-locked: any change here means
        // the cache key or call pattern shifted.
        assert_eq!(uncached, 18, "expected one width + one height measure per cell");
        // The naive count (without the cache) re-measures every column pass, row
        // pass and the final placement pass, so it is strictly higher.
        assert!(uncached < 9 * 3, "cache must skip repeated measures");

        // Geometry unchanged: each cell "abcde" = 50 wide, 20 tall.
        let a = cell_box(&root, &doc, "a").dimensions.border_box();
        let e = cell_box(&root, &doc, "e").dimensions.border_box();
        let i = cell_box(&root, &doc, "i").dimensions.border_box();
        assert_eq!((a.x, a.y, a.width, a.height), (0.0, 0.0, 50.0, 20.0));
        assert_eq!((e.x, e.y, e.width), (50.0, 20.0, 50.0));
        assert_eq!((i.x, i.y, i.width), (100.0, 40.0, 50.0));
    }

    /// Grid variant: a 2×2 auto-track grid measures each item's width (auto
    /// column sizing) and height (auto row sizing); the cache dedupes the repeats.
    #[test]
    fn measure_cache_dedupes_grid() {
        use crate::cache::MEASURE_UNCACHED_CALLS;
        let html = "<html><body><div id='g'>\
            <div id='a'>ab</div><div id='b'>ab</div>\
            <div id='c'>ab</div><div id='d'>ab</div>\
            </div></body></html>";
        let css = "body{margin:0} #g{margin:0;display:grid;\
                   grid-template-columns:auto auto;grid-template-rows:auto auto;gap:0} \
                   #g>div{margin:0;line-height:20px}";
        let m = FixedMeasurer { per: 10.0 };
        let (doc, t) = build(html, css);

        MEASURE_UNCACHED_CALLS.with(|c| c.set(0));
        let _root = layout(&doc, &t, 1000.0, &m, &NoImages);
        let uncached = MEASURE_UNCACHED_CALLS.with(|c| c.get());

        // Observed-and-locked count: less than the naive per-call total.
        assert_eq!(uncached, 8);
        assert!(uncached < 4 * 3);
    }

    // --- E13-M1: box-sizing + min/max sizing ---

    #[test]
    fn border_box_content_shrinks_by_padding_border() {
        // width:200 padding:10 border:5 border-box → content = 200 - (20+10) = 170;
        // border-box width back to 200.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;box-sizing:border-box;\
             width:200px;padding:10px;border:5px solid black}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 170.0);
        assert_eq!(a.dimensions.border_box().width, 200.0);
    }

    #[test]
    fn content_box_explicit_unchanged() {
        // content-box (default) keeps content = 200 with the same padding/border.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;width:200px;padding:10px;border:5px solid black}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 200.0);
    }

    #[test]
    fn min_width_raises_small_width() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;width:50px;min-width:120px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 120.0);
    }

    #[test]
    fn max_width_caps_large_width() {
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;width:600px;max-width:200px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 200.0);
    }

    #[test]
    fn max_width_caps_percentage_width() {
        // width:80% of 800 = 640, capped by max-width:300px.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;width:80%;max-width:300px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 300.0);
    }

    #[test]
    fn min_max_height_clamp() {
        // height:20 raised to min-height:60.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;height:20px;min-height:60px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.height, 60.0);
        // height:300 capped to max-height:100.
        let (doc2, t2) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;height:300px;max-height:100px}",
        );
        let root2 = layout(&doc2, &t2, 800.0, &DefaultMeasurer, &NoImages);
        let a2 = box_for(&root2, find_id(&doc2, "a")).unwrap();
        assert_eq!(a2.dimensions.content.height, 100.0);
    }

    #[test]
    fn border_box_with_percentage() {
        // width:50% of 800 = 400 border-box; padding 10 border 5 → content 370.
        let (doc, t) = build(
            "<html><body><div id='a'>x</div></body></html>",
            "body{margin:0} #a{margin:0;box-sizing:border-box;\
             width:50%;padding:10px;border:5px solid black}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 370.0);
        assert_eq!(a.dimensions.border_box().width, 400.0);
    }

    #[test]
    fn border_box_flex_item_basis() {
        // flex-basis:100 border-box, padding:10 → base content 80; the item's
        // margin-box main extent stays 100. No grow/shrink so it keeps that size.
        let (doc, t) = build(
            "<html><body><div id='f'><div id='a'>a</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:400px;height:100px} \
             #a{margin:0;box-sizing:border-box;flex:0 0 100px;padding:10px}",
        );
        let root = layout(&doc, &t, 400.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 80.0);
        assert_eq!(a.dimensions.margin_box().width, 100.0);
    }

    #[test]
    fn min_width_on_flex_item() {
        // Two items width:80 in a 100px row shrink toward 50 each, but min-width:70
        // floors the first item at 70.
        let (doc, t) = build(
            "<html><body><div id='f'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} #f{margin:0;display:flex;width:100px;height:50px} \
             #a{margin:0;width:80px;min-width:70px} #b{margin:0;width:80px}",
        );
        let root = layout(&doc, &t, 300.0, &DefaultMeasurer, &NoImages);
        let a = box_for(&root, find_id(&doc, "a")).unwrap();
        assert_eq!(a.dimensions.content.width, 70.0);
    }

    #[test]
    fn img_max_width_clamp() {
        // intrinsic 40×20; css width:100 capped by max-width:30 → 30 (aspect not
        // preserved, height stays the scaled-from-100 value... here width is the
        // only css size so height follows intrinsic-scaling on the pre-clamp width).
        let (doc, t) = build(
            "<html><body><p><img id='im' src='img'></p></body></html>",
            "body{margin:0} p{margin:0} #im{width:100px;max-width:30px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 30.0);
    }

    #[test]
    fn img_border_box_shrinks() {
        // border-box width:100 with padding... images carry no padding/border in
        // this engine unless set; set padding:10 → content 80.
        let (doc, t) = build(
            "<html><body><p><img id='im' src='img'></p></body></html>",
            "body{margin:0} p{margin:0} #im{box-sizing:border-box;width:100px;padding:10px}",
        );
        let root = layout(&doc, &t, 800.0, &DefaultMeasurer, &StubImages);
        let img = image_box(&root);
        assert_eq!(img.dimensions.content.width, 80.0);
    }
}
