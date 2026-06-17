//! Block layout (§3): width, position, children, height. No margin collapsing.
//! Extended for M2 with float placement, `clear`, and `position:relative`.

use starfish_dom::Document;
use starfish_style::{
    BoxSizing, Clear, ComputedStyle, ContentVisibility, Direction, Display, Length, Overflow,
    Position, ScrollbarGutter, WritingMode,
};

use crate::boxtree::{is_normal_flow, is_out_of_flow, style_of, BoxKind, LayoutBox};
use crate::cache::LayoutCache;
use crate::dimensions::{Dimensions, Rect};
use crate::float::{ClearSides, FloatContext, FloatSide};
use crate::inline::{layout_inline, translate_box};
use crate::measure::{ImageSource, TextMeasurer};
use starfish_style::{Float, StyledTree};

/// Resolve a `Length` against the containing-block width. `Auto` → `None`.
pub(crate) fn resolve(len: &Length, cb_width: f32) -> Option<f32> {
    match len {
        Length::Px(v) => Some(*v),
        Length::Percent(p) => Some(p / 100.0 * cb_width),
        Length::Auto => None,
        // calc() linear form (E13-M2): px + percent% of the containing block.
        Length::Calc { px, percent } => Some(px + percent / 100.0 * cb_width),
        // Math-function tree (E24-M1): resolved against the same basis.
        Length::Math(m) => Some(m.resolve(cb_width)),
    }
}

/// Resolve a `Length` used as a definite value (`Auto` → 0).
pub(crate) fn resolve_or_zero(len: &Length, cb_width: f32) -> f32 {
    resolve(len, cb_width).unwrap_or(0.0)
}

/// box-sizing-aware: convert a specified (border-box-if-border-box) size to a
/// CONTENT size. For `content-box` this is the identity (E13-M1).
pub(crate) fn content_from_specified(spec: f32, bs: BoxSizing, pb: f32) -> f32 {
    match bs {
        BoxSizing::ContentBox => spec,
        BoxSizing::BorderBox => (spec - pb).max(0.0),
    }
}

/// Clamp a CONTENT size to `[min, max]`. `min=Auto` → 0 (no lower bound);
/// `max=Auto` → no upper bound (+∞). `min`/`max` resolve against `cb_basis` like
/// width; both are box-sizing values so they are converted to content first.
/// (E13-M1)
pub(crate) fn clamp_size(
    content: f32,
    min: &Length,
    max: &Length,
    cb_basis: f32,
    bs: BoxSizing,
    pb: f32,
) -> f32 {
    let mut v = content;
    if let Some(mx) = resolve(max, cb_basis) {
        // max=Auto → None → no upper bound.
        v = v.min(content_from_specified(mx, bs, pb));
    }
    let min_c = match resolve(min, cb_basis) {
        // min=Auto → None → 0.
        Some(mn) => content_from_specified(mn, bs, pb),
        None => 0.0,
    };
    // CSS: if min > max, min wins (max applied first, then min via `.max`).
    v.max(min_c)
}

