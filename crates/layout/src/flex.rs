//! Flex layout (E2-M3): a parallel layout mode dispatched from `layout_block`
//! when a box's `display` is `flex` / `inline-flex`. See `docs/design/E2-M3.md`.
//!
//! The container's width/position is already resolved by `layout_block` before
//! `layout_flex` is called; this module lays out the in-flow children (flex
//! items) along a main axis with grow/shrink + cross-axis alignment, positions
//! them with absolute coordinates via `translate_box`, and returns the
//! container's content-box height.

use starfish_dom::Document;
use starfish_style::{
    AlignItems, AlignSelf, ComputedStyle, FlexWrap, JustifyContent, Length, StyledTree,
};

use crate::block::{clamp_size, content_from_specified, layout_block, resolve, resolve_or_zero};
use crate::boxtree::{is_normal_flow, style_of, BoxStyleRef, LayoutBox};
use crate::cache::{LayoutCache, MeasureKind};
use crate::dimensions::{Dimensions, Rect};
use crate::float::FloatContext;
use crate::inline::translate_box;
use crate::measure::{ImageSource, TextMeasurer};

/// Logical-axis helper. `row == true` means the main axis is horizontal.
#[derive(Clone, Copy)]
struct Axis {
    row: bool,
}

impl Axis {
    /// Outer (margin-box) main size of a laid-out box.
    fn outer_main(&self, d: &Dimensions) -> f32 {
        if self.row {
            d.margin_box().width
        } else {
            d.margin_box().height
        }
    }
    /// Outer (margin-box) cross size of a laid-out box.
    fn outer_cross(&self, d: &Dimensions) -> f32 {
        if self.row {
            d.margin_box().height
        } else {
            d.margin_box().width
        }
    }
}

/// Per-item working state.
struct FlexItem {
    /// Index into the container's `children` Vec.
    idx: usize,
    style: ComputedStyle,
    grow: f32,
    shrink: f32,
    /// Flex base (content) main size.
    base_main: f32,
    /// Resolved (content) main size after grow/shrink.
    used_main: f32,
    align: AlignItems,
    /// True iff the item's cross-size property is auto (eligible for stretch).
    cross_auto: bool,
    /// Outer cross size after the item is laid out at its used main size.
    outer_cross: f32,
}

/// One flex line: indices into the `items` Vec.
struct FlexLine {
    items: Vec<usize>,
}

