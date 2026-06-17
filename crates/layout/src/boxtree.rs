//! Box tree model + generation (§1.2, §2): `BoxKind`, `BoxStyleRef`,
//! `LayoutBox`, whitespace collapsing, and the anonymous-block rule.

use starfish_dom::{Document, NodeKind};
use starfish_style::{
    ComputedStyle, Content, Display, Float, ListStyleType, NodeId, Position, PseudoElement,
    StyledTree, TextTransform, Viewport, WhiteSpace,
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
    /// An inline replaced `<svg>` root. Carries the svg element's `NodeId` in
    /// `style`; its used size is in `dimensions`. Its DOM children are NOT
    /// turned into LayoutBoxes — the SVG painter walks the DOM subtree directly
    /// (E9-M1).
    Svg,
    /// A native text form control (`<input>` text-like, `<textarea>`,
    /// `<button>`). An atomic replaced-style box: carries the element's `NodeId`
    /// in `style`, no children built. The displayed text is resolved from the DOM
    /// at sizing (inline.rs) / paint (display.rs) time (E14-M1).
    FormControl,
    /// A `<video>`/`<audio>` element (E15-M3). An atomic inline replaced-style
    /// box with no children built: carries the `<video poster>` url in `text`
    /// (audio → `None`). Paints the poster image or a placeholder box.
    Media,
    /// A `<canvas>` element (E20-M1). An atomic inline replaced-style box with no
    /// children built: carries the canvas `NodeId` in `style`; its used size is in
    /// `dimensions` (HTML width/height attrs, default 300×150). Paint replays the
    /// recorded 2D ops into a backing pixmap and composites it into this box.
    Canvas,
}

/// A parsed SVG `viewBox="minX minY width height"` (E9-M1 §4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Parse a `viewBox` attribute. `None` unless it is exactly four finite numbers
/// with a positive width and height (E9-M1 §4).
pub fn parse_view_box(s: Option<&str>) -> Option<ViewBox> {
    let s = s?;
    let n: Vec<f32> = s
        .split([',', ' ', '\t', '\n', '\r'])
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    if n.len() == 4 && n.iter().all(|v| v.is_finite()) && n[2] > 0.0 && n[3] > 0.0 {
        Some(ViewBox {
            x: n[0],
            y: n[1],
            w: n[2],
            h: n[3],
        })
    } else {
        None
    }
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
    /// E31-M3: when this box is a subgrid, the parent grid's spanned column
    /// widths injected before layout. `None` for a normal box.
    pub subgrid_cols: Option<Vec<f32>>,
    /// E56-M2: `initial-letter` drop-cap multiplier for a `::first-letter` run —
    /// the run's font-size is scaled by this factor (≈ size in lines). `None` =
    /// normal first-letter run.
    pub initial_letter_scale: Option<f32>,
}