/// Lay out a block-level box (`BlockContainer` / `AnonymousBlock`) within its
/// containing block's content geometry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_block(
    b: &mut LayoutBox,
    containing: Dimensions,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    floats: &mut FloatContext,
    cache: &LayoutCache,
) {
    let style = style_of(styled, b);
    calculate_block_width(b, &style, containing);
    calculate_block_position(b, &style, containing);
    // E60-M1: a scroll container with `scrollbar-gutter: stable [both-edges]`
    // reserves a scrollbar-width gutter on the inline-end edge (both edges for
    // `both-edges`) even when not overflowing — narrowing the content box that
    // children lay out in. `auto` (default) / non-scroll-container boxes are
    // untouched, so unaffected pages stay byte-identical.
    reserve_scrollbar_gutter(b, &style);
    // E31-M1: `content-visibility: hidden` skips the subtree's layout + paint
    // (its children are dropped); its used size collapses to the explicit size
    // (or 0). Same size collapse for `contain: size`.
    let hidden = style.content_visibility == ContentVisibility::Hidden;
    if hidden {
        b.children.clear();
    }
    if hidden {
        // No child/height phase; size handled below.
    } else if matches!(style.display, Display::Flex | Display::InlineFlex) {
        // Flex container: the flex algorithm replaces the children+height phase.
        let h = crate::flex::layout_flex(b, containing, &style, styled, doc, m, images, cache);
        b.dimensions.content.height = h;
    } else if matches!(style.display, Display::Grid | Display::InlineGrid) {
        // Grid container: the grid algorithm replaces the children+height phase.
        let h = crate::grid::layout_grid(b, containing, &style, styled, doc, m, images, cache);
        b.dimensions.content.height = h;
    } else if matches!(style.display, Display::Table | Display::InlineTable) {
        // Table container: the table algorithm replaces the children+height phase.
        let h = crate::table::layout_table(b, containing, &style, styled, doc, m, images, cache);
        b.dimensions.content.height = h;
    } else if (style.column_count.is_some() || style.column_width.is_some())
        && b.children
            .iter()
            .any(|c| !c.is_inline_level() && c.kind != BoxKind::LineBox)
    {
        // Multi-column container with block children (E18-M2): distribute the
        // block children across N columns. A pure-inline multicol container has
        // no block child here and falls through to the normal children path,
        // which lays it out as a single column (documented MVP limit).
        let h =
            crate::multicol::layout_multicol(b, containing, &style, styled, doc, m, images, cache);
        b.dimensions.content.height = h;
    } else {
        layout_block_children(b, &style, containing, styled, doc, m, images, floats, cache);
    }
    calculate_block_height(b, &style, containing.content.width);
    // Size containment / hidden: the used height doesn't depend on contents.
    if hidden || style.contain_size {
        b.dimensions.content.height = resolve(&style.height, containing.content.height).unwrap_or(0.0);
    }

    // position:relative — reserve space in flow (already done), then translate
    // the whole subtree by the resolved offset (§4.1). position:sticky does NOT
    // match here: a one-shot render has scroll offset 0, so a sticky box is never
    // scrolled past and stays at its in-flow (static) position with insets ignored
    // (E16-M4).
    if style.position == Position::Relative {
        let cbw = containing.content.width;
        let cbh = containing.content.height;
        let dx = rel_offset(&style.left, &style.right, cbw);
        let dy = rel_offset(&style.top, &style.bottom, cbh);
        if dx != 0.0 || dy != 0.0 {
            translate_box(b, dx, dy);
        }
    }
}

/// E60-M1: scrollbar-width gutter reserved by `scrollbar-gutter`. Matches the
/// painter's overlay-scrollbar `SCROLLBAR_WIDTH` (crates/paint/src/display.rs).
const SCROLLBAR_GUTTER_WIDTH: f32 = 12.0; // E60-M1

/// E60-M1: shrink a scroll container's content box for a stable `scrollbar-gutter`.
/// A scroll container is `overflow: scroll | auto | hidden` (matching the painter's
/// notion). `stable` reserves one gutter on the inline-end edge; `both-edges`
/// reserves on both, offsetting the content start by one gutter for the inline-start
/// edge. The inline-start/end map to left/right by `direction` (LTR: end = right).
fn reserve_scrollbar_gutter(b: &mut LayoutBox, style: &ComputedStyle) {
    if style.scrollbar_gutter == ScrollbarGutter::Auto {
        return;
    }
    if !matches!(
        style.overflow,
        Overflow::Scroll | Overflow::Auto | Overflow::Hidden
    ) {
        return;
    }
    let both = style.scrollbar_gutter == ScrollbarGutter::StableBothEdges;
    let total = if both {
        SCROLLBAR_GUTTER_WIDTH * 2.0
    } else {
        SCROLLBAR_GUTTER_WIDTH
    };
    b.dimensions.content.width = (b.dimensions.content.width - total).max(0.0);
    // The inline-start edge gutter shifts the content origin inward: left edge in
    // LTR, right edge in RTL (no x shift, just the width loss).
    if both && style.direction == Direction::Ltr {
        b.dimensions.content.x += SCROLLBAR_GUTTER_WIDTH;
    } else if !both && style.direction == Direction::Rtl {
        // Single gutter on the inline-start (left, RTL) edge shifts the origin.
        b.dimensions.content.x += SCROLLBAR_GUTTER_WIDTH;
    }
}

/// CSS relative offset: `left`/`top` wins if set, else `-right`/`-bottom`, else 0.
fn rel_offset(start: &Length, end: &Length, basis: f32) -> f32 {
    match (resolve(start, basis), resolve(end, basis)) {
        (Some(s), _) => s,
        (None, Some(e)) => -e,
        (None, None) => 0.0,
    }
}