/// Lay out a flex container's in-flow children. Returns the container's
/// content-box height (cross extent for row, main extent for column).
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_flex(
    b: &mut LayoutBox,
    containing: Dimensions,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let dir = self_style.flex_direction;
    let axis = Axis { row: dir.is_row() };

    // gap *between items along main* / *between lines along cross*.
    let cbw = containing.content.width;
    let row_gap = resolve_or_zero(&self_style.row_gap, cbw);
    let col_gap = resolve_or_zero(&self_style.column_gap, cbw);
    let main_gap = if axis.row { col_gap } else { row_gap };
    let cross_gap = if axis.row { row_gap } else { col_gap };

    // Container content-box origin + definite main/cross size.
    let content_x = b.dimensions.content.x;
    let content_y = b.dimensions.content.y;
    let content_w = b.dimensions.content.width; // definite (block width algo)
                                                // Definite container height, if any (explicit `height`).
    let explicit_h = resolve(&self_style.height, containing.content.height);

    let (main_size, cross_size_def): (f32, Option<f32>) = if axis.row {
        (content_w, explicit_h)
    } else {
        // Column: main is vertical (height if explicit, else content-driven),
        // cross is the content width (definite).
        (explicit_h.unwrap_or(0.0), Some(content_w))
    };
    let main_definite = axis.row || explicit_h.is_some();

    // --- Collect flex items (skip out-of-flow children). ---
    let mut items: Vec<FlexItem> = Vec::new();
    let children = std::mem::take(&mut b.children);
    let mut children = children;
    for (idx, child) in children.iter_mut().enumerate() {
        let cstyle = style_of(styled, child);
        if !is_normal_flow(&cstyle) {
            continue; // float/abs/fixed are not flex items (M3 §6).
        }
        // Flex base size along the main axis.
        let base_main = flex_base_main(
            child, &cstyle, &axis, content_w, main_size, styled, doc, m, images, cache,
        );
        let cross_auto = if axis.row {
            matches!(cstyle.height, Length::Auto)
        } else {
            matches!(cstyle.width, Length::Auto)
        };
        let align = match cstyle.align_self {
            AlignSelf::Auto => self_style.align_items,
            AlignSelf::Stretch => AlignItems::Stretch,
            AlignSelf::FlexStart => AlignItems::FlexStart,
            AlignSelf::FlexEnd => AlignItems::FlexEnd,
            AlignSelf::Center => AlignItems::Center,
            AlignSelf::Baseline => AlignItems::Baseline,
        };
        items.push(FlexItem {
            idx,
            grow: cstyle.flex_grow,
            shrink: cstyle.flex_shrink,
            base_main,
            used_main: base_main,
            align,
            cross_auto,
            outer_cross: 0.0,
            style: cstyle,
        });
    }

    // Outer main size of an item = content main + main bpm + main margins.
    let item_outer_main =
        |it: &FlexItem| it.used_main + main_bpm_margin(&it.style, &axis, content_w);
    let item_base_outer =
        |it: &FlexItem| it.base_main + main_bpm_margin(&it.style, &axis, content_w);

    // --- Collect items into lines. ---
    let lines = collect_lines(
        &items,
        self_style.flex_wrap,
        main_size,
        main_definite,
        main_gap,
        &item_base_outer,
    );

    // --- Resolve flexible lengths per line. ---
    for line in &lines {
        let n = line.items.len();
        if n == 0 {
            continue;
        }
        let gaps = main_gap * (n.saturating_sub(1) as f32);
        let content_main: f32 = line.items.iter().map(|&i| item_base_outer(&items[i])).sum();
        // For an auto-height column container, the main size grows to fit; no
        // grow/shrink free space to distribute.
        let inner_main = if main_definite {
            main_size
        } else {
            content_main + gaps
        };
        let free = inner_main - gaps - content_main;
        resolve_flex_line(&mut items, line, free, main_gap);
    }

    // min/max clamp on the item's main-axis used size (E13-M1). Single-pass: the
    // clamped value is not redistributed back into the line's free space
    // (documented limitation). Guard so default items are unchanged: only items
    // with a non-Auto main-axis min/max are touched. Basis (cbw) for the main
    // axis is the container content width for row, container main size for column.
    let main_basis = if axis.row { content_w } else { main_size };
    for it in &mut items {
        let (min, max) = if axis.row {
            (&it.style.min_width, &it.style.max_width)
        } else {
            (&it.style.min_height, &it.style.max_height)
        };
        if !matches!(min, Length::Auto) || !matches!(max, Length::Auto) {
            let pb = main_bp(&it.style, &axis, content_w);
            it.used_main = clamp_size(it.used_main, min, max, main_basis, it.style.box_sizing, pb);
        }
    }

    // --- Lay out each item at its used main size; read cross size. ---
    for it in &mut items {
        let child = &mut children[it.idx];
        layout_item(
            child,
            &it.style,
            &axis,
            content_w,
            it.used_main,
            styled,
            doc,
            m,
            images,
            cache,
        );
        it.outer_cross = axis.outer_cross(&child.dimensions);
    }

    // --- Line cross sizes. ---
    let mut line_cross: Vec<f32> = Vec::with_capacity(lines.len());
    for line in &lines {
        let c = line
            .items
            .iter()
            .map(|&i| items[i].outer_cross)
            .fold(0.0_f32, f32::max);
        line_cross.push(c);
    }
    // Container cross size: explicit (single line fills it) else sum of lines.
    let total_lines_cross: f32 =
        line_cross.iter().sum::<f32>() + cross_gap * (lines.len().saturating_sub(1) as f32);
    let container_cross = cross_size_def.unwrap_or(total_lines_cross);
    // A single definite-cross line stretches to the container cross size.
    if lines.len() == 1 {
        if let Some(cd) = cross_size_def {
            line_cross[0] = line_cross[0].max(cd);
        }
    }

    // --- Stretch items whose align is stretch and cross size is auto. ---
    for (li, line) in lines.iter().enumerate() {
        let lc = line_cross[li];
        for &i in &line.items {
            let it = &mut items[i];
            if it.align == AlignItems::Stretch && it.cross_auto {
                let child = &mut children[it.idx];
                stretch_item(
                    child,
                    &it.style,
                    &axis,
                    content_w,
                    it.used_main,
                    lc,
                    styled,
                    doc,
                    m,
                    images,
                    cache,
                );
                it.outer_cross = axis.outer_cross(&child.dimensions);
            }
        }
    }

    // --- Position items: justify-content (main) + align (cross). ---
    let main_origin = if axis.row { content_x } else { content_y };
    let cross_origin = if axis.row { content_y } else { content_x };
    let mut line_cross_start = cross_origin;
    for (li, line) in lines.iter().enumerate() {
        let lc = line_cross[li];
        position_line(
            &mut children,
            &items,
            line,
            &axis,
            self_style.justify_content,
            dir.is_reverse(),
            main_origin,
            main_size,
            main_definite,
            main_gap,
            line_cross_start,
            lc,
        );
        line_cross_start += lc + cross_gap;
    }

    b.children = children;

    // Container content height (§3.9).
    if axis.row {
        container_cross
    } else {
        // Column: height = main extent = max line used main + gaps.
        let mut max_main = 0.0_f32;
        for line in &lines {
            let n = line.items.len();
            let gaps = main_gap * (n.saturating_sub(1) as f32);
            let sum: f32 = line.items.iter().map(|&i| item_outer_main(&items[i])).sum();
            max_main = max_main.max(sum + gaps);
        }
        explicit_h.unwrap_or(max_main)
    }
}