impl LayoutBox {
    pub fn new(kind: BoxKind, style: BoxStyleRef) -> LayoutBox {
        LayoutBox {
            kind,
            style,
            text: None,
            dimensions: Dimensions::default(),
            children: Vec::new(),
            subgrid_cols: None,
            initial_letter_scale: None, // E56-M2
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
                | BoxKind::Svg
                | BoxKind::FormControl
                | BoxKind::Media
                | BoxKind::Canvas
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
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => preserve_ws(raw),
        WhiteSpace::PreLine => collapse_ws_keep_newlines(raw),
    }
}

/// `pre`/`pre-wrap`/`break-spaces`: keep all whitespace verbatim, but normalize
/// `\r\n`/`\r` → `\n`. `\t` is PRESERVED (E22-M1 tab-size); inline layout
/// advances a preserved tab to the next tab stop.
fn preserve_ws(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
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
fn build_node(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    parent_elem: NodeId,
    vp: Viewport,
    images: &dyn crate::ImageSource, // E53-M1: list-style-image availability
) -> Option<LayoutBox> {
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
            // E36-M1: <details> disclosure widget. Always shows its <summary>
            // (or a synthesized "Details" label) with a disclosure triangle
            // marker; shows its other children only when `open` is present.
            if doc.tag_name(id) == Some("details") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Block);
                if display == Display::None {
                    return None;
                }
                return Some(build_details(doc, styled, id, vp, images));
            }
            // E56-M1: <ruby> annotation. Stacks the <rt> annotation row above the
            // base content row inside an atomic inline-block (rt first → on top).
            if doc.tag_name(id) == Some("ruby") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::InlineBlock);
                if display == Display::None {
                    return None;
                }
                return Some(build_ruby(doc, styled, id, vp, images));
            }
            // Replaced element: <img> with a src → a leaf Image box (no children).
            if doc.tag_name(id) == Some("img") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                // E15-M2: responsive source selection (srcset/sizes/<picture>);
                // a plain `<img src=x>` resolves to `x` exactly as the raw
                // `get_attribute("src")` did. <img> with no resolvable src → no box.
                let src = crate::responsive::resolve_img_src(
                    doc,
                    id,
                    vp,
                    crate::responsive::DEVICE_PIXEL_RATIO,
                )?;
                let mut b = LayoutBox::new(BoxKind::Image, BoxStyleRef::Node(id));
                b.text = Some(src);
                return Some(b);
            }
            // Replaced element: <svg> → a leaf Svg box (no children built; the
            // SVG painter walks the DOM subtree directly, E9-M1 §3.1).
            if doc.tag_name(id) == Some("svg") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                return Some(LayoutBox::new(BoxKind::Svg, BoxStyleRef::Node(id)));
            }
            // Replaced media: <video>/<audio> → a leaf Media box (no children
            // built; the painter draws the poster or a placeholder, E15-M3).
            // `<video poster>` is carried in `text` (audio → None).
            if matches!(doc.tag_name(id), Some("video") | Some("audio")) {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                let mut b = LayoutBox::new(BoxKind::Media, BoxStyleRef::Node(id));
                if doc.tag_name(id) == Some("video") {
                    b.text = doc.get_attribute(id, "poster").map(str::to_string);
                }
                return Some(b);
            }
            // Replaced element: <canvas> → a leaf Canvas box (no children built;
            // paint replays the recorded 2D ops into a backing pixmap, E20-M1).
            if doc.tag_name(id) == Some("canvas") {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                return Some(LayoutBox::new(BoxKind::Canvas, BoxStyleRef::Node(id)));
            }
            // Native text form control (`<input>` text-like / `<textarea>` /
            // `<button>`): a leaf atomic replaced-style box, no children built
            // (E14-M1). select/checkbox/radio/hidden return None from
            // form_control_kind, so they keep their default inline-block path.
            if crate::form::form_control_kind(doc, id).is_some() {
                let display = styled.get(id).map(|s| s.display).unwrap_or(Display::Inline);
                if display == Display::None {
                    return None;
                }
                return Some(LayoutBox::new(BoxKind::FormControl, BoxStyleRef::Node(id)));
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
                // E34-M1: `display:contents` is spliced away in build_children;
                // reaching here means it slipped through (e.g. the root element
                // is contents) — fall back to a block container, not a broken box.
                Display::Contents => BoxKind::BlockContainer,
                // E34-M2: `display:flow-root` is a block-level container laid
                // out by the block algorithm; it always establishes a new BFC
                // (handled in block.rs::layout_block_children).
                Display::FlowRoot => BoxKind::BlockContainer,
            };
            let mut b = LayoutBox::new(kind, BoxStyleRef::Node(id));
            b.children = build_children(doc, styled, id, vp, images);
            // E35-M3: `::first-letter` splits the first typographic letter of this
            // block's first in-flow text into its own pseudo-styled run. Only for
            // block-level containers (the pseudo's host); no rule → no split →
            // byte-identical. Done before marker/::before/::after are prepended.
            if kind == BoxKind::BlockContainer
                && styled.pseudo(id, PseudoElement::FirstLetter).is_some()
            {
                apply_first_letter(&mut b.children, id, styled);
            }
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
                if let Some(marker) = make_marker(doc, styled, id, images) {
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

// E36-M1: build the box for a `<details>` disclosure widget. The summary (the
// first `<summary>` element child, or a synthesized "Details" label) is always
// shown with a disclosure-triangle marker (▸ closed, ▾ open) prepended. The
// remaining children are built only when the `open` attribute is present.
fn build_details(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    vp: Viewport,
    images: &dyn crate::ImageSource, // E53-M1
) -> LayoutBox {
    let is_open = doc.get_attribute(id, "open").is_some();
    let summary_child = doc
        .composed_children(id)
        .into_iter()
        .find(|&c| doc.tag_name(c) == Some("summary"));

    // The disclosure triangle marker (▼ open, ▶ closed). A Marker box carrying
    // the triangle char + a trailing space, styled by the details element. Use
    // the BLACK *-POINTING TRIANGLE glyphs (U+25BC/U+25B6), which the vendored
    // DejaVu fonts cover, rather than the small U+25BE/U+25B8 (no glyph).
    let mut marker = LayoutBox::new(BoxKind::TextRun, BoxStyleRef::Node(id));
    marker.text = Some(if is_open {
        "\u{25BC} ".to_string() // ▼
    } else {
        "\u{25B6} ".to_string() // ▶
    });

    // Build the summary box, or synthesize a default "Details" one.
    let mut summary = match summary_child {
        Some(s) => build_node(doc, styled, s, id, vp, images)
            .unwrap_or_else(|| LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Node(id))),
        None => {
            let mut anon = LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Anonymous(id));
            let mut label = LayoutBox::new(BoxKind::TextRun, BoxStyleRef::Anonymous(id));
            label.text = Some("Details".to_string());
            anon.children.push(label);
            anon
        }
    };
    summary.children.insert(0, marker);

    let mut b = LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Node(id));
    b.children.push(summary);

    // The remaining (non-summary) children render only when `open`.
    if is_open {
        for child in doc.composed_children(id) {
            if Some(child) == summary_child {
                continue;
            }
            // Drop whitespace-only text between block siblings (mirrors
            // build_children's collapsing for the common-case flow).
            if let NodeKind::Text(t) = doc.kind(child) {
                let ws = styled
                    .get(id)
                    .map(|s| s.white_space)
                    .unwrap_or(WhiteSpace::Normal);
                if ws.collapses() {
                    let collapsed = collapse_ws(t);
                    if collapsed.is_empty() || collapsed == " " {
                        let prev_inline =
                            b.children.last().map(|c| c.is_inline_level()).unwrap_or(false);
                        if collapsed.is_empty() || !prev_inline {
                            continue;
                        }
                    }
                }
            }
            if let Some(cb) = build_node(doc, styled, child, id, vp, images) {
                b.children.push(cb);
            }
        }
        b.children = wrap_anonymous_blocks(std::mem::take(&mut b.children), id);
    }
    b
}