/// Run block layout on an inline-block sub-box to get its used size (§3.1 Step
/// A). Its content origin is fixed up later when it's committed to a line.
///
/// `layout_block`'s width algorithm absorbs the containing block's underflow
/// into the right (or auto) margins to fill the line — correct for a block, but
/// for an *atomic inline* we want a tight margin-box. So we re-resolve the
/// element's specified horizontal margins (auto → 0) and overwrite the computed
/// ones, collapsing the spurious fill.
pub(crate) fn layout_inline_block(
    b: &mut LayoutBox,
    cb: Dimensions,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) {
    // An inline-block establishes its own BFC: fresh float context (§3.2).
    let mut local_floats = FloatContext::default();
    layout_block(b, cb, styled, doc, m, images, &mut local_floats, cache);
    let style = style_of(styled, b);
    let cbw = cb.content.width;
    b.dimensions.margin.left = resolve_or_zero(&style.margin_left, cbw);
    b.dimensions.margin.right = resolve_or_zero(&style.margin_right, cbw);
    // E56-M1: a `<ruby>` inline-block must SHRINK-TO-FIT around its widest row
    // (the base content row or the smaller `<rt>` annotation row). The auto-width
    // inline-block above fills the line, so its two block rows are full-width and
    // their `text-align:center` floats the annotation to the line's middle. Re-pin
    // the box width to the measured max-content span of its rows, then re-lay it
    // out at that tight width so each row re-centers over the column.
    if doc.tag_name(b.style.node()) == Some("ruby") && matches!(style.width, Length::Auto) {
        if let Some(span) = ruby_shrink_width(b) {
            if span > 0.0 && span < b.dimensions.content.width {
                let tight = Dimensions {
                    content: Rect {
                        width: span,
                        ..cb.content
                    },
                    ..cb
                };
                let mut tight_floats = FloatContext::default();
                layout_block(b, tight, styled, doc, m, images, &mut tight_floats, cache);
                b.dimensions.margin.left = resolve_or_zero(&style.margin_left, cbw);
                b.dimensions.margin.right = resolve_or_zero(&style.margin_right, cbw);
            }
        }
    }
}

/// E56-M1: the shrink-to-fit width of the ruby inline-block — the widest of its
/// rows' intrinsic content widths. Each row's intrinsic width is the span
/// (rightmost − leftmost leaf-fragment edge) WITHIN that row: because each row's
/// `text-align` offsets all its fragments by the same amount, `right − left` is
/// alignment-invariant and equals the row's actual content width. Taking the max
/// across rows (rather than one global span) avoids one centered row's offset
/// inflating the measure. `None` if no row has leaf content.
fn ruby_shrink_width(b: &LayoutBox) -> Option<f32> {
    fn row_span(b: &LayoutBox, lo: &mut Option<f32>, hi: &mut Option<f32>) {
        if matches!(
            b.kind,
            BoxKind::TextRun
                | BoxKind::Image
                | BoxKind::Svg
                | BoxKind::InlineBlock
                | BoxKind::FormControl
                | BoxKind::Media
                | BoxKind::Canvas
                | BoxKind::Embed // E61-M2
        ) {
            let mb = b.dimensions.margin_box();
            *lo = Some(lo.map_or(mb.x, |v: f32| v.min(mb.x)));
            *hi = Some(hi.map_or(mb.x + mb.width, |v: f32| v.max(mb.x + mb.width)));
        }
        for c in &b.children {
            row_span(c, lo, hi);
        }
    }
    let mut max: Option<f32> = None;
    for row in &b.children {
        let (mut lo, mut hi) = (None, None);
        row_span(row, &mut lo, &mut hi);
        if let (Some(lo), Some(hi)) = (lo, hi) {
            let w = hi - lo;
            max = Some(max.map_or(w, |m: f32| m.max(w)));
        }
    }
    max
}