/// Main-axis border+padding+margins of an item (added to its content main size
/// to get the outer/margin-box main size).
fn main_bpm_margin(s: &ComputedStyle, axis: &Axis, cbw: f32) -> f32 {
    if axis.row {
        s.border_left_width
            + s.border_right_width
            + resolve_or_zero(&s.padding_left, cbw)
            + resolve_or_zero(&s.padding_right, cbw)
            + resolve_or_zero(&s.margin_left, cbw)
            + resolve_or_zero(&s.margin_right, cbw)
    } else {
        s.border_top_width
            + s.border_bottom_width
            + resolve_or_zero(&s.padding_top, cbw)
            + resolve_or_zero(&s.padding_bottom, cbw)
            + resolve_or_zero(&s.margin_top, cbw)
            + resolve_or_zero(&s.margin_bottom, cbw)
    }
}

/// Main-axis border+padding (no margin) of an item — the `pb` for box-sizing on
/// the main axis (E13-M1).
fn main_bp(s: &ComputedStyle, axis: &Axis, cbw: f32) -> f32 {
    if axis.row {
        s.border_left_width
            + s.border_right_width
            + resolve_or_zero(&s.padding_left, cbw)
            + resolve_or_zero(&s.padding_right, cbw)
    } else {
        s.border_top_width
            + s.border_bottom_width
            + resolve_or_zero(&s.padding_top, cbw)
            + resolve_or_zero(&s.padding_bottom, cbw)
    }
}

/// Flex base (content) main size of an item (§3.3). Prefers explicit basis /
/// main size; falls back to the item's content extent via a block layout pass.
#[allow(clippy::too_many_arguments)]
fn flex_base_main(
    child: &mut LayoutBox,
    s: &ComputedStyle,
    axis: &Axis,
    cbw: f32,
    main_size: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let pb = main_bp(s, axis, cbw);
    // flex-basis (non-auto) → resolve against the container main size, then fold
    // box-sizing (border-box basis shrinks the content) (E13-M1).
    if !matches!(s.flex_basis, Length::Auto) {
        if let Some(v) = resolve(&s.flex_basis, main_size) {
            return content_from_specified(v.max(0.0), s.box_sizing, pb);
        }
    }
    // Auto basis: use the explicit main-axis size property if set.
    let main_len = if axis.row { &s.width } else { &s.height };
    if let Some(v) = resolve(main_len, if axis.row { cbw } else { main_size }) {
        return content_from_specified(v.max(0.0), s.box_sizing, pb);
    }
    // Else lay the item out at the container main size and read its content
    // extent along the main axis (pragmatic content size, §3.3 / §6).
    // E12-M1: memoize per (node, FlexBaseMain, cbw); anonymous boxes (whose
    // NodeId is a shared parent ref) bypass the cache.
    let node = match child.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: cbw,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(child, cb, styled, doc, m, images, &mut floats, cache);
        if axis.row {
            child.dimensions.content.width
        } else {
            child.dimensions.content.height
        }
    };
    match node {
        Some(node) => cache.measure(node, MeasureKind::FlexBaseMain, cbw, compute),
        None => compute(),
    }
}

