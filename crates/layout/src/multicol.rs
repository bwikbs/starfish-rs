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
use starfish_style::{ComputedStyle, Length, StyledTree};

use crate::block::{layout_block, resolve_or_zero};
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
    let gap = match self_style.column_gap {
        Length::Px(0.0) => self_style.font_size,
        g => resolve_or_zero(g, content.width),
    };
    let col_width_px = self_style.column_width.and_then(|l| match l {
        Length::Px(p) => Some(p),
        _ => None,
    });
    let (n, col_w) = resolve_columns(content.width, gap, self_style.column_count, col_width_px);

    // Lay out each in-flow block child into a column-width CB at the container
    // origin; record its margin-box height in source order. Out-of-flow boxes
    // (abs/fixed) are skipped; floats flow as ordinary in-column blocks (MVP).
    let mut children = std::mem::take(&mut b.children);
    let mut measured: Vec<(usize, f32)> = Vec::new();
    for (idx, child) in children.iter_mut().enumerate() {
        let cstyle = style_of(styled, child);
        if matches!(
            cstyle.position,
            starfish_style::Position::Absolute | starfish_style::Position::Fixed
        ) {
            continue;
        }
        let mut col_cb = containing;
        col_cb.content.width = col_w;
        col_cb.content.x = content.x;
        col_cb.content.y = content.y;
        col_cb.content.height = 0.0;
        let mut floats = FloatContext::default();
        layout_block(child, col_cb, styled, doc, m, images, &mut floats, cache);
        let ch = child.dimensions.margin_box().height;
        measured.push((idx, ch));
    }

    // Greedy balance to a target = total / n. Assign each child to a column,
    // tracking the running offset within that column.
    let total: f32 = measured.iter().map(|(_, h)| h).sum();
    let target = if n > 0 { total / n as f32 } else { total };
    let mut col = 0u32;
    let mut col_h = 0.0f32; // running height of the current column
    let mut col_heights = vec![0.0f32; n as usize]; // final per-column heights
    for &(idx, ch) in &measured {
        if col + 1 < n && col_h > 0.0 && col_h + ch > target {
            col += 1;
            col_h = 0.0;
        }
        let y_off = col_h;
        // Reposition: column x + per-column vertical offset.
        let col_x = content.x + col as f32 * (col_w + gap);
        let cur = children[idx].dimensions.margin_box();
        translate_box(
            &mut children[idx],
            col_x - cur.x,
            (content.y + y_off) - cur.y,
        );
        col_h += ch;
        col_heights[col as usize] = col_h;
    }

    b.children = children;
    col_heights.into_iter().fold(0.0f32, f32::max)
}