/// §3.2 width resolution (content-box, no box-sizing).
fn calculate_block_width(b: &mut LayoutBox, style: &ComputedStyle, containing: Dimensions) {
    let cb = containing.content.width;

    let width = &style.width;
    let mut margin_l = resolve(&style.margin_left, cb);
    let mut margin_r = resolve(&style.margin_right, cb);
    let border_l = style.border_left_width;
    let border_r = style.border_right_width;
    let padding_l = resolve_or_zero(&style.padding_left, cb);
    let padding_r = resolve_or_zero(&style.padding_right, cb);

    // box-sizing: a border-box width folds horizontal padding+border into the
    // specified width (content shrinks). Auto width is untouched (E13-M1).
    let pb_h = padding_l + padding_r + border_l + border_r;
    let width_resolved =
        resolve(width, cb).map(|w| content_from_specified(w, style.box_sizing, pb_h));

    // Sum of all non-auto parts (auto width/margins count as 0).
    let total = margin_l.unwrap_or(0.0)
        + border_l
        + padding_l
        + width_resolved.unwrap_or(0.0)
        + padding_r
        + border_r
        + margin_r.unwrap_or(0.0);

    let width_is_auto = width_resolved.is_none();

    // Overconstrained (fixed width, total too wide): drop auto margins to 0.
    if !width_is_auto && total > cb {
        if margin_l.is_none() {
            margin_l = Some(0.0);
        }
        if margin_r.is_none() {
            margin_r = Some(0.0);
        }
    }

    let underflow = cb - total;

    let (used_width, used_ml, used_mr) = if width_is_auto {
        // Auto width fills available space; auto margins → 0. Exception
        // (E18-M1): width auto + definite Px height + `aspect-ratio` ⇒ derive
        // width = height * ratio instead of filling (percent height against an
        // indefinite CB is deferred). The min/max-width clamp below still applies.
        let w = match style.aspect_ratio {
            Some(ratio) if ratio > 0.0 => match style.height {
                // A border-box height folds vertical padding+border in; the
                // ratio applies to the content box, so convert first (padding %
                // resolves against the containing width `cb`, per CSS).
                Length::Px(h) => {
                    let pb_v = resolve_or_zero(&style.padding_top, cb)
                        + resolve_or_zero(&style.padding_bottom, cb)
                        + style.border_top_width
                        + style.border_bottom_width;
                    (content_from_specified(h, style.box_sizing, pb_v) * ratio).max(0.0)
                }
                _ => underflow.max(0.0),
            },
            _ => underflow.max(0.0),
        };
        (w, margin_l.unwrap_or(0.0), margin_r.unwrap_or(0.0))
    } else {
        let w = width_resolved.unwrap();
        match (margin_l, margin_r) {
            // both auto → center
            (None, None) => (w, underflow / 2.0, underflow / 2.0),
            // left auto → absorbs underflow
            (None, Some(mr)) => (w, underflow, mr),
            // right auto → absorbs underflow
            (Some(ml), None) => (w, ml, underflow),
            // none auto → push right margin by underflow (left-aligned)
            (Some(ml), Some(mr)) => (w, ml, mr + underflow),
        }
    };

    // min/max-width clamp (E13-M1). Only enter the clamp/rebalance path when a
    // constraint is actually set, so default pages stay byte-identical. The
    // clamped width is treated as fixed: any delta is re-absorbed into the right
    // margin (simple policy; auto-margin re-centering is a documented limit).
    let (used_width, used_mr) =
        if !matches!(style.max_width, Length::Auto) || !matches!(style.min_width, Length::Auto) {
            let clamped = clamp_size(
                used_width,
                &style.min_width,
                &style.max_width,
                cb,
                style.box_sizing,
                pb_h,
            );
            let delta = used_width - clamped;
            (clamped, used_mr + delta)
        } else {
            (used_width, used_mr)
        };

    b.dimensions.content.width = used_width;
    b.dimensions.margin.left = used_ml;
    b.dimensions.margin.right = used_mr;
    b.dimensions.border.left = border_l;
    b.dimensions.border.right = border_r;
    b.dimensions.padding.left = padding_l;
    b.dimensions.padding.right = padding_r;
}

