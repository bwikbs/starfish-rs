//! Inline layout (§4): greedy word-wrap into `LineBox` children with `TextRun`
//! fragments. text-align Left/Center/Right honored (Justify → Left).

use starfish_dom::{Document, NodeId};
use starfish_style::{Length, LineHeight, StyledTree, TextAlign};

use crate::block::{layout_inline_block, resolve, resolve_or_zero};
use crate::boxtree::{style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::dimensions::{Dimensions, Rect};
use crate::float::FloatContext;
use crate::measure::{ImageSource, TextMeasurer};

/// Used line-height in px for a style (§4.3).
fn used_line_height(font_size: f32, lh: LineHeight) -> f32 {
    match lh {
        LineHeight::Px(v) => v,
        LineHeight::Number(n) => n * font_size,
        LineHeight::Normal => 1.2 * font_size,
    }
}

/// One placed item on a line, before it is committed to a `LineBox`.
enum PlacedItem {
    /// A word fragment → becomes a `TextRun`.
    Word {
        text: String,
        style_ref: BoxStyleRef,
        line_height: f32,
        /// x relative to the inline container's content origin.
        x: f32,
        width: f32,
    },
    /// A pre-laid-out inline-block sub-box (its index into `atomics`).
    Atomic {
        atom: usize,
        line_height: f32,
        /// x of the atom's margin-box left, relative to the content origin.
        x: f32,
        /// margin-box width (advance on the line).
        width: f32,
    },
}

impl PlacedItem {
    fn line_height(&self) -> f32 {
        match self {
            PlacedItem::Word { line_height, .. } | PlacedItem::Atomic { line_height, .. } => {
                *line_height
            }
        }
    }
    fn right_edge(&self) -> f32 {
        match self {
            PlacedItem::Word { x, width, .. } | PlacedItem::Atomic { x, width, .. } => x + width,
        }
    }
}

/// A measurable inline item collected from the flattened inline subtree.
enum CollectedItem {
    /// A word with the metadata needed to measure it and decide whether a
    /// collapsed space precedes it.
    Word {
        text: String,
        style_ref: BoxStyleRef,
        font_size: f32,
        line_height: LineHeight,
        /// True iff source whitespace was collapsed immediately before this word
        /// within the flattened inline run (false for the very first word).
        space_before: bool,
    },
    /// A pre-laid-out inline-block; index into the side `atomics` Vec, plus its
    /// margin-box width / height.
    Atomic {
        atom: usize,
        width: f32,
        height: f32,
        space_before: bool,
    },
}

/// A marker pulled out of the word stream, to be hung in the gutter (§3.2).
struct PulledMarker {
    text: String,
    style_ref: BoxStyleRef,
}

/// Threaded state for `collect_items`.
struct Collector<'a> {
    doc: &'a Document,
    styled: &'a StyledTree,
    m: &'a dyn TextMeasurer,
    images: &'a dyn ImageSource,
    /// Containing block for laying out inline-block sub-boxes.
    cb: Dimensions,
    out: Vec<CollectedItem>,
    /// Pre-laid-out inline-block sub-boxes, indexed by `CollectedItem::Atomic`.
    atomics: Vec<LayoutBox>,
    pending_space: bool,
    /// A leading `Marker` child, pulled out of the flow.
    marker: Option<PulledMarker>,
}