/// Greedy line collection (§3.4).
fn collect_lines(
    items: &[FlexItem],
    wrap: FlexWrap,
    main_size: f32,
    main_definite: bool,
    main_gap: f32,
    item_base_outer: &dyn Fn(&FlexItem) -> f32,
) -> Vec<FlexLine> {
    if wrap == FlexWrap::Nowrap || !main_definite {
        // Single line with all items.
        return vec![FlexLine {
            items: (0..items.len()).collect(),
        }];
    }
    let mut lines: Vec<FlexLine> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut used = 0.0_f32;
    for (i, it) in items.iter().enumerate() {
        let w = item_base_outer(it);
        let gap = if cur.is_empty() { 0.0 } else { main_gap };
        if !cur.is_empty() && used + gap + w > main_size {
            lines.push(FlexLine {
                items: std::mem::take(&mut cur),
            });
            used = 0.0;
        }
        let gap = if cur.is_empty() { 0.0 } else { main_gap };
        used += gap + w;
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(FlexLine { items: cur });
    }
    if lines.is_empty() {
        lines.push(FlexLine { items: Vec::new() });
    }
    lines
}

/// Distribute `free` main space across a line's items by grow (free>0) or scaled
/// shrink (free<0). Single pass, results clamped to >= 0 (§3.5).
fn resolve_flex_line(items: &mut [FlexItem], line: &FlexLine, free: f32, _main_gap: f32) {
    if line.items.is_empty() {
        return;
    }
    if free > 0.0 {
        let sum_grow: f32 = line.items.iter().map(|&i| items[i].grow).sum();
        if sum_grow > 0.0 {
            for &i in &line.items {
                let g = items[i].grow;
                items[i].used_main = items[i].base_main + free * (g / sum_grow);
            }
        }
    } else if free < 0.0 {
        let sum_w: f32 = line
            .items
            .iter()
            .map(|&i| items[i].shrink * items[i].base_main)
            .sum();
        if sum_w > 0.0 {
            for &i in &line.items {
                let w = items[i].shrink * items[i].base_main;
                let delta = free * (w / sum_w);
                items[i].used_main = (items[i].base_main + delta).max(0.0);
            }
        }
    }
}

/// Lay out an item at its resolved main size, re-pinning margins tight (§3.6).
#[allow(clippy::too_many_arguments)]
fn layout_item(
    child: &mut LayoutBox,
    s: &ComputedStyle,
    axis: &Axis,
    cbw: f32,
    used_main: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) {
    if axis.row {
        // Containing width = used_main so the item's width algorithm produces it.
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: used_main,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(child, cb, styled, doc, m, images, &mut floats, cache);
        // Force the content width to the resolved main size (overrides the block
        // width algorithm's auto-fill / underflow handling) and pin margins.
        child.dimensions.content.width = used_main;
        child.dimensions.margin.left = resolve_or_zero(&s.margin_left, cbw);
        child.dimensions.margin.right = resolve_or_zero(&s.margin_right, cbw);
    } else {
        // Column: lay out at the container content width; force content height to
        // the resolved main size.
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: cbw,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(child, cb, styled, doc, m, images, &mut floats, cache);
        child.dimensions.content.height = used_main;
        child.dimensions.margin.left = resolve_or_zero(&s.margin_left, cbw);
        child.dimensions.margin.right = resolve_or_zero(&s.margin_right, cbw);
    }
}

/// Stretch an item's cross size so its outer cross fills the line cross (§3.6.4).
#[allow(clippy::too_many_arguments)]
fn stretch_item(
    child: &mut LayoutBox,
    s: &ComputedStyle,
    axis: &Axis,
    cbw: f32,
    used_main: f32,
    line_cross: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) {
    let cross_bpm_margin = if axis.row {
        // cross = vertical
        s.border_top_width
            + s.border_bottom_width
            + resolve_or_zero(&s.padding_top, cbw)
            + resolve_or_zero(&s.padding_bottom, cbw)
            + resolve_or_zero(&s.margin_top, cbw)
            + resolve_or_zero(&s.margin_bottom, cbw)
    } else {
        s.border_left_width
            + s.border_right_width
            + resolve_or_zero(&s.padding_left, cbw)
            + resolve_or_zero(&s.padding_right, cbw)
            + resolve_or_zero(&s.margin_left, cbw)
            + resolve_or_zero(&s.margin_right, cbw)
    };
    let content_cross = (line_cross - cross_bpm_margin).max(0.0);
    if axis.row {
        // Vertical stretch: just set the content height (no reflow needed).
        child.dimensions.content.height = content_cross;
    } else {
        // Horizontal stretch: re-run block layout at the new width so the item's
        // inline content reflows.
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: content_cross,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(child, cb, styled, doc, m, images, &mut floats, cache);
        child.dimensions.content.width = content_cross;
        child.dimensions.content.height = used_main;
        child.dimensions.margin.left = resolve_or_zero(&s.margin_left, cbw);
        child.dimensions.margin.right = resolve_or_zero(&s.margin_right, cbw);
    }
}