// E56-M1: build the box for a `<ruby>` annotation. An atomic inline-block whose
// children are two stacked block-level rows: the `<rt>` annotation row first
// (rendered on top, smaller via the UA `rt{font-size:50%}` rule), then the base
// content row (the ruby's children except `<rt>`/`<rp>`). Both rows inherit the
// ruby's `text-align:center`, so each centers over the wider row's column; the
// inline-block shrink-wraps to that width. `<rp>` fallback parens are dropped.
fn build_ruby(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    vp: Viewport,
    images: &dyn crate::ImageSource,
) -> LayoutBox {
    // Base content row: the ruby's children except <rt>/<rp>, wrapped in an
    // anonymous block so it lays out as one block-level row.
    let mut base = LayoutBox::new(BoxKind::AnonymousBlock, BoxStyleRef::Anonymous(id));
    let mut rt_box: Option<LayoutBox> = None;
    for child in doc.composed_children(id) {
        match doc.tag_name(child) {
            Some("rp") => continue, // parenthesis fallback: not rendered
            Some("rt") => {
                // First <rt> becomes the annotation row (a block via the UA rule).
                if rt_box.is_none() {
                    rt_box = build_node(doc, styled, child, id, vp, images);
                }
            }
            _ => {
                if let Some(cb) = build_node(doc, styled, child, id, vp, images) {
                    base.children.push(cb);
                }
            }
        }
    }

    let mut b = LayoutBox::new(BoxKind::InlineBlock, BoxStyleRef::Node(id));
    // rt first → stacked above the base by block layout.
    if let Some(rt) = rt_box {
        b.children.push(rt);
    }
    b.children.push(base);
    b
}

/// Build the generated inline box for `id`'s `::before`/`::after`, or `None` if
/// no pseudo was generated (no side-table entry) or the pseudo is `display:none`
/// (E7-M2). The box is an `InlineBox` carrying a single `TextRun` with the
/// content string; an empty string still yields a box (for bg/border).
fn make_pseudo(styled: &StyledTree, id: NodeId, side: PseudoElement) -> Option<LayoutBox> {
    let (pstyle, text) = styled.pseudo(id, side.clone())?;
    if pstyle.display == Display::None {
        return None;
    }
    let mut gen = LayoutBox::new(
        BoxKind::InlineBox,
        BoxStyleRef::Generated {
            origin: id,
            side: side.clone(),
        },
    );
    // E53-M2: `content: url(...)` → the pseudo wraps an image replaced box whose
    // src is the (already quote-stripped) url carried in `text`.
    if matches!(pstyle.content, Content::Url(_)) {
        let mut img = LayoutBox::new(
            BoxKind::Image,
            BoxStyleRef::Generated {
                origin: id,
                side,
            },
        );
        img.text = Some(text.clone());
        gen.children.push(img);
        return Some(gen);
    }
    if !text.is_empty() {
        let mut run = LayoutBox::new(
            BoxKind::TextRun,
            BoxStyleRef::Generated { origin: id, side },
        );
        run.text = Some(text.clone());
        gen.children.push(run);
    }
    Some(gen)
}

