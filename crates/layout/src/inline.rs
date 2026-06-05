//! Inline layout (§4): greedy word-wrap into `LineBox` children with `TextRun`
//! fragments. text-align Left/Center/Right honored (Justify → Left).

use starfish_style::{LineHeight, StyledTree, TextAlign};

use crate::block::layout_inline_block;
use crate::boxtree::{style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::dimensions::{Dimensions, Rect};
use crate::measure::TextMeasurer;

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
    styled: &'a StyledTree,
    m: &'a dyn TextMeasurer,
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
                layout_inline_block(&mut sub, c.cb, c.styled, c.m);
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

/// Recursively translate a box subtree's absolute content origins by `(dx,dy)`.
fn translate_box(b: &mut LayoutBox, dx: f32, dy: f32) {
    b.dimensions.content.x += dx;
    b.dimensions.content.y += dy;
    for c in &mut b.children {
        translate_box(c, dx, dy);
    }
}

/// Lay out the inline-level children of `b` into `LineBox` children. Returns the
/// total height of all line boxes (consumed by the block as its content height
/// when `height: auto`).
pub(crate) fn layout_inline(b: &mut LayoutBox, styled: &StyledTree, m: &dyn TextMeasurer) -> f32 {
    let container_style = style_of(styled, b);
    let avail = b.dimensions.content.width;
    let origin = b.dimensions.content;
    let align = container_style.text_align;

    let mut collector = Collector {
        styled,
        m,
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

    // Greedy line breaking. Each line is a Vec<PlacedItem>.
    let mut lines: Vec<Vec<PlacedItem>> = Vec::new();
    let mut cur: Vec<PlacedItem> = Vec::new();
    let mut cursor_x = 0.0f32;

    for item in items {
        match item {
            CollectedItem::Word { text: word, style_ref, font_size, line_height: lh, space_before } => {
                let style = match &style_ref {
                    BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
                };
                let weight = style.map(|s| s.font_weight).unwrap_or(container_style.font_weight);
                let w = m.measure(&word, font_size, weight);
                let space_w = if space_before { m.measure(" ", font_size, weight) } else { 0.0 };
                let line_h = used_line_height(font_size, lh);

                if cur.is_empty() {
                    cur.push(PlacedItem::Word { text: word, style_ref, line_height: line_h, x: 0.0, width: w });
                    cursor_x = w;
                } else if cursor_x + space_w + w <= avail {
                    let x = cursor_x + space_w;
                    cur.push(PlacedItem::Word { text: word, style_ref, line_height: line_h, x, width: w });
                    cursor_x = x + w;
                } else {
                    lines.push(std::mem::take(&mut cur));
                    cur.push(PlacedItem::Word { text: word, style_ref, line_height: line_h, x: 0.0, width: w });
                    cursor_x = w;
                }
            }
            CollectedItem::Atomic { atom, width, height, space_before } => {
                // The margin-box height sets the line height for this atom.
                let space_w = if space_before { 0.5 * container_style.font_size } else { 0.0 };
                if cur.is_empty() {
                    cur.push(PlacedItem::Atomic { atom, line_height: height, x: 0.0, width });
                    cursor_x = width;
                } else if cursor_x + space_w + width <= avail {
                    let x = cursor_x + space_w;
                    cur.push(PlacedItem::Atomic { atom, line_height: height, x, width });
                    cursor_x = x + width;
                } else {
                    lines.push(std::mem::take(&mut cur));
                    cur.push(PlacedItem::Atomic { atom, line_height: height, x: 0.0, width });
                    cursor_x = width;
                }
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // Build LineBox children with absolute geometry.
    let mut line_y = 0.0f32;
    let mut line_boxes: Vec<LayoutBox> = Vec::new();

    for line in lines {
        let line_height = line.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);

        // text-align offset uses the line's used width — the rightmost extent of
        // every item (words AND atomics). The offset shifts every item equally.
        let used_width = line.iter().map(|w| w.right_edge()).fold(0.0f32, f32::max);
        let slack = (avail - used_width).max(0.0);
        let offset = match align {
            TextAlign::Right => slack,
            TextAlign::Center => slack / 2.0,
            TextAlign::Left | TextAlign::Justify => 0.0,
        };

        let mut lb = LayoutBox::new(BoxKind::LineBox, b.style.clone());
        lb.dimensions.content = Rect {
            x: origin.x,
            y: origin.y + line_y,
            width: avail,
            height: line_height,
        };

        for item in line {
            match item {
                PlacedItem::Word { text, style_ref, line_height: lh, x, width } => {
                    let mut frag = LayoutBox::new(BoxKind::TextRun, style_ref);
                    frag.text = Some(text);
                    frag.dimensions.content = Rect {
                        x: origin.x + offset + x,
                        y: origin.y + line_y,
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
                    let target_y = origin.y + line_y;
                    translate_box(&mut sub, target_x - cur_mb.x, target_y - cur_mb.y);
                    lb.children.push(sub);
                }
            }
        }

        line_boxes.push(lb);
        line_y += line_height;
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