/// §3.3 position. `containing.content.height` is the running y-cursor.
fn calculate_block_position(b: &mut LayoutBox, style: &ComputedStyle, containing: Dimensions) {
    let cb = containing.content.width;
    let d = &mut b.dimensions;

    d.margin.top = resolve_or_zero(&style.margin_top, cb);
    d.margin.bottom = resolve_or_zero(&style.margin_bottom, cb);
    d.padding.top = resolve_or_zero(&style.padding_top, cb);
    d.padding.bottom = resolve_or_zero(&style.padding_bottom, cb);
    d.border.top = style.border_top_width;
    d.border.bottom = style.border_bottom_width;

    d.content.x = containing.content.x + d.margin.left + d.border.left + d.padding.left;
    d.content.y = containing.content.y
        + containing.content.height
        + d.margin.top
        + d.border.top
        + d.padding.top;
}

/// §3.4 children. Block children stack with the running-height trick; inline
/// children are flowed via `layout_inline` (now float-aware). Out-of-flow
/// children (floats / abs / fixed) are diverted (§3.3/§4).
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_block_children(
    b: &mut LayoutBox,
    self_style: &ComputedStyle,
    containing: Dimensions,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    floats: &mut FloatContext,
    cache: &LayoutCache,
) {
    // A float / abs / fixed box establishes its OWN BFC for its descendants
    // (its float context is isolated from the surrounding flow, §3.2).
    // E34-M2: `display:flow-root` ALSO establishes its own BFC (it contains its
    // floats), so it gets a fresh, isolated float context just like an
    // out-of-flow box.
    let mut local_floats;
    let child_floats: &mut FloatContext =
        if is_out_of_flow(self_style) || self_style.display == Display::FlowRoot {
            local_floats = FloatContext::default();
            &mut local_floats
        } else {
            floats
        };

    // A `LineBox` child is the artifact of a PRIOR inline pass over this box
    // (this box is re-laid-out by a container that measures then finally places
    // it — table cell / flex|grid item). It is NOT a real block child: re-running
    // inline layout (which re-flattens stale lines, restoring inter-word spaces)
    // keeps that re-layout idempotent. So treat LineBox children as inline.
    let has_block_child = b
        .children
        .iter()
        .any(|c| !c.is_inline_level() && c.kind != BoxKind::LineBox);

    let vertical = self_style.writing_mode.is_vertical();

    if !has_block_child && !b.children.is_empty() {
        // All-inline content → inline layout. In horizontal mode the return is the
        // content height. In vertical mode it is the block-axis (X) extent → the
        // container's width; the height (inline extent) keeps its definite value
        // unless auto (then it stays at whatever the width algorithm produced).
        if vertical {
            // The inline axis is HEIGHT, normally resolved after children. Resolve
            // a definite (Px) height NOW so `layout_inline` has a wrap extent; an
            // indefinite height stays 0 → a single overflowing column (MVP limit).
            if let Length::Px(h) = self_style.height {
                let pb_v = b.dimensions.padding.top
                    + b.dimensions.padding.bottom
                    + b.dimensions.border.top
                    + b.dimensions.border.bottom;
                b.dimensions.content.height =
                    content_from_specified(h, self_style.box_sizing, pb_v);
            }
        }
        let ext = layout_inline(b, doc, styled, m, images, child_floats, cache);
        if vertical {
            if matches!(self_style.width, Length::Auto) {
                b.dimensions.content.width = ext;
            }
        } else {
            b.dimensions.content.height = ext;
        }
        return;
    }

    let mut d = b.dimensions;
    if vertical {
        // Vertical block flow (E18-M3): the block axis is X. The containing block's
        // writing mode drives child placement, so the cursor is applied HERE (in
        // the container) rather than in each child's self-positioning — a child
        // positions itself with the verbatim horizontal formula (x at the
        // container content-left, y at content-top since height=0), then we
        // translate it along X by the running block cursor. Children's own
        // (inherited-vertical) content then flows vertically by recursion.
        let width_is_auto = matches!(self_style.width, Length::Auto);
        d.content.height = 0.0; // children share the inline (Y) start
        let cbw = d.content.width;
        let mut children = std::mem::take(&mut b.children);
        let mut cursor = 0.0f32; // block-axis (X) running offset from content-left
        for child in &mut children {
            let cstyle = style_of(styled, child);
            // Floats/abs/clear in vertical mode are deferred: treat every child as
            // in-flow (documented MVP limit §3).
            layout_block(child, d, styled, doc, m, images, child_floats, cache);
            // The horizontal width algorithm absorbs CB underflow into the right
            // margin to fill the line; in vertical flow the horizontal axis is the
            // BLOCK axis, so a child wants a tight margin-box. Re-pin the specified
            // horizontal margins (auto→0), collapsing the spurious fill.
            child.dimensions.margin.left = resolve_or_zero(&cstyle.margin_left, cbw);
            child.dimensions.margin.right = resolve_or_zero(&cstyle.margin_right, cbw);
            translate_box(child, cursor, 0.0);
            cursor += child.dimensions.margin_box().width;
        }
        let extent = cursor;
        // vertical-rl: the block axis grows leftward — the FIRST child sits at the
        // right edge. Children were placed left-to-right; mirror each within the
        // block extent. vertical-lr is left-origin (natural), no mirror.
        if self_style.writing_mode == WritingMode::VerticalRl {
            let cb_left = d.content.x;
            for child in &mut children {
                let mb = child.dimensions.margin_box();
                let rel = mb.x - cb_left; // left-to-right offset from container left
                let new_left = cb_left + (extent - rel - mb.width);
                translate_box(child, new_left - mb.x, 0.0);
            }
        }
        b.children = children;
        if width_is_auto {
            b.dimensions.content.width = extent;
        }
        return;
    }

    d.content.height = 0.0;
    // Take the children out so we can index `styled`/`floats` mutably alongside.
    let mut children = std::mem::take(&mut b.children);
    for child in &mut children {
        let cstyle = style_of(styled, child);

        // position:absolute / fixed — skip entirely in normal flow (phase 1).
        if matches!(cstyle.position, Position::Absolute | Position::Fixed) {
            continue;
        }

        // float — place out of flow; does not advance the y-cursor (§3.3).
        if cstyle.float != Float::None {
            place_float(
                child,
                &cstyle,
                containing,
                d.content.height,
                styled,
                doc,
                m,
                images,
                child_floats,
                cache,
            );
            continue;
        }

        debug_assert!(is_normal_flow(&cstyle));

        // clear — drop the running cursor below the relevant floats (§3.5).
        if cstyle.clear != Clear::None {
            let sides = match cstyle.clear {
                Clear::Left => ClearSides::Left,
                Clear::Right => ClearSides::Right,
                Clear::Both => ClearSides::Both,
                Clear::None => ClearSides::None,
            };
            let cur_abs_y = containing.content.y + d.content.height;
            let floor = child_floats.clearance_y(sides, cur_abs_y);
            if floor > cur_abs_y {
                d.content.height += floor - cur_abs_y;
            }
        }

        layout_block(child, d, styled, doc, m, images, child_floats, cache);
        d.content.height += child.dimensions.margin_box().height;
    }
    b.children = children;
    // E34-M2: a `display:flow-root` with AUTO height grows to enclose its own
    // floats (it contains them — the defining BFC behaviour). `child_floats` is
    // the LOCAL, isolated context this box established, so `clearance_y` only
    // sees this box's own floats. Explicit (Px) heights are governed by
    // `calculate_block_height`, so gate on auto to stay byte-identical there.
    if self_style.display == Display::FlowRoot && matches!(self_style.height, Length::Auto) {
        let enclosed =
            child_floats.clearance_y(ClearSides::Both, containing.content.y) - containing.content.y;
        d.content.height = d.content.height.max(enclosed.max(0.0));
    }
    // Store accumulated child height in content.height (used when height auto).
    b.dimensions.content.height = d.content.height;
}