// E35-M3: split the first typographic letter of a block's first in-flow text
// into its own `TextRun` styled by the `::first-letter` pseudo. Walks `children`
// in document order, descending into inline boxes, to find the first `TextRun`
// with a non-whitespace char; skips leading whitespace-only runs and
// Marker/replaced boxes. Returns `true` once a split has been performed so the
// recursion stops. MVP: first `char` only (no grapheme clusters, no leading
// punctuation). Bounded recursion via `depth`.
fn apply_first_letter(children: &mut Vec<LayoutBox>, origin: NodeId, styled: &StyledTree) {
    // E56-M2: if the first-letter pseudo declares `initial-letter: <n>`, the
    // generated run is a drop cap — its font is scaled by `n` so the glyph spans
    // ~n lines. `None` (no `initial-letter`) leaves the run untouched (E35).
    let scale = styled
        .pseudo_style(origin, PseudoElement::FirstLetter)
        .and_then(|s| s.initial_letter);
    first_letter_walk(children, origin, 0, scale);
}

fn first_letter_walk(
    children: &mut Vec<LayoutBox>,
    origin: NodeId,
    depth: usize,
    scale: Option<f32>, // E56-M2: drop-cap font multiplier, if any
) -> bool {
    if depth > 32 {
        return false;
    }
    let mut i = 0;
    while i < children.len() {
        match children[i].kind {
            BoxKind::TextRun => {
                let text = children[i].text.clone().unwrap_or_default();
                // Skip whitespace-only runs (no typographic letter here).
                if text.trim().is_empty() {
                    i += 1;
                    continue;
                }
                // First char is the first-letter; split on its byte boundary.
                let split = text.char_indices().nth(1).map(|(b, _)| b);
                let (first, rest) = match split {
                    Some(b) => (text[..b].to_string(), text[b..].to_string()),
                    // Single-char run: the whole run is the first letter.
                    None => (text, String::new()),
                };
                let mut fl = LayoutBox::new(
                    BoxKind::TextRun,
                    BoxStyleRef::Generated {
                        origin,
                        side: PseudoElement::FirstLetter,
                    },
                );
                fl.text = Some(first);
                fl.initial_letter_scale = scale; // E56-M2
                if rest.is_empty() {
                    // Original run becomes the first-letter run (no empty remainder).
                    children[i] = fl;
                } else {
                    children[i].text = Some(rest);
                    children.insert(i, fl);
                }
                return true;
            }
            // Descend into inline structure to find the first text run.
            BoxKind::InlineBox => {
                if first_letter_walk(&mut children[i].children, origin, depth + 1, scale) {
                    return true;
                }
                i += 1;
            }
            // Marker / replaced / atomic boxes are not text — stop the search at
            // the first such box only if it precedes any text? Per MVP, skip them
            // and keep scanning following siblings for the first text run.
            _ => {
                i += 1;
            }
        }
    }
    false
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
fn make_marker(
    doc: &Document,
    styled: &StyledTree,
    li: NodeId,
    images: &dyn crate::ImageSource, // E53-M1
) -> Option<LayoutBox> {
    let st = styled.get(li)?;
    // E53-M1: `list-style-image: url(...)` uses the image as the marker when it
    // decoded; an absent/undecodable image falls through to the text marker. The
    // marker keeps `BoxKind::Marker` (so it's still hung in the gutter) but its
    // `text` carries the url — equal to `list_style_image`, which is how the
    // inline pass recognizes an image marker and sizes/paints it as an image.
    if let Some(src) = &st.list_style_image {
        if images.intrinsic_size(src).is_some() {
            let mut m = LayoutBox::new(BoxKind::Marker, BoxStyleRef::Node(li));
            m.text = Some(src.to_string());
            return Some(m);
        }
    }
    // E42-M3: a custom `@counter-style` named by `list-style-type` formats the
    // ordinal with its symbols + prefix/suffix, overriding the built-in keyword.
    let label = if let Some(cs) = &st.list_style_custom {
        cs.format_marker(ordinal_of(doc, li) as i32)
    } else {
        match st.list_style_type {
            ListStyleType::None => return None,
            ListStyleType::Disc => "\u{2022}".to_string(), // •
            ListStyleType::Circle => "\u{25E6}".to_string(), // ◦
            ListStyleType::Square => "\u{25AA}".to_string(), // ▪
            ListStyleType::Decimal => format!("{}.", ordinal_of(doc, li)),
        }
    };
    // E35-M1: a `::marker` rule on this list item styles the marker box (read via
    // BoxStyleRef::Generated → styled.pseudo_style) and, with a `content` string,
    // replaces the bullet/ordinal text. No `::marker` rule → byte-identical path.
    let (style_ref, text) = match styled.pseudo(li, PseudoElement::Marker) {
        Some((_, content)) => {
            let text = if content.is_empty() {
                label
            } else {
                content.clone()
            };
            (
                BoxStyleRef::Generated {
                    origin: li,
                    side: PseudoElement::Marker,
                },
                text,
            )
        }
        None => (BoxStyleRef::Node(li), label),
    };
    let mut m = LayoutBox::new(BoxKind::Marker, style_ref);
    m.text = Some(text);
    Some(m)
}

/// 1-based position of `li` among its `<li>` element siblings (for `<ol>`).
/// No `start`/`value`/`counter-reset` support (M1).
fn ordinal_of(doc: &Document, li: NodeId) -> usize {
    let Some(parent) = doc.parent(li) else {
        return 1;
    };
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
fn build_children(
    doc: &Document,
    styled: &StyledTree,
    elem: NodeId,
    vp: Viewport,
    images: &dyn crate::ImageSource, // E53-M1
) -> Vec<LayoutBox> {
    let mut raw: Vec<LayoutBox> = Vec::new();
    // E33-M2: composed-tree walk. A shadow host iterates its shadow tree and a
    // `<slot>` iterates its assigned light children (or fallback content when
    // empty), while still passing `elem` as `parent_elem` so text inherits its
    // white-space. A non-shadow, non-slot element has `composed_children ==
    // children` → byte-identical.
    for child in doc.composed_children(elem) {
        // E34-M1: `display:contents` element generates no box — splice its own
        // children directly into this flow at its position. The recursive
        // build_children passes `child` as parent_elem (inheritance/white-space
        // flow through) and itself flattens nested contents and slot redirection.
        if matches!(doc.kind(child), NodeKind::Element(_))
            && styled.get(child).map(|s| s.display) == Some(Display::Contents)
        {
            raw.extend(build_children(doc, styled, child, vp, images));
            continue;
        }
        // Drop a whitespace-only text node that is not adjacent to inline
        // content (i.e. its collapsed form is a lone space and the previous
        // generated box is block-level or absent — it sits between blocks).
        if let NodeKind::Text(t) = doc.kind(child) {
            // ws collapsing/dropping only applies in collapsing modes; pre*
            // text nodes keep their (structural) whitespace.
            let ws = styled
                .get(elem)
                .map(|s| s.white_space)
                .unwrap_or(WhiteSpace::Normal);
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
        if let Some(b) = build_node(doc, styled, child, elem, vp, images) {
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
pub(crate) fn build_box_tree(
    doc: &Document,
    styled: &StyledTree,
    root_element: NodeId,
    vp: Viewport,
    images: &dyn crate::ImageSource, // E53-M1
) -> LayoutBox {
    let mut root = LayoutBox::new(BoxKind::BlockContainer, BoxStyleRef::Node(root_element));
    root.children = build_children(doc, styled, root_element, vp, images);
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
            .pseudo_style(*origin, side.clone())
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
    fn process_text_pre_keeps_tab() {
        // E22-M1: tabs are preserved through to inline layout (tab-size).
        assert_eq!(process_text("a\tb", WhiteSpace::Pre), "a\tb");
    }

    #[test]
    fn transform_text_cases() {
        assert_eq!(transform_text("héllo", TextTransform::Uppercase), "HÉLLO");
        assert_eq!(transform_text("ABC", TextTransform::Lowercase), "abc");
        assert_eq!(
            transform_text("two words", TextTransform::Capitalize),
            "Two Words"
        );
        assert_eq!(transform_text("keep", TextTransform::None), "keep");
    }
}
