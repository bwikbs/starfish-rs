//! Box tree model + generation (§1.2, §2): `BoxKind`, `BoxStyleRef`,
//! `LayoutBox`, whitespace collapsing, and the anonymous-block rule.

use starfish_dom::{Document, NodeKind};
use starfish_style::{
    ComputedStyle, Display, Float, ListStyleType, NodeId, Position, PseudoElement, StyledTree,
    TextTransform, WhiteSpace,
};

use crate::dimensions::Dimensions;

/// A box is out of flow if it floats or is absolutely/fixed positioned.
pub(crate) fn is_out_of_flow(s: &ComputedStyle) -> bool {
    s.float != Float::None || matches!(s.position, Position::Absolute | Position::Fixed)
}

/// A box participates in normal flow stacking iff it is not out of flow.
pub(crate) fn is_normal_flow(s: &ComputedStyle) -> bool {
    !is_out_of_flow(s)
}

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
    /// A replaced element (`<img>`). Always inline-level; carries the raw `src`
    /// in `text` and its used size in `dimensions`; no children (E2-M4).
    Image,
}

/// Back-reference to style. Elements point at their styled `NodeId`; anonymous
/// and line boxes inherit the containing block's style for font/color defaults.
#[derive(Debug, Clone)]
pub enum BoxStyleRef {
    /// A styled element: look up `styled.computed(node)`.
    Node(NodeId),
    /// Anonymous box: no DOM node. Inherit font/color from this element's style.
    Anonymous(NodeId),
    /// A `::before`/`::after` generated box: resolve via `styled.pseudo_style`
    /// keyed by the originating element + side (E7-M2).
    Generated { origin: NodeId, side: PseudoElement },
}