/// Place a floated box against its containing block's left/right float stack at
/// the current y-cursor, dropping down when it doesn't fit (§3.3).
#[allow(clippy::too_many_arguments)]
fn place_float(
    child: &mut LayoutBox,
    cstyle: &ComputedStyle,
    containing: Dimensions,
    cur_rel_y: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    floats: &mut FloatContext,
    cache: &LayoutCache,
) {
    let side = if cstyle.float == Float::Right {
        FloatSide::Right
    } else {
        FloatSide::Left
    };
    let cb_left = containing.content.x;
    let cb_right = cb_left + containing.content.width;

    // Lay out the float subtree at a provisional origin to fill its dimensions
    // (width per the normal algorithm; shrink-to-fit ≈ fill available for M2).
    let mut prov = containing;
    prov.content.height = cur_rel_y;
    layout_block(child, prov, styled, doc, m, images, floats, cache);
    // A block absorbs CB underflow into its right margin to fill the line; a
    // float wants a tight margin-box, so re-pin the specified margins (auto→0).
    let cbw = containing.content.width;
    child.dimensions.margin.left = resolve_or_zero(&cstyle.margin_left, cbw);
    child.dimensions.margin.right = resolve_or_zero(&cstyle.margin_right, cbw);

    let fw = child.dimensions.margin_box().width;
    let fh = child.dimensions.margin_box().height;

    // Drop-down search for a y-band where the float fits beside existing floats.
    let mut y = containing.content.y + cur_rel_y;
    loop {
        let left_inset = floats.left_offset(y, fh, cb_left);
        let right_inset = floats.right_offset(y, fh, cb_right);
        let avail = (cb_right - right_inset) - (cb_left + left_inset);
        if fw <= avail {
            // Fits beside existing floats on this band.
            let x = match side {
                FloatSide::Left => cb_left + left_inset,
                FloatSide::Right => (cb_right - right_inset) - fw,
            };
            let cur_mb = child.dimensions.margin_box();
            translate_box(child, x - cur_mb.x, y - cur_mb.y);
            floats.add(side, child.dimensions.margin_box(), shape_outside_of(cstyle));
            return;
        }
        // Drop below the nearest float bottom and retry.
        let next = floats.clearance_y(ClearSides::Both, y);
        if next <= y {
            // No lower band — place at current y overflowing.
            let x = match side {
                FloatSide::Left => cb_left + left_inset,
                FloatSide::Right => (cb_right - right_inset) - fw,
            };
            let cur_mb = child.dimensions.margin_box();
            translate_box(child, x - cur_mb.x, y - cur_mb.y);
            floats.add(side, child.dimensions.margin_box(), shape_outside_of(cstyle));
            return;
        }
        y = next;
    }
}