/// Flatten the inline subtree into a flat list of items in document order,
/// carrying a per-item `space_before` flag. Plain inline boxes are flattened
/// into the flow (their box-model is ignored, §4.2/§7); inline-blocks are laid
/// out as atoms; a leading marker is pulled aside.
fn collect_items(c: &mut Collector, b: &LayoutBox) {
    for child in &b.children {
        match child.kind {
            // Pull the marker aside (only the first one matters in practice).
            BoxKind::Marker if c.marker.is_none() => {
                c.marker = Some(PulledMarker {
                    text: child.text.clone().unwrap_or_default(),
                    style_ref: child.style.clone(),
                });
            }
            BoxKind::TextRun => {
                let style = style_of(c.styled, child);
                let text = child.text.clone().unwrap_or_default();
                if text.starts_with(' ') {
                    c.pending_space = true;
                }
                for word in text.split(' ') {
                    if word.is_empty() {
                        continue;
                    }
                    c.out.push(CollectedItem::Word {
                        text: word.to_string(),
                        style_ref: child.style.clone(),
                        font_size: style.font_size,
                        line_height: style.line_height,
                        space_before: c.pending_space,
                    });
                    c.pending_space = true;
                }
                c.pending_space = text.ends_with(' ');
            }
            BoxKind::InlineBlock => {
                // Lay out the inline-block's own block to get its used size.
                let mut sub = child.clone();
                layout_inline_block(&mut sub, c.cb, c.styled, c.doc, c.m, c.images);
                let mb = sub.dimensions.margin_box();
                let atom = c.atomics.len();
                c.atomics.push(sub);
                c.out.push(CollectedItem::Atomic {
                    atom,
                    width: mb.width,
                    height: mb.height,
                    space_before: c.pending_space,
                });
                c.pending_space = false;
            }
            BoxKind::Image => {
                // Replaced element: a leaf box sized from intrinsic/attrs, placed
                // on the line like an atomic inline (§5/§6.1). No child layout.
                let id = child.style.node();
                let style = style_of(c.styled, child);
                let cbw = c.cb.content.width;
                let src = child.text.clone().unwrap_or_default();
                let intrinsic = c.images.intrinsic_size(&src);
                // CSS width/height (definite) override the HTML attrs.
                let attr_w = resolve(style.width, cbw).or_else(|| attr_px(c.doc, id, "width"));
                let attr_h = match style.height {
                    Length::Auto => attr_px(c.doc, id, "height"),
                    h => resolve(h, cbw),
                };
                let (w, h) = replaced_size(intrinsic, attr_w, attr_h);

                let mut sub = child.clone();
                sub.dimensions = Dimensions::default();
                sub.dimensions.content.width = w;
                sub.dimensions.content.height = h;
                sub.dimensions.margin.left = resolve_or_zero(style.margin_left, cbw);
                sub.dimensions.margin.right = resolve_or_zero(style.margin_right, cbw);
                sub.dimensions.margin.top = resolve_or_zero(style.margin_top, cbw);
                sub.dimensions.margin.bottom = resolve_or_zero(style.margin_bottom, cbw);

                let mb = sub.dimensions.margin_box();
                let atom = c.atomics.len();
                c.atomics.push(sub);
                c.out.push(CollectedItem::Atomic {
                    atom,
                    width: mb.width,
                    height: mb.height,
                    space_before: c.pending_space,
                });
                c.pending_space = false;
            }
            BoxKind::InlineBox => collect_items(c, child),
            _ => {}
        }
    }
}