impl BoxStyleRef {
    /// The `NodeId` backing this ref (real element for `Node`, the inheriting
    /// element for `Anonymous`, the originating element for `Generated`).
    pub fn node(&self) -> NodeId {
        match self {
            BoxStyleRef::Node(id)
            | BoxStyleRef::Anonymous(id)
            | BoxStyleRef::Generated { origin: id, .. } => *id,
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
            BoxKind::InlineBox
                | BoxKind::InlineBlock
                | BoxKind::TextRun
                | BoxKind::Marker
                | BoxKind::Image
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

/// Transform raw text-node content per `white-space` (E6-M3 §2). A preserved
/// `\n` is kept literal in the returned string; inline.rs splits on it for
/// forced breaks. Collapsing modes match the old `collapse_ws` behaviour.
fn process_text(raw: &str, ws: WhiteSpace) -> String {
    match ws {
        WhiteSpace::Normal | WhiteSpace::Nowrap => collapse_ws(raw),
        WhiteSpace::Pre | WhiteSpace::PreWrap => preserve_ws(raw),
        WhiteSpace::PreLine => collapse_ws_keep_newlines(raw),
    }
}

/// `pre`/`pre-wrap`: keep all whitespace verbatim, but normalize `\r\n`/`\r` →
/// `\n` and `\t` → a single space (no 8-column tab stops, §7).
fn preserve_ws(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    normalized.replace('\t', " ")
}

/// `pre-line`: collapse space runs but keep `\n` as a literal segment break.
/// Spaces are collapsed per `\n`-delimited segment; a space adjacent to a
/// segment break collapses away (edge spaces trimmed), matching `pre-line`'s
/// "collapse spaces, preserve newlines" behaviour (E6-M3 §2.2).
fn collapse_ws_keep_newlines(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split('\n')
        .map(|seg| collapse_ws(seg).trim().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply `text-transform` to a whole string (Unicode-correct; length may change).
fn transform_text(s: &str, tt: TextTransform) -> String {
    match tt {
        TextTransform::None => s.to_string(),
        TextTransform::Uppercase => s.to_uppercase(),
        TextTransform::Lowercase => s.to_lowercase(),
        TextTransform::Capitalize => capitalize(s),
    }
}

/// Uppercase the first cased char of each whitespace-delimited word; leave the
/// rest as authored (CSS `capitalize` does not lowercase the tail).
fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            at_word_start = true;
            out.push(ch);
        } else if at_word_start {
            out.extend(ch.to_uppercase());
            at_word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Generate the box for one DOM node (and its subtree). `None` if the node
/// produces no box (display:none element, non-text non-element, or dropped
/// whitespace-only text).
fn build_node(doc: &Document, styled: &StyledTree, id: NodeId, parent_elem: NodeId) -> Option<LayoutBox> {
    match doc.kind(id) {
        NodeKind::Text(raw) => {
            // Text inherits the parent element's white-space / text-transform.
            let ps = styled.get(parent_elem);
            let ws = ps.map(|s| s.white_space).unwrap_or(WhiteSpace::Normal);
            let tt = ps.map(|s| s.text_transform).unwrap_or(TextTransform::None);
            let text = transform_text(&process_text(raw, ws), tt);
            // Drop a run that processed to nothing (empty in any mode).
            if text.is_empty() {
                return None;
            }
            let mut b = LayoutBox::new(BoxKind::TextRun, BoxStyleRef::Node(parent_elem));
            b.text = Some(text);
            Some(b)
        }
        NodeKind::Element(_) => {
            // Replaced element: <img> with a src → a leaf Image box (no children).
            if doc.tag_name(id) == Some("img") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                let src = doc.get_attribute(id, "src")?; // <img> without src → no box
                let mut b = LayoutBox::new(BoxKind::Image, BoxStyleRef::Node(id));
                b.text = Some(src.to_string());
                return Some(b);
            }
            let style = styled.get(id);
            let display = style.map(|s| s.display).unwrap_or(Display::Inline);
            let out_of_flow = style.map(is_out_of_flow).unwrap_or(false);
            let kind = match display {
                Display::None => return None,
                // float/abs/fixed blockify to a block-level container (§2).
                _ if out_of_flow => BoxKind::BlockContainer,
                Display::Block => BoxKind::BlockContainer,
                // A block-level flex container is a BlockContainer laid out by
                // the flex algorithm (dispatched on display in block.rs, §2).
                Display::Flex => BoxKind::BlockContainer,
                // A block-level grid container is a BlockContainer laid out by
                // the grid algorithm (dispatched on display in block.rs).
                Display::Grid => BoxKind::BlockContainer,
                Display::Inline => BoxKind::InlineBox,
                Display::InlineBlock => BoxKind::InlineBlock,
                // An inline-flex container is an atomic inline (inline-block)
                // laid out internally by the flex algorithm (§2).
                Display::InlineFlex => BoxKind::InlineBlock,
                // An inline-grid container is likewise an atomic inline laid out
                // internally by the grid algorithm.
                Display::InlineGrid => BoxKind::InlineBlock,
                // A block-level table container is a BlockContainer laid out by
                // the table algorithm (dispatched on display in block.rs, E7-M3).
                Display::Table => BoxKind::BlockContainer,
                // An inline-table is an atomic inline laid out by the table algo.
                Display::InlineTable => BoxKind::InlineBlock,
                // Internal table structure boxes are block containers; the table
                // algorithm walks them and positions their cells directly.
                Display::TableRowGroup => BoxKind::BlockContainer,
                Display::TableRow => BoxKind::BlockContainer,
                Display::TableCell => BoxKind::BlockContainer,
            };
            let mut b = LayoutBox::new(kind, BoxStyleRef::Node(id));
            b.children = build_children(doc, styled, id);
            // Flex/grid container: turn its in-flow children into items —
            // whitespace-only runs dropped, inline-level runs wrapped in
            // anonymous blocks so each becomes a block-level item (§2).
            if matches!(
                display,
                Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
            ) {
                b.children = flexify_children(std::mem::take(&mut b.children), id);
            }
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
            // E7-M2: ::before / ::after generated boxes (after children + marker).
            if let Some(before) = make_pseudo(styled, id, PseudoElement::Before) {
                b.children.insert(0, before);
            }
            if let Some(after) = make_pseudo(styled, id, PseudoElement::After) {
                b.children.push(after);
            }
            Some(b)
        }
        _ => None,
    }
}

/// Build the generated inline box for `id`'s `::before`/`::after`, or `None` if
/// no pseudo was generated (no side-table entry) or the pseudo is `display:none`
/// (E7-M2). The box is an `InlineBox` carrying a single `TextRun` with the
/// content string; an empty string still yields a box (for bg/border).
fn make_pseudo(styled: &StyledTree, id: NodeId, side: PseudoElement) -> Option<LayoutBox> {
    let (pstyle, text) = styled.pseudo(id, side)?;
    if pstyle.display == Display::None {
        return None;
    }
    let mut gen = LayoutBox::new(BoxKind::InlineBox, BoxStyleRef::Generated { origin: id, side });
    if !text.is_empty() {
        let mut run = LayoutBox::new(BoxKind::TextRun, BoxStyleRef::Generated { origin: id, side });
        run.text = Some(text.clone());
        gen.children.push(run);
    }
    Some(gen)
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
            // ws collapsing/dropping only applies in collapsing modes; pre*
            // text nodes keep their (structural) whitespace.
            let ws = styled.get(elem).map(|s| s.white_space).unwrap_or(WhiteSpace::Normal);
            if ws.collapses() {
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

/// Turn a flex container's raw children into flex items: each block-level child
/// passes through as its own item; each maximal run of inline-level children is
/// wrapped in an `AnonymousBlock` so it becomes one block-level item. (For the
/// common case of element children that are already block-level, this is a
/// no-op.) Out-of-flow children are already `BlockContainer`s here and pass
/// through; the flex algorithm later skips them via their style.
fn flexify_children(children: Vec<LayoutBox>, elem: NodeId) -> Vec<LayoutBox> {
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
        BoxStyleRef::Generated { origin, side } => styled
            .pseudo_style(*origin, *side)
            .cloned()
            .unwrap_or_else(ComputedStyle::initial),
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

    // --- E6-M3: white-space-aware processing + text-transform ---

    #[test]
    fn process_text_pre_keeps_newline_and_spaces() {
        let out = process_text("a\n b", WhiteSpace::Pre);
        assert!(out.contains('\n'), "pre keeps \\n: {out:?}");
        assert!(out.contains(" b"), "pre keeps leading space: {out:?}");
    }

    #[test]
    fn process_text_normal_collapses() {
        assert_eq!(process_text("a\n b", WhiteSpace::Normal), "a b");
    }

    #[test]
    fn process_text_pre_line_collapses_spaces_keeps_newline() {
        assert_eq!(process_text("a   \n  b", WhiteSpace::PreLine), "a\nb");
    }

    #[test]
    fn process_text_pre_tab_to_space() {
        assert_eq!(process_text("a\tb", WhiteSpace::Pre), "a b");
    }

    #[test]
    fn transform_text_cases() {
        assert_eq!(transform_text("héllo", TextTransform::Uppercase), "HÉLLO");
        assert_eq!(transform_text("ABC", TextTransform::Lowercase), "abc");
        assert_eq!(transform_text("two words", TextTransform::Capitalize), "Two Words");
        assert_eq!(transform_text("keep", TextTransform::None), "keep");
    }
}