/// The float's `shape-outside` shape (E65-M1), unboxed, or `None`.
fn shape_outside_of(cstyle: &ComputedStyle) -> Option<starfish_style::ClipShape> {
    cstyle.shape_outside.as_deref().cloned()
}

/// §3.5 height. Auto → keep accumulated child/inline height; explicit → used.
/// box-sizing folds vertical padding+border into a Px height; min/max-height
/// clamp the content height (px constraints only) (E13-M1).
fn calculate_block_height(b: &mut LayoutBox, style: &ComputedStyle, cb_width: f32) {
    let pb_v = b.dimensions.padding.top
        + b.dimensions.padding.bottom
        + b.dimensions.border.top
        + b.dimensions.border.bottom;
    match style.height {
        Length::Px(v) => {
            b.dimensions.content.height = content_from_specified(v, style.box_sizing, pb_v)
        }
        // Percent height against an indefinite CB → treat as Auto (§7). A calc()
        // height with a percent part is likewise ignored here (E13-M2), as is an
        // unfoldable math tree (it always carries a percent part) (E24-M1).
        Length::Percent(_) | Length::Auto | Length::Calc { .. } | Length::Math(_) => {
            // content.height already holds the accumulated child/inline height.
            // `aspect-ratio` (E18-M1): height auto + definite width property
            // ⇒ derive height = content_width / ratio (before the min/max clamp,
            // so an explicit max-height still caps the derived value).
            if let Some(ratio) = style.aspect_ratio {
                if ratio > 0.0
                    && matches!(style.height, Length::Auto)
                    && resolve(&style.width, cb_width).is_some()
                {
                    b.dimensions.content.height = b.dimensions.content.width / ratio;
                }
            }
        }
    }
    // min/max-height clamp (E13-M1). Px-only constraints under an indefinite CB
    // (cb_basis 0 → percentages resolve to 0, so we only act on Px values). Only
    // enter when a constraint is set, so default pages stay byte-identical.
    if !matches!(style.max_height, Length::Auto) || !matches!(style.min_height, Length::Auto) {
        let min_h = if matches!(style.min_height, Length::Percent(_)) {
            Length::Auto
        } else {
            style.min_height.clone()
        };
        let max_h = if matches!(style.max_height, Length::Percent(_)) {
            Length::Auto
        } else {
            style.max_height.clone()
        };
        b.dimensions.content.height = clamp_size(
            b.dimensions.content.height,
            &min_h,
            &max_h,
            0.0,
            style.box_sizing,
            pb_v,
        );
    }
}

