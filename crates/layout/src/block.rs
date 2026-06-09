//! Block layout (§3): width, position, children, height. No margin collapsing.
//! Extended for M2 with float placement, `clear`, and `position:relative`.

use starfish_dom::Document;
use starfish_style::{BoxSizing, Clear, ComputedStyle, Display, Length, Position};

use crate::boxtree::{is_normal_flow, is_out_of_flow, style_of, BoxKind, LayoutBox};
use crate::cache::LayoutCache;
use crate::dimensions::{Dimensions, Rect};
use crate::float::{ClearSides, FloatContext, FloatSide};
use crate::inline::{layout_inline, translate_box};
use crate::measure::{ImageSource, TextMeasurer};
use starfish_style::{Float, StyledTree};

/// Resolve a `Length` against the containing-block width. `Auto` → `None`.
pub(crate) fn resolve(len: Length, cb_width: f32) -> Option<f32> {
    match len {
        Length::Px(v) => Some(v),
        Length::Percent(p) => Some(p / 100.0 * cb_width),
        Length::Auto => None,
        // calc() linear form (E13-M2): px + percent% of the containing block.
        Length::Calc { px, percent } => Some(px + percent / 100.0 * cb_width),
    }
}

/// Resolve a `Length` used as a definite value (`Auto` → 0).
pub(crate) fn resolve_or_zero(len: Length, cb_width: f32) -> f32 {
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
    min: Length,
    max: Length,
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
    if matches!(style.display, Display::Flex | Display::InlineFlex) {
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
    } else {
        layout_block_children(b, &style, containing, styled, doc, m, images, floats, cache);
    }
    calculate_block_height(b, &style);

    // position:relative — reserve space in flow (already done), then translate
    // the whole subtree by the resolved offset (§4.1).
    if style.position == Position::Relative {
        let cbw = containing.content.width;
        let cbh = containing.content.height;
        let dx = rel_offset(style.left, style.right, cbw);
        let dy = rel_offset(style.top, style.bottom, cbh);
        if dx != 0.0 || dy != 0.0 {
            translate_box(b, dx, dy);
        }
    }
}

/// CSS relative offset: `left`/`top` wins if set, else `-right`/`-bottom`, else 0.
fn rel_offset(start: Length, end: Length, basis: f32) -> f32 {
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
    b.dimensions.margin.left = resolve_or_zero(style.margin_left, cbw);
    b.dimensions.margin.right = resolve_or_zero(style.margin_right, cbw);
}

/// §3.2 width resolution (content-box, no box-sizing).
fn calculate_block_width(b: &mut LayoutBox, style: &ComputedStyle, containing: Dimensions) {
    let cb = containing.content.width;

    let width = style.width;
    let mut margin_l = resolve(style.margin_left, cb);
    let mut margin_r = resolve(style.margin_right, cb);
    let border_l = style.border_left_width;
    let border_r = style.border_right_width;
    let padding_l = resolve_or_zero(style.padding_left, cb);
    let padding_r = resolve_or_zero(style.padding_right, cb);

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
        // Auto width fills available space; auto margins → 0.
        let w = underflow.max(0.0);
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
                style.min_width,
                style.max_width,
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

    d.margin.top = resolve_or_zero(style.margin_top, cb);
    d.margin.bottom = resolve_or_zero(style.margin_bottom, cb);
    d.padding.top = resolve_or_zero(style.padding_top, cb);
    d.padding.bottom = resolve_or_zero(style.padding_bottom, cb);
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
    let mut local_floats;
    let child_floats: &mut FloatContext = if is_out_of_flow(self_style) {
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

    if !has_block_child && !b.children.is_empty() {
        // All-inline content → inline layout. Stash height in content.height.
        let h = layout_inline(b, doc, styled, m, images, child_floats, cache);
        b.dimensions.content.height = h;
        return;
    }

    let mut d = b.dimensions;
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
    child.dimensions.margin.left = resolve_or_zero(cstyle.margin_left, cbw);
    child.dimensions.margin.right = resolve_or_zero(cstyle.margin_right, cbw);

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
            floats.add(side, child.dimensions.margin_box());
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
            floats.add(side, child.dimensions.margin_box());
            return;
        }
        y = next;
    }
}

/// §3.5 height. Auto → keep accumulated child/inline height; explicit → used.
/// box-sizing folds vertical padding+border into a Px height; min/max-height
/// clamp the content height (px constraints only) (E13-M1).
fn calculate_block_height(b: &mut LayoutBox, style: &ComputedStyle) {
    let pb_v = b.dimensions.padding.top
        + b.dimensions.padding.bottom
        + b.dimensions.border.top
        + b.dimensions.border.bottom;
    match style.height {
        Length::Px(v) => {
            b.dimensions.content.height = content_from_specified(v, style.box_sizing, pb_v)
        }
        // Percent height against an indefinite CB → treat as Auto (§7). A calc()
        // height with a percent part is likewise ignored here (E13-M2).
        Length::Percent(_) | Length::Auto | Length::Calc { .. } => {
            // content.height already holds the accumulated child/inline height.
        }
    }
    // min/max-height clamp (E13-M1). Px-only constraints under an indefinite CB
    // (cb_basis 0 → percentages resolve to 0, so we only act on Px values). Only
    // enter when a constraint is set, so default pages stay byte-identical.
    if !matches!(style.max_height, Length::Auto) || !matches!(style.min_height, Length::Auto) {
        let min_h = if matches!(style.min_height, Length::Percent(_)) {
            Length::Auto
        } else {
            style.min_height
        };
        let max_h = if matches!(style.max_height, Length::Percent(_)) {
            Length::Auto
        } else {
            style.max_height
        };
        b.dimensions.content.height = clamp_size(
            b.dimensions.content.height,
            min_h,
            max_h,
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
        + resolve_or_zero(s.padding_left, cbw)
        + resolve_or_zero(s.padding_right, cbw);
    let used_w = match resolve(s.width, cbw) {
        Some(w) => {
            let content = content_from_specified(w, s.box_sizing, abs_pb_h);
            clamp_size(
                content,
                s.min_width,
                s.max_width,
                cbw,
                s.box_sizing,
                abs_pb_h,
            )
        }
        None => match (resolve(s.left, cbw), resolve(s.right, cbw)) {
            (Some(l), Some(r)) => {
                let bpm = s.border_left_width
                    + s.border_right_width
                    + resolve_or_zero(s.padding_left, cbw)
                    + resolve_or_zero(s.padding_right, cbw)
                    + resolve_or_zero(s.margin_left, cbw)
                    + resolve_or_zero(s.margin_right, cbw);
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
    let x = match (resolve(s.left, cbw), resolve(s.right, cbw)) {
        (Some(l), _) => cb.x + l,
        (None, Some(r)) => cb.x + cb.width - r - mb.width,
        (None, None) => cb.x, // static-position approximation
    };
    let y = match (resolve(s.top, cbh), resolve(s.bottom, cbh)) {
        (Some(t), _) => cb.y + t,
        (None, Some(b_off)) => cb.y + cb.height - b_off - mb.height,
        (None, None) => cb.y, // static-position approximation
    };
    let cur = b.dimensions.margin_box();
    translate_box(b, x - cur.x, y - cur.y);
}
