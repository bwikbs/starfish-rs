//! Block layout (§3): width, position, children, height. No margin collapsing.

use starfish_style::{ComputedStyle, Length};

use crate::boxtree::{style_of, LayoutBox};
use crate::dimensions::Dimensions;
use crate::inline::layout_inline;
use crate::measure::TextMeasurer;
use starfish_style::StyledTree;

/// Resolve a `Length` against the containing-block width. `Auto` → `None`.
fn resolve(len: Length, cb_width: f32) -> Option<f32> {
    match len {
        Length::Px(v) => Some(v),
        Length::Percent(p) => Some(p / 100.0 * cb_width),
        Length::Auto => None,
    }
}

/// Resolve a `Length` used as a definite value (`Auto` → 0).
fn resolve_or_zero(len: Length, cb_width: f32) -> f32 {
    resolve(len, cb_width).unwrap_or(0.0)
}

/// Lay out a block-level box (`BlockContainer` / `AnonymousBlock`) within its
/// containing block's content geometry.
pub(crate) fn layout_block(
    b: &mut LayoutBox,
    containing: Dimensions,
    styled: &StyledTree,
    m: &dyn TextMeasurer,
) {
    let style = style_of(styled, b);
    calculate_block_width(b, &style, containing);
    calculate_block_position(b, &style, containing);
    layout_block_children(b, styled, m);
    calculate_block_height(b, &style);
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

    let width_resolved = resolve(width, cb);

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
/// children are flowed via `layout_inline`.
fn layout_block_children(b: &mut LayoutBox, styled: &StyledTree, m: &dyn TextMeasurer) {
    let has_block_child = b.children.iter().any(|c| !c.is_inline_level());

    if !has_block_child && !b.children.is_empty() {
        // All-inline content → inline layout. Stash height in content.height.
        let h = layout_inline(b, styled, m);
        b.dimensions.content.height = h;
        return;
    }

    let mut d = b.dimensions;
    d.content.height = 0.0;
    for child in &mut b.children {
        layout_block(child, d, styled, m);
        d.content.height += child.dimensions.margin_box().height;
    }
    // Store accumulated child height in content.height (used when height auto).
    b.dimensions.content.height = d.content.height;
}

/// §3.5 height. Auto → keep accumulated child/inline height; explicit → used.
fn calculate_block_height(b: &mut LayoutBox, style: &ComputedStyle) {
    match style.height {
        Length::Px(v) => b.dimensions.content.height = v,
        // Percent height against an indefinite CB → treat as Auto (§7).
        Length::Percent(_) | Length::Auto => {
            // content.height already holds the accumulated child/inline height.
        }
    }
}
