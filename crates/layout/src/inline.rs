//! Inline layout (§4): greedy word-wrap into `LineBox` children with `TextRun`
//! fragments. text-align Left/Center/Right honored (Justify → Left).

use starfish_dom::{Document, NodeId};
use starfish_style::{
    Direction, Length, LineHeight, Overflow, StyledTree, TextAlign, TextOverflow, UnicodeBidi,
    WhiteSpace, WritingMode,
};
use unicode_bidi::{BidiInfo, Level};

use crate::block::{
    clamp_size, content_from_specified, layout_inline_block, resolve, resolve_or_zero,
};
use crate::boxtree::{style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::cache::LayoutCache;
use crate::dimensions::{Dimensions, Rect};
use crate::float::FloatContext;
use crate::measure::{FontQuery, ImageSource, TextMeasurer};

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
        /// Count of spaces logically before this word (for bidi re-x, §5).
        spaces_before: u16,
        /// Width of one inter-word space (incl. word-spacing) for this run.
        space_w: f32,
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
    /// A word with the metadata needed to measure it and decide how many
    /// spaces precede it (0 = no gap; 1 = a collapsed space; >1 only in
    /// `pre`/`pre-wrap` where runs of spaces are preserved).
    Word {
        text: String,
        style_ref: BoxStyleRef,
        font_size: f32,
        line_height: LineHeight,
        /// Count of spaces immediately before this word (E6-M3 §2.3).
        spaces_before: u16,
        /// letter-spacing px for this run (E6-M3 §4).
        letter_spacing: f32,
        /// word-spacing px for this run (E6-M3 §4).
        word_spacing: f32,
    },
    /// A preserved hard line break (`\n` in pre/pre-wrap/pre-line, E6-M3 §2.3).
    ForcedBreak,
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
    cache: &'a LayoutCache,
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
            // A `LineBox` here is the artifact of a PRIOR inline pass over this
            // same box (e.g. a table cell / flex|grid item re-laid-out by its
            // container's measure → final passes). Inline layout would otherwise
            // be non-idempotent: the original `TextRun "a b"` was already split
            // into word fragments whose inter-word space lives only as the x-gap
            // between them, so naively re-collecting them would drop the space.
            // Re-flatten the stale line back into its words, recovering each
            // word's leading space from that positional gap (§4 idempotence).
            BoxKind::LineBox => collect_stale_line(c, child),
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
                let ws = style.white_space;
                // Split on preserved `\n` into segments (only pre* modes ever
                // carry literal `\n`); a ForcedBreak separates segments.
                let segments: Vec<&str> = text.split('\n').collect();
                for (i, seg) in segments.iter().enumerate() {
                    if i > 0 {
                        c.out.push(CollectedItem::ForcedBreak);
                        c.pending_space = false;
                    }
                    collect_segment(c, child, &style, ws, seg);
                }
            }
            BoxKind::InlineBlock => {
                // Lay out the inline-block's own block to get its used size.
                let mut sub = child.clone();
                layout_inline_block(&mut sub, c.cb, c.styled, c.doc, c.m, c.images, c.cache);
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
                // box-sizing folds padding+border into a CSS width/height; the
                // HTML width/height attr fallback is always content-box (E13-M1).
                let (pb_h, pb_v) = replaced_pb(&style, cbw);
                // CSS width/height (definite) override the HTML attrs.
                let attr_w = resolve(style.width, cbw)
                    .map(|w| content_from_specified(w, style.box_sizing, pb_h))
                    .or_else(|| attr_px(c.doc, id, "width"));
                let attr_h = match style.height {
                    Length::Auto => attr_px(c.doc, id, "height"),
                    h => resolve(h, cbw).map(|v| content_from_specified(v, style.box_sizing, pb_v)),
                };
                let (w, h) = replaced_size(intrinsic, attr_w, attr_h);
                // min/max clamp each axis independently (aspect ratio NOT
                // preserved — documented). Guarded so default images are
                // unchanged (max=Auto→∞, min=Auto→0).
                let w = clamp_replaced(
                    w,
                    style.min_width,
                    style.max_width,
                    cbw,
                    style.box_sizing,
                    pb_h,
                );
                let h = clamp_replaced(
                    h,
                    drop_percent(style.min_height),
                    drop_percent(style.max_height),
                    cbw,
                    style.box_sizing,
                    pb_v,
                );

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
            BoxKind::Svg => {
                // Inline replaced <svg>: sized from width/height attrs, else the
                // viewBox, else the CSS replaced default (E9-M1 §3.2). No child
                // layout — the painter walks the DOM subtree.
                let id = child.style.node();
                let style = style_of(c.styled, child);
                let cbw = c.cb.content.width;
                let (iw, ih) = svg_intrinsic_size(c.doc, id);
                let (pb_h, pb_v) = replaced_pb(&style, cbw);
                // CSS width/height (definite) override the attrs, like <img>.
                let w = resolve(style.width, cbw)
                    .map(|v| content_from_specified(v, style.box_sizing, pb_h))
                    .unwrap_or(iw);
                let h = match style.height {
                    Length::Auto => ih,
                    hh => resolve(hh, cbw)
                        .map(|v| content_from_specified(v, style.box_sizing, pb_v))
                        .unwrap_or(ih),
                };
                // min/max clamp each axis (guarded; default svgs unchanged).
                let w = clamp_replaced(
                    w.max(0.0),
                    style.min_width,
                    style.max_width,
                    cbw,
                    style.box_sizing,
                    pb_h,
                );
                let h = clamp_replaced(
                    h.max(0.0),
                    drop_percent(style.min_height),
                    drop_percent(style.max_height),
                    cbw,
                    style.box_sizing,
                    pb_v,
                );

                let mut sub = child.clone();
                sub.dimensions = Dimensions::default();
                sub.dimensions.content.width = w.max(0.0);
                sub.dimensions.content.height = h.max(0.0);
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
            BoxKind::Media => {
                // <video>/<audio> (E15-M3): an atomic replaced-style box. The
                // intrinsic size is the poster's (if present), else a replaced
                // default (video 300×150, audio 300×54). CSS width/height + HTML
                // width/height attrs override, like <img>. Always produces a box
                // (the placeholder is shown — it never collapses).
                let id = child.style.node();
                let style = style_of(c.styled, child);
                let cbw = c.cb.content.width;
                let is_video = c.doc.tag_name(id) == Some("video");
                let default = if is_video {
                    (300.0, 150.0)
                } else {
                    (300.0, 54.0)
                };
                let intrinsic = child
                    .text
                    .as_deref()
                    .and_then(|poster| c.images.intrinsic_size(poster))
                    .or(Some(default));
                let (pb_h, pb_v) = replaced_pb(&style, cbw);
                let attr_w = resolve(style.width, cbw)
                    .map(|w| content_from_specified(w, style.box_sizing, pb_h))
                    .or_else(|| attr_px(c.doc, id, "width"));
                let attr_h = match style.height {
                    Length::Auto => attr_px(c.doc, id, "height"),
                    h => resolve(h, cbw).map(|v| content_from_specified(v, style.box_sizing, pb_v)),
                };
                let (w, h) = replaced_size(intrinsic, attr_w, attr_h);
                let w = clamp_replaced(
                    w,
                    style.min_width,
                    style.max_width,
                    cbw,
                    style.box_sizing,
                    pb_h,
                );
                let h = clamp_replaced(
                    h,
                    drop_percent(style.min_height),
                    drop_percent(style.max_height),
                    cbw,
                    style.box_sizing,
                    pb_v,
                );

                let mut sub = child.clone();
                sub.dimensions = Dimensions::default();
                sub.dimensions.content.width = w.max(0.0);
                sub.dimensions.content.height = h.max(0.0);
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
            BoxKind::Canvas => {
                // <canvas> (E20-M1): an atomic replaced-style box. The intrinsic
                // size is the width/height attrs, else the replaced default
                // 300×150. CSS width/height + HTML width/height attrs override,
                // like <img>/<video>. No child layout.
                let id = child.style.node();
                let style = style_of(c.styled, child);
                let cbw = c.cb.content.width;
                let intrinsic = Some((
                    attr_px(c.doc, id, "width").unwrap_or(300.0),
                    attr_px(c.doc, id, "height").unwrap_or(150.0),
                ));
                let (pb_h, pb_v) = replaced_pb(&style, cbw);
                let attr_w = resolve(style.width, cbw)
                    .map(|w| content_from_specified(w, style.box_sizing, pb_h))
                    .or_else(|| attr_px(c.doc, id, "width"));
                let attr_h = match style.height {
                    Length::Auto => attr_px(c.doc, id, "height"),
                    h => resolve(h, cbw).map(|v| content_from_specified(v, style.box_sizing, pb_v)),
                };
                let (w, h) = replaced_size(intrinsic, attr_w, attr_h);
                let w = clamp_replaced(
                    w,
                    style.min_width,
                    style.max_width,
                    cbw,
                    style.box_sizing,
                    pb_h,
                );
                let h = clamp_replaced(
                    h,
                    drop_percent(style.min_height),
                    drop_percent(style.max_height),
                    cbw,
                    style.box_sizing,
                    pb_v,
                );

                let mut sub = child.clone();
                sub.dimensions = Dimensions::default();
                sub.dimensions.content.width = w.max(0.0);
                sub.dimensions.content.height = h.max(0.0);
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
            BoxKind::FormControl => {
                // Native text form control: an atomic replaced-style box sized
                // from cols/rows/size attrs (or the button label), with CSS
                // width/height overriding (E14-M1). Unlike <img>/<svg> it carries
                // a UA border+padding, so those dims are filled here for a correct
                // margin-box advance + paint geometry.
                let id = child.style.node();
                let style = style_of(c.styled, child);
                let cbw = c.cb.content.width;
                let q = FontQuery {
                    family: &style.font_family,
                    style: style.font_style,
                    weight: style.font_weight,
                    size: style.font_size,
                    letter_spacing: style.letter_spacing,
                    word_spacing: style.word_spacing,
                };
                let ch = c.m.measure("0", &q).max(1.0);
                let line_h = style.font_size * 1.2;
                let (content_w, content_h) = match crate::form::form_control_kind(c.doc, id) {
                    Some(crate::form::FormControl::TextInput { .. }) => {
                        let cols = attr_count(c.doc, id, "size")
                            .filter(|&n| n >= 1)
                            .unwrap_or(20);
                        (cols as f32 * ch, line_h)
                    }
                    Some(crate::form::FormControl::TextArea) => {
                        let cols = attr_count(c.doc, id, "cols").unwrap_or(20);
                        let rows = attr_count(c.doc, id, "rows").unwrap_or(2);
                        (cols as f32 * ch, rows as f32 * line_h)
                    }
                    Some(crate::form::FormControl::Button) => {
                        let label = crate::form::control_label(c.doc, id);
                        (c.m.measure(&label, &q), line_h)
                    }
                    // E14-M2: checkbox/radio are a fixed 13×13 (size/value ignored;
                    // CSS width/height still overrides via the common path below).
                    Some(crate::form::FormControl::Checkbox { .. })
                    | Some(crate::form::FormControl::Radio { .. }) => (13.0, 13.0),
                    // E14-M2: select fits its selected option's text + a dropdown
                    // arrow (font-size wide) + a `ch` gap between them.
                    Some(crate::form::FormControl::Select) => {
                        let text = crate::form::selected_option_text(c.doc, id);
                        let text_w = c.m.measure(&text, &q);
                        let arrow_w = style.font_size;
                        (text_w + arrow_w + ch, line_h)
                    }
                    // E14-M3: color swatch / range slider (CSS width/height still
                    // overrides via the common path below).
                    Some(crate::form::FormControl::Color) => (style.font_size * 2.0, 13.0),
                    Some(crate::form::FormControl::Range) => (style.font_size * 12.0, 13.0),
                    None => (0.0, 0.0),
                };

                // CSS width/height (definite) override the intrinsic size like
                // <img>; box-sizing folds padding+border into a CSS dimension.
                let (pb_h, pb_v) = replaced_pb(&style, cbw);
                let w = resolve(style.width, cbw)
                    .map(|v| content_from_specified(v, style.box_sizing, pb_h))
                    .unwrap_or(content_w);
                let h = match style.height {
                    Length::Auto => content_h,
                    hh => resolve(hh, cbw)
                        .map(|v| content_from_specified(v, style.box_sizing, pb_v))
                        .unwrap_or(content_h),
                };
                let w = clamp_replaced(
                    w.max(0.0),
                    style.min_width,
                    style.max_width,
                    cbw,
                    style.box_sizing,
                    pb_h,
                );
                let h = clamp_replaced(
                    h.max(0.0),
                    drop_percent(style.min_height),
                    drop_percent(style.max_height),
                    cbw,
                    style.box_sizing,
                    pb_v,
                );

                let mut sub = child.clone();
                sub.dimensions = Dimensions::default();
                sub.dimensions.content.width = w.max(0.0);
                sub.dimensions.content.height = h.max(0.0);
                // Form controls have a UA border + padding: fill those dims so the
                // margin-box advance + paint geometry are correct.
                sub.dimensions.border.left = style.border_left_width;
                sub.dimensions.border.right = style.border_right_width;
                sub.dimensions.border.top = style.border_top_width;
                sub.dimensions.border.bottom = style.border_bottom_width;
                sub.dimensions.padding.left = resolve_or_zero(style.padding_left, cbw);
                sub.dimensions.padding.right = resolve_or_zero(style.padding_right, cbw);
                sub.dimensions.padding.top = resolve_or_zero(style.padding_top, cbw);
                sub.dimensions.padding.bottom = resolve_or_zero(style.padding_bottom, cbw);
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

/// Re-flatten a stale `LineBox` (produced by a previous inline pass over this
/// box) back into the item stream so that re-running inline layout is
/// idempotent. Each fragment becomes a fresh `CollectedItem`; the inter-word
/// space that the prior pass encoded only as the x-gap between two fragments is
/// recovered as a `spaces_before` of 1 whenever a positive gap is present.
///
/// `pending_space` from a preceding sibling carries onto the first fragment.
/// Within the line, the gap is measured against the previous fragment's right
/// edge (after the first fragment, spacing is intrinsic to the gap, so the
/// running `pending_space` is reset and recomputed per fragment).
fn collect_stale_line(c: &mut Collector, line: &LayoutBox) {
    let mut prev_right: Option<f32> = None;
    for frag in &line.children {
        // Leading gap: from a preceding sibling (pending_space) for the first
        // fragment, else from the positional gap to the previous fragment.
        let has_gap = match prev_right {
            None => c.pending_space,
            Some(r) => frag.dimensions.content.x - r > 0.01,
        };
        c.pending_space = has_gap;
        match frag.kind {
            BoxKind::TextRun => {
                let style = style_of(c.styled, frag);
                let text = frag.text.clone().unwrap_or_default();
                let ws = style.white_space;
                let segments: Vec<&str> = text.split('\n').collect();
                for (i, seg) in segments.iter().enumerate() {
                    if i > 0 {
                        c.out.push(CollectedItem::ForcedBreak);
                        c.pending_space = false;
                    }
                    collect_segment(c, frag, &style, ws, seg);
                }
            }
            // Atomics / markers nested in a stale line re-collect via the normal
            // path (a one-element borrow keeps the existing handling).
            _ => collect_items(c, &one_child(frag)),
        }
        let mb = frag.dimensions.margin_box();
        prev_right = Some(mb.x + mb.width);
    }
}

/// Wrap a single box so it can be fed back through `collect_items` (which
/// iterates `b.children`). Avoids duplicating the atomic/inline-block handling.
fn one_child(child: &LayoutBox) -> LayoutBox {
    let mut holder = LayoutBox::new(BoxKind::InlineBox, child.style.clone());
    holder.children.push(child.clone());
    holder
}

/// Collect the words of one text segment (no internal `\n`) under `ws`.
/// Collapsing modes (`normal`/`nowrap`/`pre-line`) split on `' '` and skip
/// empties (a collapsed space ⇒ `spaces_before = 1`). Preserving modes
/// (`pre`/`pre-wrap`) keep maximal space runs as the `spaces_before` count so
/// each space advances (E6-M3 §2.3).
fn collect_segment(
    c: &mut Collector,
    child: &LayoutBox,
    style: &starfish_style::ComputedStyle,
    ws: WhiteSpace,
    seg: &str,
) {
    let mk_word = |c: &mut Collector, text: String, spaces: u16| {
        c.out.push(CollectedItem::Word {
            text,
            style_ref: child.style.clone(),
            font_size: style.font_size,
            line_height: style.line_height,
            spaces_before: spaces,
            letter_spacing: style.letter_spacing,
            word_spacing: style.word_spacing,
        });
    };

    if ws.collapses() {
        // Collapsing: a leading space sets a pending collapsed space.
        if seg.starts_with(' ') {
            c.pending_space = true;
        }
        for word in seg.split(' ') {
            if word.is_empty() {
                continue;
            }
            let spaces = if c.pending_space { 1 } else { 0 };
            mk_word(c, word.to_string(), spaces);
            c.pending_space = true;
        }
        c.pending_space = seg.ends_with(' ');
    } else {
        // Preserving (pre / pre-wrap): keep every space; count leading spaces as
        // the gap before the next word; a trailing all-space segment produces a
        // word with empty text carrying the spaces (keeps the run visible).
        let pending = if c.pending_space { 1u16 } else { 0 };
        c.pending_space = false;
        let mut spaces: u16 = pending;
        let mut buf = String::new();
        let mut emitted = false;
        for ch in seg.chars() {
            if ch == ' ' {
                if !buf.is_empty() {
                    mk_word(c, std::mem::take(&mut buf), spaces);
                    emitted = true;
                    spaces = 0;
                }
                spaces = spaces.saturating_add(1);
            } else {
                buf.push(ch);
            }
        }
        if !buf.is_empty() {
            mk_word(c, buf, spaces);
        } else if spaces > 0 && !emitted {
            // segment of only spaces (e.g. leading spaces before a \n): keep
            // them as an empty-text word so their advance shows.
            mk_word(c, String::new(), spaces);
        } else if spaces > 0 {
            // trailing spaces after the last word: attach to a final empty word.
            mk_word(c, String::new(), spaces);
        }
    }
}

/// The `<svg>` intrinsic content size (E9-M1 §3.2): `width`/`height` attrs
/// (px/unitless) win; a missing dimension is derived from the `viewBox` aspect
/// ratio; with neither, the CSS replaced-element default 300×150 applies.
fn svg_intrinsic_size(doc: &Document, id: NodeId) -> (f32, f32) {
    let w = attr_px(doc, id, "width");
    let h = attr_px(doc, id, "height");
    let vb = crate::boxtree::parse_view_box(doc.get_attribute(id, "viewBox"));
    match (w, h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, vb.map(|v| w * v.h / v.w).unwrap_or(150.0)),
        (None, Some(h)) => (vb.map(|v| h * v.w / v.h).unwrap_or(300.0), h),
        (None, None) => vb.map(|v| (v.w, v.h)).unwrap_or((300.0, 150.0)),
    }
}

/// Parse a positive-integer HTML attribute (`size`/`cols`/`rows`, E14-M1).
/// `None` if absent / invalid / non-positive.
fn attr_count(doc: &Document, id: NodeId, name: &str) -> Option<u32> {
    doc.get_attribute(id, name)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1)
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

/// Horizontal + vertical padding+border of a replaced element, used as the `pb`
/// for box-sizing / min-max clamping on each axis (E13-M1).
fn replaced_pb(s: &starfish_style::ComputedStyle, cbw: f32) -> (f32, f32) {
    let h = s.border_left_width
        + s.border_right_width
        + resolve_or_zero(s.padding_left, cbw)
        + resolve_or_zero(s.padding_right, cbw);
    let v = s.border_top_width
        + s.border_bottom_width
        + resolve_or_zero(s.padding_top, cbw)
        + resolve_or_zero(s.padding_bottom, cbw);
    (h, v)
}

/// Clamp a replaced element's content size on one axis, guarded so the default
/// (min=Auto / max=Auto) path is the identity (E13-M1).
fn clamp_replaced(
    v: f32,
    min: Length,
    max: Length,
    cbw: f32,
    bs: starfish_style::BoxSizing,
    pb: f32,
) -> f32 {
    if matches!(min, Length::Auto) && matches!(max, Length::Auto) {
        return v;
    }
    clamp_size(v, min, max, cbw, bs, pb)
}

/// A percentage `min/max-height` resolves against the containing block's HEIGHT,
/// which is indefinite for a replaced box here — so (matching the block path's
/// percent-height handling) treat it as `Auto` (ignored). Percent `min/max-width`
/// keeps resolving against `cbw`, so this is only applied on the height axis.
fn drop_percent(l: Length) -> Length {
    if matches!(l, Length::Percent(_)) {
        Length::Auto
    } else {
        l
    }
}

/// Reorder one line's items into visual left-to-right order per the bidi
/// algorithm (E6-M3 §5). Returns the items with recomputed visual `x`.
///
/// Pragmatic subset: only `PlacedItem::Word`s reorder; if the line contains any
/// atomic inline it keeps its logical order (atoms are neutral, §7) but an RTL
/// base still right-aligns via text-align. Pure-LTR lines (LTR base, no strong
/// RTL char) are returned unchanged (fast path → byte-identical to pre-M3).
fn reorder_line(
    line: Vec<PlacedItem>,
    l_start: f32,
    dir: Direction,
    bidi_override: bool,
) -> Vec<PlacedItem> {
    // Atomics present → don't attempt word reordering (subset). LTR base keeps
    // logical order; RTL base relies on text-align for the right edge.
    if line.iter().any(|i| matches!(i, PlacedItem::Atomic { .. })) {
        return line;
    }
    // Build the logical string + per-word char ranges (words joined by a single
    // separator space, matching the inter-word gap model).
    let mut logical = String::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new(); // (char_start, char_len) per word
    let mut char_pos = 0usize;
    for (i, item) in line.iter().enumerate() {
        if let PlacedItem::Word { text, .. } = item {
            if i > 0 {
                logical.push(' ');
                char_pos += 1;
            }
            let len = text.chars().count();
            ranges.push((char_pos, len));
            logical.push_str(text);
            char_pos += len;
        }
    }

    let base = if dir == Direction::Rtl {
        Level::rtl()
    } else {
        Level::ltr()
    };

    // bidi-override: force the whole line to the base direction (§5.3). For an
    // RTL base that means reverse the whole visual order; LTR base = unchanged.
    if bidi_override {
        if dir == Direction::Rtl {
            return relay_visual(line, l_start, true);
        }
        return line;
    }

    let info = BidiInfo::new(&logical, Some(base));
    if !info.has_rtl() && dir == Direction::Ltr {
        // Pure-LTR fast path: nothing reorders.
        return line;
    }
    let para = &info.paragraphs[0];
    let (levels, runs) = info.visual_runs(para, para.range.clone());

    // Map each visual run (byte range) to the words it covers, in visual order.
    // Build a byte→char index for the logical string.
    let char_starts: Vec<usize> = {
        let mut v = Vec::with_capacity(logical.len() + 1);
        for (ci, (bi, _)) in logical.char_indices().enumerate() {
            while v.len() <= bi {
                v.push(ci);
            }
        }
        while v.len() <= logical.len() {
            v.push(logical.chars().count());
        }
        v
    };

    // Reconstruct: walk visual runs L→R, gather covered words (reverse word order
    // within an RTL run), append to the visual order. The word TEXT stays in
    // LOGICAL order — shaping (measure + paint) infers RTL from the chars and
    // emits glyphs in visual order itself (E10-M2), so no char pre-reversal here.
    let words: Vec<PlacedItem> = line;
    // index words by their logical position for O(1) take.
    let mut visual_indices: Vec<usize> = Vec::with_capacity(words.len());
    for run in &runs {
        let run_rtl = levels[run.start].is_rtl();
        let run_cs = char_starts[run.start];
        let run_ce = char_starts[run.end];
        // words whose char range lies within [run_cs, run_ce).
        let mut in_run: Vec<usize> = Vec::new();
        for (wi, &(cs, len)) in ranges.iter().enumerate() {
            if cs >= run_cs && cs + len <= run_ce {
                in_run.push(wi);
            }
        }
        if run_rtl {
            in_run.reverse();
        }
        visual_indices.extend(in_run);
    }
    // Any words not captured by a run (shouldn't happen) keep logical order.
    if visual_indices.len() != ranges.len() {
        return words;
    }

    // Re-place the words at their visual order positions, advancing L→R from the
    // line start. The first visual word gets no leading space.
    relay_in_order(words, &visual_indices, l_start)
}

/// Lay the given words out L→R in `order` (indices into `words`), starting at
/// `l_start`, advancing by each word's width plus the inter-word gap. The very
/// first visual word drops its leading space (line edge).
///
/// The inter-word gap belongs to the LOGICAL boundary between two words, not to
/// whichever word lands in a given visual slot. `spaces_before`/`space_w` are
/// recorded against the word that logically *follows* the space. So for two
/// visually-adjacent words the gap between them is taken from the logically
/// *later* of the pair (the one that owns that boundary's space). For an LTR run
/// this is just each word's own `spaces_before` (unchanged); for a reversed RTL
/// run it keeps the space on the correct boundary instead of dropping it.
fn relay_in_order(words: Vec<PlacedItem>, order: &[usize], l_start: f32) -> Vec<PlacedItem> {
    // Per logical-word space metadata, captured before draining the slots so the
    // gap for a boundary can be read from the logically-later neighbour.
    let space_info: Vec<(u16, f32)> = words
        .iter()
        .map(|it| match it {
            PlacedItem::Word {
                spaces_before,
                space_w,
                ..
            } => (*spaces_before, *space_w),
            _ => (0, 0.0),
        })
        .collect();
    let mut slots: Vec<Option<PlacedItem>> = words.into_iter().map(Some).collect();
    let mut out: Vec<PlacedItem> = Vec::with_capacity(order.len());
    let mut cursor = l_start;
    for (pos, &wi) in order.iter().enumerate() {
        let Some(item) = slots[wi].take() else {
            continue;
        };
        if let PlacedItem::Word {
            text,
            style_ref,
            line_height,
            width,
            spaces_before,
            space_w,
            ..
        } = item
        {
            let gap = if pos == 0 {
                0.0
            } else {
                // Gap between this word and the previous visual word lives on the
                // boundary owned by the logically-later of the two.
                let prev = order[pos - 1];
                let (sb, sw) = space_info[wi.max(prev)];
                sw * sb as f32
            };
            let x = cursor + gap;
            cursor = x + width;
            out.push(PlacedItem::Word {
                text,
                style_ref,
                line_height,
                x,
                width,
                spaces_before,
                space_w,
            });
        }
    }
    out
}

/// bidi-override relayout: optionally reverse the run/word order, then lay out
/// L→R (used for the whole-line forced override, §5.3). Word TEXT stays LOGICAL;
/// shaping infers direction and emits glyphs in visual order (E10-M2).
fn relay_visual(line: Vec<PlacedItem>, l_start: f32, reverse_order: bool) -> Vec<PlacedItem> {
    let order: Vec<usize> = if reverse_order {
        (0..line.len()).rev().collect()
    } else {
        (0..line.len()).collect()
    };
    relay_in_order(line, &order, l_start)
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_inline(
    b: &mut LayoutBox,
    doc: &Document,
    styled: &StyledTree,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    floats: &FloatContext,
    cache: &LayoutCache,
) -> f32 {
    let container_style = style_of(styled, b);
    let origin = b.dimensions.content;
    // E18-M3 vertical writing modes: the inline axis runs along Y, the block axis
    // (line stacking) along X. The wrap measure is the container's inline extent:
    // height in vertical, width in horizontal. A 0-height vertical container can't
    // wrap → a single overflowing column (documented MVP limit).
    let vertical = container_style.writing_mode.is_vertical();
    let avail = if vertical {
        origin.height
    } else {
        origin.width
    };
    let dir = container_style.direction;
    let bidi_override = container_style.unicode_bidi == UnicodeBidi::BidiOverride;
    // RTL base remaps the default (initial `Left`) text-align to the right start
    // edge (§5.5). Explicit center/right/justify are honored verbatim. In vertical
    // mode, text-align MVP is treated as start (slack distribution skipped below).
    let align = match (container_style.text_align, dir) {
        (TextAlign::Left, Direction::Rtl) => TextAlign::Right,
        (other, _) => other,
    };

    let mut collector = Collector {
        doc,
        styled,
        m,
        images,
        cache,
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

    // Band for the current (in-progress) line. In vertical mode floats are
    // deferred, so the band is the full inline extent with no insets.
    let band = |y: f32| -> (f32, f32) {
        if vertical {
            return (0.0, avail);
        }
        let abs_y = origin.y + y;
        let left_inset = floats.left_offset(abs_y, band_h, origin.x);
        let right_inset = floats.right_offset(abs_y, band_h, origin.x + avail);
        let line_avail = (avail - left_inset - right_inset).max(0.0);
        (left_inset, line_avail)
    };
    let (mut line_start, mut line_avail) = band(line_y);

    // Soft-wrap is allowed only when the container's white-space wraps (§2.3).
    let wraps = container_style.white_space.wraps();
    // A per-space advance (incl. word-spacing) carried onto PlacedItem::Word.
    let mut commit_line = |cur: &mut Vec<PlacedItem>,
                           line_start: &mut f32,
                           line_avail: &mut f32,
                           line_y: &mut f32| {
        let line_height = cur.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);
        // an empty forced-break line still advances by the container LH.
        let lh = if cur.is_empty() { band_h } else { line_height };
        lines.push((std::mem::take(cur), *line_start, *line_avail, *line_y));
        *line_y += lh;
        let (s, a) = band(*line_y);
        *line_start = s;
        *line_avail = a;
    };

    for item in items {
        // ForcedBreak: commit the current line unconditionally (even if empty).
        if matches!(item, CollectedItem::ForcedBreak) {
            commit_line(&mut cur, &mut line_start, &mut line_avail, &mut line_y);
            cursor_x = line_start;
            continue;
        }

        let (width, line_h, space_w, sp_before, per_space): (f32, f32, f32, u16, f32) = match &item
        {
            CollectedItem::Word {
                text: word,
                style_ref,
                font_size,
                line_height: lh,
                spaces_before,
                letter_spacing,
                word_spacing,
            } => {
                let style = match style_ref {
                    BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
                    BoxStyleRef::Generated { origin, side } => styled.pseudo_style(*origin, *side),
                };
                let style = style.unwrap_or(&container_style);
                let q = FontQuery {
                    family: &style.font_family,
                    style: style.font_style,
                    weight: style.font_weight,
                    size: *font_size,
                    letter_spacing: *letter_spacing,
                    word_spacing: *word_spacing,
                };
                let w = m.measure(word, &q);
                let per_space = if *spaces_before > 0 {
                    m.measure(" ", &q)
                } else {
                    0.0
                };
                let space_w = per_space * (*spaces_before as f32);
                let line_h = used_line_height(*font_size, *lh);
                (w, line_h, space_w, *spaces_before, per_space)
            }
            CollectedItem::Atomic {
                width,
                height,
                space_before,
                ..
            } => {
                let space_w = if *space_before {
                    0.5 * container_style.font_size
                } else {
                    0.0
                };
                (
                    *width,
                    *height,
                    space_w,
                    *space_before as u16,
                    0.5 * container_style.font_size,
                )
            }
            CollectedItem::ForcedBreak => unreachable!(),
        };

        // Decide placement x (relative to origin.x).
        let x = if cur.is_empty() {
            line_start + space_w
        } else if !wraps || cursor_x + space_w + width <= line_start + line_avail {
            cursor_x + space_w
        } else {
            // Wrap: commit the current line, advance y, recompute the band.
            commit_line(&mut cur, &mut line_start, &mut line_avail, &mut line_y);
            line_start
        };
        cursor_x = x + width;

        match item {
            CollectedItem::Word {
                text: word,
                style_ref,
                ..
            } => {
                cur.push(PlacedItem::Word {
                    text: word,
                    style_ref,
                    line_height: line_h,
                    x,
                    width,
                    spaces_before: sp_before,
                    space_w: per_space,
                });
            }
            CollectedItem::Atomic { atom, .. } => {
                cur.push(PlacedItem::Atomic {
                    atom,
                    line_height: line_h,
                    x,
                    width,
                });
            }
            CollectedItem::ForcedBreak => unreachable!(),
        }
    }
    if !cur.is_empty() {
        let line_height = cur.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);
        lines.push((cur, line_start, line_avail, line_y));
        line_y += line_height;
    }

    // Build LineBox children with absolute geometry.
    let mut line_boxes: Vec<LayoutBox> = Vec::new();

    // vertical-rl mirror reference: the right edge of the block-axis extent. Use
    // the larger of the container's content width and the total line stack so it
    // works for both definite-width and auto-width vertical containers.
    let vrl = vertical && container_style.writing_mode == WritingMode::VerticalRl;
    let block_right = origin.x + origin.width.max(line_y);

    for (line, l_start, l_avail, l_y) in lines {
        // bidi: reorder this line's words into visual L→R order when it has any
        // strong-RTL content or an RTL base/override (§5). Pure-LTR lines are a
        // no-op (positions unchanged). Disabled in vertical mode (no-op).
        let line = if vertical {
            line
        } else {
            reorder_line(line, l_start, dir, bidi_override)
        };
        let line_height = line.iter().map(|w| w.line_height()).fold(0.0f32, f32::max);

        // text-align offset uses the line's used width relative to the line band.
        // Vertical mode treats align as start (skip slack distribution, MVP §3).
        let offset = if vertical {
            0.0
        } else {
            let used_width = line
                .iter()
                .map(|w| w.right_edge() - l_start)
                .fold(0.0f32, f32::max);
            let slack = (l_avail - used_width).max(0.0);
            match align {
                TextAlign::Right => slack,
                TextAlign::Center => slack / 2.0,
                TextAlign::Left | TextAlign::Justify => 0.0,
            }
        };

        // Map a logical (inline-pos `ip`, inline-advance `adv`, cross-size `cross`)
        // tuple to a physical Rect, given the line's cross offset `l_y`.
        // horizontal: (x = origin.x+offset+ip, y = origin.y+l_y, w = adv, h = cross)
        // vertical:   the inline axis is Y, the cross (line) axis is X.
        let phys = |ip: f32, adv: f32, cross: f32| -> Rect {
            if vertical {
                // Block-axis (X) position of this line's left edge.
                let lx = if vrl {
                    block_right - (l_y + cross)
                } else {
                    origin.x + l_y
                };
                Rect {
                    x: lx,
                    y: origin.y + offset + ip,
                    width: cross,
                    height: adv,
                }
            } else {
                Rect {
                    x: origin.x + offset + ip,
                    y: origin.y + l_y,
                    width: adv,
                    height: cross,
                }
            }
        };

        let mut lb = LayoutBox::new(BoxKind::LineBox, b.style.clone());
        lb.dimensions.content = if vertical {
            let lx = if vrl {
                block_right - (l_y + line_height)
            } else {
                origin.x + l_y
            };
            Rect {
                x: lx,
                y: origin.y,
                width: line_height,
                height: l_avail,
            }
        } else {
            Rect {
                x: origin.x + l_start,
                y: origin.y + l_y,
                width: l_avail,
                height: line_height,
            }
        };

        for item in line {
            match item {
                PlacedItem::Word {
                    text,
                    style_ref,
                    line_height: lh,
                    x,
                    width,
                    ..
                } => {
                    let mut frag = LayoutBox::new(BoxKind::TextRun, style_ref);
                    frag.text = Some(text);
                    frag.dimensions.content = phys(x, width, lh);
                    lb.children.push(frag);
                }
                PlacedItem::Atomic {
                    atom,
                    x,
                    width,
                    line_height: lh,
                } => {
                    // Translate the pre-laid-out sub-box so its margin-box's
                    // top-left sits at the committed line position. The sub-box
                    // was laid out at the container's content origin (cb), so its
                    // current margin-box top-left is the delta source.
                    let mut sub = std::mem::replace(
                        &mut atomics[atom],
                        LayoutBox::new(BoxKind::InlineBlock, b.style.clone()),
                    );
                    let cur_mb = sub.dimensions.margin_box();
                    let target = phys(x, width, lh);
                    translate_box(&mut sub, target.x - cur_mb.x, target.y - cur_mb.y);
                    lb.children.push(sub);
                }
            }
        }

        line_boxes.push(lb);
    }

    // Hang the marker into the left gutter of the first line (§3.2). Vertical mode
    // keeps this horizontal-only (skipped — documented MVP limit).
    if let (false, Some(pm)) = (vertical, marker) {
        if let Some(first) = line_boxes.first_mut() {
            let style = match &pm.style_ref {
                BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
                BoxStyleRef::Generated { origin, side } => styled.pseudo_style(*origin, *side),
            };
            let style = style.unwrap_or(&container_style);
            let font_size = style.font_size;
            let q = FontQuery {
                family: &style.font_family,
                style: style.font_style,
                weight: style.font_weight,
                size: font_size,
                letter_spacing: style.letter_spacing,
                word_spacing: style.word_spacing,
            };
            let mw = m.measure(&pm.text, &q);
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

    // text-overflow: ellipsis (E16-M4). Gated post-pass: only a non-wrapping,
    // overflow-clipped container with `ellipsis` truncates. Single-line (nowrap)
    // so only the first line box is processed. Complete no-op otherwise → all
    // existing pages keep byte-identical fragments.
    if !vertical
        && container_style.text_overflow == TextOverflow::Ellipsis
        && !container_style.white_space.wraps()
        && matches!(container_style.overflow, Overflow::Hidden | Overflow::Clip)
    {
        if let Some(first) = line_boxes.first_mut() {
            let q = FontQuery {
                family: &container_style.font_family,
                style: container_style.font_style,
                weight: container_style.font_weight,
                size: container_style.font_size,
                letter_spacing: container_style.letter_spacing,
                word_spacing: container_style.word_spacing,
            };
            let ell_w = m.measure("\u{2026}", &q);
            let limit = origin.x + avail;
            // Rightmost TextRun fragment's right edge.
            let max_right = first
                .children
                .iter()
                .filter(|c| c.kind == BoxKind::TextRun)
                .map(|c| c.dimensions.content.x + c.dimensions.content.width)
                .fold(f32::NEG_INFINITY, f32::max);
            if max_right > limit + 0.01 {
                let limit_x = limit - ell_w;
                apply_ellipsis(&mut first.children, limit_x, &q, m);
            }
        }
    }

    b.children = line_boxes;
    line_y
}

/// Truncate a single line's TextRun fragments so the line ends with `…` at or
/// before `limit_x` (E16-M4). Fragments fully past `limit_x` are dropped; the
/// fragment straddling it is cut char-by-char to the longest prefix that fits
/// `limit_x - frag.x`, then `…` is appended. Atomic inline boxes past the limit
/// are dropped whole (never split). Word/text fragments only.
fn apply_ellipsis(line: &mut Vec<LayoutBox>, limit_x: f32, q: &FontQuery, m: &dyn TextMeasurer) {
    let mut truncated = false;
    line.retain_mut(|frag| {
        if truncated {
            // Everything after the cut fragment is dropped (markers sit in the
            // left gutter at x < origin.x, so they never reach here).
            return false;
        }
        let fx = frag.dimensions.content.x;
        let fright = fx + frag.dimensions.content.width;
        if fright <= limit_x {
            // Entirely before the limit — kept as-is.
            return true;
        }
        // This fragment crosses limit_x: truncate text fragments, drop atomics.
        truncated = true;
        let Some(text) = frag.text.as_ref() else {
            // Atomic inline box crossing the limit → dropped whole.
            return false;
        };
        let budget = (limit_x - fx).max(0.0);
        let mut prefix = String::new();
        for ch in text.chars() {
            let mut cand = prefix.clone();
            cand.push(ch);
            if m.measure(&cand, q) > budget {
                break;
            }
            prefix = cand;
        }
        prefix.push('\u{2026}');
        frag.dimensions.content.width = m.measure(&prefix, q);
        frag.text = Some(prefix);
        true
    });
}
