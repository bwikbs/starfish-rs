//! Multi-column layout (E18-M2): dispatched from `layout_block` when a box has
//! `column-count` or `column-width` set. The container's width/position is
//! already resolved before `layout_multicol` runs; this module distributes the
//! in-flow *block* children across N equal-width columns (greedy balancing) and
//! returns the container's content-box height (the tallest column).
//!
//! MVP scope: only block-level children columnize. A pure-inline container never
//! reaches this module (the all-inline fast path in `layout_block_children` runs
//! instead), so inline-only multicol stays a single column.

use starfish_dom::Document;
use starfish_style::{ColumnFill, ColumnSpan, ComputedStyle, Length, StyledTree};

use crate::block::{content_from_specified, layout_block, resolve, resolve_or_zero};
use crate::boxtree::{style_of, LayoutBox};
use crate::cache::LayoutCache;
use crate::dimensions::Dimensions;
use crate::float::FloatContext;
use crate::inline::translate_box;
use crate::measure::{ImageSource, TextMeasurer};

/// Resolve the used column count and column width from the available content
/// width `u`, the inter-column `gap`, and the (optional) `count`/`width`.
///
/// - width only: `used = max(1, floor((U+G)/(W+G)))`
/// - count only: `used = max(1, count)`
/// - both: `used = max(1, min(count, floor((U+G)/(W+G))))`
///
/// `col_w = (U - (used-1)*G) / used`, clamped to `>= 0`. A non-positive `W+G`
/// (degenerate width) falls back to a single column.
pub(crate) fn resolve_columns(
    u: f32,
    gap: f32,
    count: Option<u32>,
    width: Option<f32>,
) -> (u32, f32) {
    let by_width = |w: f32| -> u32 {
        let wg = w + gap;
        if wg <= 0.0 {
            1
        } else {
            (((u + gap) / wg).floor() as i32).max(1) as u32
        }
    };
    let used = match (count, width) {
        (None, Some(w)) => by_width(w),
        (Some(c), None) => c.max(1),
        (Some(c), Some(w)) => c.max(1).min(by_width(w)),
        // Should not happen (dispatch guards on at least one being set), but be
        // safe: a single column.
        (None, None) => 1,
    };
    let col_w = ((u - (used - 1) as f32 * gap) / used as f32).max(0.0);
    (used, col_w)
}

/// Lay out a multi-column container's in-flow block children. Returns the
/// container's content-box height (tallest column).
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_multicol(
    b: &mut LayoutBox,
    containing: Dimensions,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let content = b.dimensions.content;

    // `normal` column-gap → 1em (the font-size); else the resolved length.
    let gap = match &self_style.column_gap {
        Length::Px(g) if *g == 0.0 => self_style.font_size,
        g => resolve_or_zero(g, content.width),
    };
    let col_width_px = self_style.column_width.as_ref().and_then(|l| match l {
        Length::Px(p) => Some(*p),
        _ => None,
    });
    let (n, col_w) = resolve_columns(content.width, gap, self_style.column_count, col_width_px);

    // E66-M3: `column-fill: auto` fills each column to the container's DEFINITE
    // content height before moving on. Only a `Length::Px` height is definite
    // here (matching `calculate_block_height`: a percent against an indefinite CB
    // is treated as auto), converted to a content height via box-sizing. With an
    // auto height the fill height is unknown, so `auto` falls back to balancing.
    let fill_height: Option<f32> = if self_style.column_fill == ColumnFill::Auto {
        match self_style.height {
            Length::Px(_) => {
                let pb_v = b.dimensions.padding.top
                    + b.dimensions.padding.bottom
                    + b.dimensions.border.top
                    + b.dimensions.border.bottom;
                resolve(&self_style.height, containing.content.height)
                    .map(|h| content_from_specified(h, self_style.box_sizing, pb_v))
            }
            _ => None,
        }
    } else {
        None
    };

    let mut children = std::mem::take(&mut b.children);

    // Partition the in-flow block children into alternating column GROUPS and
    // SPANNERS (`column-span: all`). Out-of-flow boxes (abs/fixed) are skipped
    // from grouping (E66-M2). We walk source order, collecting indices into the
    // current column group until we hit a spanner, which terminates that group.
    let mut groups: Vec<Vec<usize>> = Vec::new(); // in-flow indices per column group
    let mut spanner_after: Vec<Option<usize>> = Vec::new(); // spanner index following each group (or None)
    let mut cur_group: Vec<usize> = Vec::new();
    for (idx, child) in children.iter().enumerate() {
        let cstyle = style_of(styled, child);
        if matches!(
            cstyle.position,
            starfish_style::Position::Absolute | starfish_style::Position::Fixed
        ) {
            continue;
        }
        if cstyle.column_span == ColumnSpan::All {
            groups.push(std::mem::take(&mut cur_group));
            spanner_after.push(Some(idx));
        } else {
            cur_group.push(idx);
        }
    }
    groups.push(cur_group);
    spanner_after.push(None);

    // Lay out top-to-bottom, advancing a running `y` cursor (content-box
    // relative, starting at `content.y`). Each column group balances into N
    // columns starting at the cursor; each spanner is a full-width block.
    let mut y = content.y;
    for (gi, group) in groups.iter().enumerate() {
        if !group.is_empty() {
            let gh = layout_column_group(
                &mut children, group, containing, content, n, col_w, gap, y, fill_height, styled,
                doc, m, images, cache,
            );
            y += gh;
        }
        if let Some(sidx) = spanner_after[gi] {
            let mut col_cb = containing;
            col_cb.content.width = content.width;
            col_cb.content.x = content.x;
            col_cb.content.y = y;
            col_cb.content.height = 0.0;
            let mut floats = FloatContext::default();
            layout_block(&mut children[sidx], col_cb, styled, doc, m, images, &mut floats, cache);
            let mb = children[sidx].dimensions.margin_box();
            // The block already positioned itself at col_cb.content origin; just
            // advance the cursor by its margin-box height.
            y += mb.height;
        }
    }

    b.children = children;
    y - content.y
}