/// Place one line's items: justify-content along main, align along cross (§3.7/8).
#[allow(clippy::too_many_arguments)]
fn position_line(
    children: &mut [LayoutBox],
    items: &[FlexItem],
    line: &FlexLine,
    axis: &Axis,
    justify: JustifyContent,
    reverse: bool,
    main_origin: f32,
    main_size: f32,
    main_definite: bool,
    main_gap: f32,
    line_cross_start: f32,
    line_cross: f32,
) {
    let n = line.items.len();
    if n == 0 {
        return;
    }
    let gaps = main_gap * (n.saturating_sub(1) as f32);
    let used_total: f32 = line
        .items
        .iter()
        .map(|&i| axis.outer_main(&children[items[i].idx].dimensions))
        .sum();
    let leftover = if main_definite {
        (main_size - gaps - used_total).max(0.0)
    } else {
        0.0
    };

    // Reverse mirroring is measured against the main extent. For a definite main
    // axis (row width, or explicit column height) that's the container `main_size`.
    // For an indefinite main axis (auto-height column) `main_size` is 0, so mirror
    // against the line's used main content extent instead — otherwise reversed
    // items land at negative coordinates above the container.
    let main_extent = if main_definite {
        main_size
    } else {
        gaps + used_total
    };

    let (lead, between) = justify_offsets(justify, leftover, n);

    // Place items in DOM order. `cursor` is the offset from the *main-start*
    // edge. For reverse directions the main-start edge is the far (right/bottom)
    // edge, so we convert the start-offset to a physical coordinate by mirroring.
    let mut cursor = lead;
    for &i in &line.items {
        let it = &items[i];
        let child = &mut children[it.idx];
        let outer_main = axis.outer_main(&child.dimensions);
        let outer_cross = axis.outer_cross(&child.dimensions);

        let cross_pos = align_cross(it.align, line_cross, outer_cross);

        // Physical main-start coordinate of the item's margin box.
        let main_phys = if reverse {
            main_origin + main_extent - cursor - outer_main
        } else {
            main_origin + cursor
        };

        let (mx, my) = if axis.row {
            (main_phys, line_cross_start + cross_pos)
        } else {
            (line_cross_start + cross_pos, main_phys)
        };
        let cur = child.dimensions.margin_box();
        translate_box(child, mx - cur.x, my - cur.y);

        cursor += outer_main + main_gap + between;
    }
}

/// justify-content → (leading offset, extra spacing between items) (§3.7).
fn justify_offsets(justify: JustifyContent, leftover: f32, n: usize) -> (f32, f32) {
    let nf = n as f32;
    match justify {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::FlexEnd => (leftover, 0.0),
        JustifyContent::Center => (leftover / 2.0, 0.0),
        JustifyContent::SpaceBetween => {
            if n > 1 {
                (0.0, leftover / (nf - 1.0))
            } else {
                (0.0, 0.0)
            }
        }
        JustifyContent::SpaceAround => {
            let between = leftover / nf;
            (between / 2.0, between)
        }
        JustifyContent::SpaceEvenly => {
            let between = leftover / (nf + 1.0);
            (between, between)
        }
    }
}

/// align-items/self → cross offset of an item within its line (§3.8).
fn align_cross(align: AlignItems, line_cross: f32, item_outer_cross: f32) -> f32 {
    let free = line_cross - item_outer_cross;
    match align {
        // Stretch already filled the item; treat as cross-start.
        AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Baseline => 0.0,
        AlignItems::FlexEnd => free,
        AlignItems::Center => free / 2.0,
    }
}