/// Parse an HTML presentational `width`/`height` attribute as a non-negative px
/// count (`"100"` or `"100px"`). `None` if absent / invalid / negative.
fn attr_px(doc: &Document, id: NodeId, name: &str) -> Option<f32> {
    doc.get_attribute(id, name)
        .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// Resolve an `<img>`'s used content size (CSS replaced-element sizing, M4
/// subset, §5). `intrinsic` is the decoded `(w,h)` or `None` for a broken image.
/// `attr_w`/`attr_h` are the resolved width/height (CSS or HTML attr), each in px.
fn replaced_size(
    intrinsic: Option<(f32, f32)>,
    attr_w: Option<f32>,
    attr_h: Option<f32>,
) -> (f32, f32) {
    match (attr_w, attr_h, intrinsic) {
        // both given → use them verbatim
        (Some(w), Some(h), _) => (w.max(0.0), h.max(0.0)),
        // one given + usable intrinsic → scale preserving aspect ratio
        (Some(w), None, Some((iw, ih))) if iw > 0.0 => (w.max(0.0), (w * ih / iw).max(0.0)),
        (None, Some(h), Some((iw, ih))) if ih > 0.0 => ((h * iw / ih).max(0.0), h.max(0.0)),
        // one given, no usable intrinsic → square placeholder
        (Some(w), None, _) => (w.max(0.0), w.max(0.0)),
        (None, Some(h), _) => (h.max(0.0), h.max(0.0)),
        // neither given + decoded → intrinsic size
        (None, None, Some((iw, ih))) => (iw, ih),
        // neither given + broken → zero box (collapses)
        (None, None, None) => (0.0, 0.0),
    }
}

/// Recursively translate a box subtree's absolute content origins by `(dx,dy)`.
pub(crate) fn translate_box(b: &mut LayoutBox, dx: f32, dy: f32) {
    b.dimensions.content.x += dx;
    b.dimensions.content.y += dy;
    for c in &mut b.children {
        translate_box(c, dx, dy);
    }
}

/// Lay out the inline-level children of `b` into `LineBox` children. Returns the
/// total height of all line boxes (consumed by the block as its content height
/// when `height: auto`).
pub(crate) fn layout_inline(
    b: &mut LayoutBox,
    doc: &Document,
    styled: &StyledTree,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    floats: &FloatContext,
) -> f32 {
    let container_style = style_of(styled, b);
    let avail = b.dimensions.content.width;
    let origin = b.dimensions.content;
    let align = container_style.text_align;

    let mut collector = Collector {
        doc,
        styled,
        m,
        images,
        cb: b.dimensions,
        out: Vec::new(),
        atomics: Vec::new(),
        pending_space: false,
        marker: None,
    };
    collect_items(&mut collector, b);
    let items = std::mem::take(&mut collector.out);
    let mut atomics = std::mem::take(&mut collector.atomics);
    let marker = collector.marker.take();

    // Provisional band height for float queries (§3.4): the container's used
    // line-height. Lines below a float return to full width.
    let band_h = used_line_height(container_style.font_size, container_style.line_height);

    // Greedy line breaking, float-aware. The available band `(start, avail)` is
    // recomputed from `floats` at each line's y. `start`/`cursor_x` are relative
    // to `origin.x`.
    let mut lines: Vec<(Vec<PlacedItem>, f32, f32, f32)> = Vec::new(); // (items, line_start, line_avail, line_y)
    let mut cur: Vec<PlacedItem> = Vec::new();
    let mut cursor_x = 0.0f32;
    let mut line_y = 0.0f32;

    // Band for the current (in-progress) line.
    let band = |y: f32| -> (f32, f32) {
        let abs_y = origin.y + y;
        let left_inset = floats.left_offset(abs_y, band_h, origin.x);
        let right_inset = floats.right_offset(abs_y, band_h, origin.x + avail);
        let line_avail = (avail - left_inset - right_inset).max(0.0);
        (left_inset, line_avail)
    };
    let (mut line_start, mut line_avail) = band(line_y);

    for item in items {
        let (width, line_h, space_w): (f32, f32, f32) = match &item {
            CollectedItem::Word { text: word, style_ref, font_size, line_height: lh, space_before } => {
                let style = match style_ref {
                    BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
                };
                let weight = style.map(|s| s.font_weight).unwrap_or(container_style.font_weight);
                let w = m.measure(word, *font_size, weight);
                let space_w = if *space_before { m.measure(" ", *font_size, weight) } else { 0.0 };
                let line_h = used_line_height(*font_size, *lh);
                (w, line_h, space_w)
            }
            CollectedItem::Atomic { width, height, space_before, .. } => {
                let space_w = if *space_before { 0.5 * container_style.font_size } else { 0.0 };
                (*width, *height, space_w)
            }
        };

        // Decide placement x (relative to origin.x).
        let x = if cur.is_empty() {
            line_start
        } else if cursor_x + space_w + width <= line_start + line_avail {
            cursor_x + space_w
        } else {
            // Wrap: commit the current line, advance y, recompute the band.
            let line_height = cur.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);
            lines.push((std::mem::take(&mut cur), line_start, line_avail, line_y));
            line_y += line_height;
            let (s, a) = band(line_y);
            line_start = s;
            line_avail = a;
            line_start
        };
        cursor_x = x + width;

        match item {
            CollectedItem::Word { text: word, style_ref, .. } => {
                cur.push(PlacedItem::Word { text: word, style_ref, line_height: line_h, x, width });
            }
            CollectedItem::Atomic { atom, .. } => {
                cur.push(PlacedItem::Atomic { atom, line_height: line_h, x, width });
            }
        }
    }
    if !cur.is_empty() {
        let line_height = cur.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);
        lines.push((cur, line_start, line_avail, line_y));
        line_y += line_height;
    }

    // Build LineBox children with absolute geometry.
    let mut line_boxes: Vec<LayoutBox> = Vec::new();

    for (line, l_start, l_avail, l_y) in lines {
        let line_height = line.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);

        // text-align offset uses the line's used width relative to the line band.
        let used_width = line
            .iter()
            .map(|w| w.right_edge() - l_start)
            .fold(0.0f32, f32::max);
        let slack = (l_avail - used_width).max(0.0);
        let offset = match align {
            TextAlign::Right => slack,
            TextAlign::Center => slack / 2.0,
            TextAlign::Left | TextAlign::Justify => 0.0,
        };

        let mut lb = LayoutBox::new(BoxKind::LineBox, b.style.clone());
        lb.dimensions.content = Rect {
            x: origin.x + l_start,
            y: origin.y + l_y,
            width: l_avail,
            height: line_height,
        };

        for item in line {
            match item {
                PlacedItem::Word { text, style_ref, line_height: lh, x, width } => {
                    let mut frag = LayoutBox::new(BoxKind::TextRun, style_ref);
                    frag.text = Some(text);
                    frag.dimensions.content = Rect {
                        x: origin.x + offset + x,
                        y: origin.y + l_y,
                        width,
                        height: lh,
                    };
                    lb.children.push(frag);
                }
                PlacedItem::Atomic { atom, x, .. } => {
                    // Translate the pre-laid-out sub-box so its margin-box's
                    // top-left sits at the committed line position. The sub-box
                    // was laid out at the container's content origin (cb), so its
                    // current margin-box top-left is the delta source.
                    let mut sub = std::mem::replace(
                        &mut atomics[atom],
                        LayoutBox::new(BoxKind::InlineBlock, b.style.clone()),
                    );
                    let cur_mb = sub.dimensions.margin_box();
                    let target_x = origin.x + offset + x;
                    let target_y = origin.y + l_y;
                    translate_box(&mut sub, target_x - cur_mb.x, target_y - cur_mb.y);
                    lb.children.push(sub);
                }
            }
        }

        line_boxes.push(lb);
    }

    // Hang the marker into the left gutter of the first line (§3.2).
    if let Some(pm) = marker {
        if let Some(first) = line_boxes.first_mut() {
            let style = match &pm.style_ref {
                BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
            };
            let font_size = style.map(|s| s.font_size).unwrap_or(container_style.font_size);
            let weight = style.map(|s| s.font_weight).unwrap_or(container_style.font_weight);
            let mw = m.measure(&pm.text, font_size, weight);
            let gap = 0.5 * font_size;
            let mut frag = LayoutBox::new(BoxKind::Marker, pm.style_ref);
            frag.text = Some(pm.text);
            frag.dimensions.content = Rect {
                x: origin.x - mw - gap,
                y: first.dimensions.content.y,
                width: mw,
                height: first.dimensions.content.height,
            };
            first.children.insert(0, frag);
        }
    }

    b.children = line_boxes;
    line_y
}