/// A box is positionable iff it is a genuine element-generated box (line,
/// anonymous, text and marker boxes borrow the container's style ref and never
/// carry their own positioning).
fn is_positionable_kind(b: &LayoutBox) -> bool {
    matches!(
        b.kind,
        BoxKind::BlockContainer | BoxKind::InlineBlock | BoxKind::InlineBox
    )
}

/// Phase 2 (§4.2): re-walk the tree positioning abs/fixed boxes against the
/// nearest positioned ancestor's padding box (`abs_cb`) or the viewport
/// (`viewport`, used for `fixed`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_absolutes(
    b: &mut LayoutBox,
    abs_cb: Rect,
    viewport: Rect,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) {
    let style = style_of(styled, b);
    // Only genuine element boxes carry positioning; line/anonymous/text boxes
    // borrow the container's style ref and must be ignored here.
    let positionable = is_positionable_kind(b);

    match style.position {
        Position::Absolute if positionable => {
            layout_abs_box(b, abs_cb, &style, styled, doc, m, images, cache)
        }
        Position::Fixed if positionable => {
            layout_abs_box(b, viewport, &style, styled, doc, m, images, cache)
        }
        _ => {}
    }

    // The CB this box provides to its abs descendants: its own padding box iff
    // it is positioned, else inherit the incoming one.
    let child_abs_cb = if positionable && style.position != Position::Static {
        b.dimensions.padding_box()
    } else {
        abs_cb
    };
    for c in &mut b.children {
        layout_absolutes(c, child_abs_cb, viewport, styled, doc, m, images, cache);
    }
}

/// Size + position one absolutely/fixed box against containing-block rect `cb`.
#[allow(clippy::too_many_arguments)]
fn layout_abs_box(
    b: &mut LayoutBox,
    cb: Rect,
    s: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) {
    let cbw = cb.width;
    let cbh = cb.height;

    // --- width ---
    // box-sizing + min/max for the abs box's own width (E13-M1). Horizontal
    // padding+border of the abs box itself.
    let abs_pb_h = s.border_left_width
        + s.border_right_width
        + resolve_or_zero(&s.padding_left, cbw)
        + resolve_or_zero(&s.padding_right, cbw);
    let used_w = match resolve(&s.width, cbw) {
        Some(w) => {
            let content = content_from_specified(w, s.box_sizing, abs_pb_h);
            clamp_size(
                content,
                &s.min_width,
                &s.max_width,
                cbw,
                s.box_sizing,
                abs_pb_h,
            )
        }
        None => match (resolve(&s.left, cbw), resolve(&s.right, cbw)) {
            (Some(l), Some(r)) => {
                let bpm = s.border_left_width
                    + s.border_right_width
                    + resolve_or_zero(&s.padding_left, cbw)
                    + resolve_or_zero(&s.padding_right, cbw)
                    + resolve_or_zero(&s.margin_left, cbw)
                    + resolve_or_zero(&s.margin_right, cbw);
                (cbw - l - r - bpm).max(0.0)
            }
            // shrink-to-fit (M2 approximation: lay out at cbw, take used width).
            _ => cbw,
        },
    };

    // Lay the box's own block out at the resolved width to fill children/height.
    let containing = Dimensions {
        content: Rect {
            x: cb.x,
            y: cb.y,
            width: used_w,
            height: 0.0,
        },
        ..Dimensions::default()
    };
    let mut local_floats = FloatContext::default(); // abs box is its own BFC
    layout_block(
        b,
        containing,
        styled,
        doc,
        m,
        images,
        &mut local_floats,
        cache,
    );
    b.dimensions.content.width = used_w;

    // --- position (by the margin-box top-left) ---
    let mb = b.dimensions.margin_box();
    let x = match (resolve(&s.left, cbw), resolve(&s.right, cbw)) {
        (Some(l), _) => cb.x + l,
        (None, Some(r)) => cb.x + cb.width - r - mb.width,
        (None, None) => cb.x, // static-position approximation
    };
    let y = match (resolve(&s.top, cbh), resolve(&s.bottom, cbh)) {
        (Some(t), _) => cb.y + t,
        (None, Some(b_off)) => cb.y + cb.height - b_off - mb.height,
        (None, None) => cb.y, // static-position approximation
    };
    let cur = b.dimensions.margin_box();
    translate_box(b, x - cur.x, y - cur.y);
}
