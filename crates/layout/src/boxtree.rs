//! Box tree model + generation (§1.2, §2): `BoxKind`, `BoxStyleRef`,
//! `LayoutBox`, whitespace collapsing, and the anonymous-block rule.

use starfish_dom::{Document, NodeKind};
use starfish_style::{ComputedStyle, Display, ListStyleType, NodeId, StyledTree};

use crate::dimensions::Dimensions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    /// Block-level box doing block layout (block children) or inline layout
    /// (inline children).
    BlockContainer,
    /// Inline-level box from a `display:inline` element.
    InlineBox,
    /// Atomic inline from a `display:inline-block` element; runs block layout
    /// internally, occupies its margin-box width as one unit on the line (§2.1).
    InlineBlock,
    /// Synthesized block wrapping a run of inline-level siblings (§2.3).
    AnonymousBlock,
    /// A run of text; carries the collapsed string + the parent element style.
    TextRun,
    /// One line produced by inline layout; children are its fragments.
    LineBox,
    /// List-item marker (bullet / ordinal); carries `text` like a `TextRun` but
    /// is never text-decorated (§3.2).
    Marker,
}

/// Back-reference to style. Elements point at their styled `NodeId`; anonymous
/// and line boxes inherit the containing block's style for font/color defaults.
#[derive(Debug, Clone)]
pub enum BoxStyleRef {
    /// A styled element: look up `styled.computed(node)`.
    Node(NodeId),
    /// Anonymous box: no DOM node. Inherit font/color from this element's style.
    Anonymous(NodeId),
}