/// Balance the in-flow block children named by `group` (indices into
/// `children`) into `n` columns of width `col_w`, with the top of the columns
/// at `y_start` (content-box relative). Lays each child into a column-width CB,
/// greedy-fills each column to a target height, repositions via `translate_box`,
/// and returns the group's height (tallest column).
///
/// The target is `total / n` (balance) by default. When `fill_height` is `Some`
/// (E66-M3 `column-fill: auto` with a definite container height) the target is
/// that fixed per-column height instead (NOT divided by `n`), so columns fill
/// sequentially up to `fill_height` before spilling into the next.
#[allow(clippy::too_many_arguments)]
fn layout_column_group(
    children: &mut [LayoutBox],
    group: &[usize],
    containing: Dimensions,
    content: crate::dimensions::Rect,
    n: u32,
    col_w: f32,
    gap: f32,
    y_start: f32,
    fill_height: Option<f32>,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    // Lay out each child into a column-width CB at the group origin; record its
    // margin-box height in source order.
    let mut measured: Vec<(usize, f32)> = Vec::with_capacity(group.len());
    for &idx in group {
        let mut col_cb = containing;
        col_cb.content.width = col_w;
        col_cb.content.x = content.x;
        col_cb.content.y = y_start;
        col_cb.content.height = 0.0;
        let mut floats = FloatContext::default();
        layout_block(&mut children[idx], col_cb, styled, doc, m, images, &mut floats, cache);
        let ch = children[idx].dimensions.margin_box().height;
        measured.push((idx, ch));
    }

    // Target height per column: a definite fill height (column-fill: auto) when
    // known, else total / n (balance). The greedy loop below is identical for
    // both — only the target differs, so the balance path stays byte-identical.
    let total: f32 = measured.iter().map(|(_, h)| h).sum();
    let target = match fill_height {
        Some(h) => h,
        None if n > 0 => total / n as f32,
        None => total,
    };
    let mut col = 0u32;
    let mut col_h = 0.0f32; // running height of the current column
    let mut col_heights = vec![0.0f32; n as usize]; // final per-column heights
    for &(idx, ch) in &measured {
        if col + 1 < n && col_h > 0.0 && col_h + ch > target {
            col += 1;
            col_h = 0.0;
        }
        let y_off = col_h;
        let col_x = content.x + col as f32 * (col_w + gap);
        let cur = children[idx].dimensions.margin_box();
        translate_box(&mut children[idx], col_x - cur.x, (y_start + y_off) - cur.y);
        col_h += ch;
        col_heights[col as usize] = col_h;
    }

    col_heights.into_iter().fold(0.0f32, f32::max)
}