impl BoxStyleRef {
    /// The `NodeId` backing this ref (real element for `Node`, the inheriting
    /// element for `Anonymous`).
    pub fn node(&self) -> NodeId {
        match self {
            BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => *id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayoutBox {
    pub kind: BoxKind,
    pub style: BoxStyleRef,
    /// Text payload — `Some` iff `kind == TextRun`.
    pub text: Option<String>,
    pub dimensions: Dimensions,
    pub children: Vec<LayoutBox>,
}

impl LayoutBox {
    pub fn new(kind: BoxKind, style: BoxStyleRef) -> LayoutBox {
        LayoutBox {
            kind,
            style,
            text: None,
            dimensions: Dimensions::default(),
            children: Vec::new(),
        }
    }

    /// True if this box is inline-level (participates in inline flow).
    pub(crate) fn is_inline_level(&self) -> bool {
        matches!(
            self.kind,
            BoxKind::InlineBox | BoxKind::InlineBlock | BoxKind::TextRun | BoxKind::Marker
        )
    }
}

/// Collapse runs of ASCII whitespace to a single U+0020 space. Leading/trailing
/// whitespace collapses to a single leading/trailing space (kept here; inline
/// layout drops them at line edges).
pub(crate) fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            in_ws = true;
        } else {
            if in_ws {
                out.push(' ');
            }
            in_ws = false;
            out.push(c);
        }
    }
    if in_ws && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// Generate the box for one DOM node (and its subtree). `None` if the node
/// produces no box (display:none element, non-text non-element, or dropped
/// whitespace-only text).
fn build_node(doc: &Document, styled: &StyledTree, id: NodeId, parent_elem: NodeId) -> Option<LayoutBox> {
    match doc.kind(id) {
        NodeKind::Text(raw) => {
            let text = collapse_ws(raw);
            if text.is_empty() {
                return None;
            }
            let mut b = LayoutBox::new(BoxKind::TextRun, BoxStyleRef::Node(parent_elem));
            b.text = Some(text);
            Some(b)
        }
        NodeKind::Element(_) => {
            let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
            let kind = match display {
                Display::None => return None,
                Display::Block => BoxKind::BlockContainer,
                Display::Inline => BoxKind::InlineBox,
                Display::InlineBlock => BoxKind::InlineBlock,
            };
            let mut b = LayoutBox::new(kind, BoxStyleRef::Node(id));
            b.children = build_children(doc, styled, id);
            // List-item marker: prepend a synthetic Marker as the first child of
            // an <li> whose parent is <ul>/<ol> (§3.2). Only when the item runs
            // the inline-layout path — i.e. it has no block-level children;
            // otherwise the marker would be wrapped into an anonymous block and
            // eat a line, pushing content down (§3.2/§6 — no marker on block li).
            if is_list_item(doc, id) && !b.children.iter().any(|c| !c.is_inline_level()) {
                if let Some(marker) = make_marker(doc, styled, id) {
                    b.children.insert(0, marker);
                }
            }
            Some(b)
        }
        _ => None,
    }
}

/// A node is a list item iff it's `<li>` whose parent is `<ul>`/`<ol>` (§3.2).
fn is_list_item(doc: &Document, id: NodeId) -> bool {
    doc.tag_name(id) == Some("li")
        && matches!(
            doc.parent(id).and_then(|p| doc.tag_name(p)),
            Some("ul") | Some("ol")
        )
}

/// Build the marker box (text payload), or `None` if `list-style-type: none`.
fn make_marker(doc: &Document, styled: &StyledTree, li: NodeId) -> Option<LayoutBox> {
    let st = styled.get(li)?;
    let label = match st.list_style_type {
        ListStyleType::None => return None,
        ListStyleType::Disc => "\u{2022}".to_string(),  // •
        ListStyleType::Circle => "\u{25E6}".to_string(), // ◦
        ListStyleType::Square => "\u{25AA}".to_string(), // ▪
        ListStyleType::Decimal => format!("{}.", ordinal_of(doc, li)),
    };
    let mut m = LayoutBox::new(BoxKind::Marker, BoxStyleRef::Node(li));
    m.text = Some(label);
    Some(m)
}

/// 1-based position of `li` among its `<li>` element siblings (for `<ol>`).
/// No `start`/`value`/`counter-reset` support (M1).
fn ordinal_of(doc: &Document, li: NodeId) -> usize {
    let Some(parent) = doc.parent(li) else { return 1 };
    let mut n = 0;
    for c in doc.children(parent) {
        if doc.tag_name(c) == Some("li") {
            n += 1;
            if c == li {
                break;
            }
        }
    }
    n
}

/// Generate child boxes for an element, applying whitespace dropping between
/// block siblings and the anonymous-block wrapping rule.
fn build_children(doc: &Document, styled: &StyledTree, elem: NodeId) -> Vec<LayoutBox> {
    let mut raw: Vec<LayoutBox> = Vec::new();
    for child in doc.children(elem) {
        // Drop a whitespace-only text node that is not adjacent to inline
        // content (i.e. its collapsed form is a lone space and the previous
        // generated box is block-level or absent — it sits between blocks).
        if let NodeKind::Text(t) = doc.kind(child) {
            let collapsed = collapse_ws(t);
            if collapsed.is_empty() {
                continue;
            }
            if collapsed == " " {
                let prev_inline = raw.last().map(|b| b.is_inline_level()).unwrap_or(false);
                if !prev_inline {
                    // whitespace-only text between/around block siblings → drop
                    continue;
                }
            }
        }
        if let Some(b) = build_node(doc, styled, child, elem) {
            raw.push(b);
        }
    }

    wrap_anonymous_blocks(raw, elem)
}

/// Apply §2.3: if children mix block- and inline-level boxes, wrap each maximal
/// run of inline-level children in an `AnonymousBlock`. If all inline or all
/// block, return unchanged.
fn wrap_anonymous_blocks(children: Vec<LayoutBox>, elem: NodeId) -> Vec<LayoutBox> {
    let has_block = children.iter().any(|c| !c.is_inline_level());
    let has_inline = children.iter().any(|c| c.is_inline_level());
    if !(has_block && has_inline) {
        return children;
    }

    let mut out: Vec<LayoutBox> = Vec::new();
    let mut run: Vec<LayoutBox> = Vec::new();
    for c in children {
        if c.is_inline_level() {
            run.push(c);
        } else {
            flush_run(&mut run, &mut out, elem);
            out.push(c);
        }
    }
    flush_run(&mut run, &mut out, elem);
    out
}

fn flush_run(run: &mut Vec<LayoutBox>, out: &mut Vec<LayoutBox>, elem: NodeId) {
    if run.is_empty() {
        return;
    }
    let mut anon = LayoutBox::new(BoxKind::AnonymousBlock, BoxStyleRef::Anonymous(elem));
    anon.children = std::mem::take(run);
    out.push(anon);
}

/// Build the root box tree from `root_element`. The root element is forced to a
/// `BlockContainer` (the initial containing block is block).
pub(crate) fn build_box_tree(doc: &Document, styled: &StyledTree, root_element: NodeId) -> LayoutBox {
    let mut root = LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Node(root_element));
    root.children = build_children(doc, styled, root_element);
    root
}

/// Resolve a box's `ComputedStyle` for layout, falling back to `initial()` for
/// boxes whose node isn't styled (anonymous wrappers, or robustness).
pub(crate) fn style_of(styled: &StyledTree, b: &LayoutBox) -> ComputedStyle {
    match &b.style {
        BoxStyleRef::Node(id) => styled
            .get(*id)
            .cloned()
            .unwrap_or_else(ComputedStyle::initial),
        // Anonymous boxes have no box-model properties of their own.
        BoxStyleRef::Anonymous(_) => ComputedStyle::initial(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_internal_and_edges() {
        assert_eq!(collapse_ws("a   b\n\tc"), "a b c");
        assert_eq!(collapse_ws("  hi  "), " hi ");
        assert_eq!(collapse_ws("   "), "");
        assert_eq!(collapse_ws(""), "");
        assert_eq!(collapse_ws("word"), "word");
    }
}
