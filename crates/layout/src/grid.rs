//! Grid layout (E5-M1): a parallel layout mode dispatched from `layout_block`
//! when a box's `display` is `grid` / `inline-grid`. See `docs/design/E5-M1.md`.
//!
//! The container's width/position is already resolved by `layout_block` before
//! `layout_grid` is called; this module resolves explicit track lists, places
//! the in-flow children (grid items) into a 2D track grid (with row-major
//! auto-placement adding implicit rows), sizes columns then rows, lays each item
//! out as a block stretched into its grid area, positions them absolutely via
//! `translate_box`, and returns the container's content-box height.

use starfish_dom::Document;
use starfish_style::{
    AlignItems, AlignSelf, ComputedStyle, GridLine, GridPlacement, JustifyContent, Length,
    StyledTree, TrackSize,
};

use crate::block::{layout_block, resolve, resolve_or_zero};
use crate::boxtree::{is_normal_flow, style_of, BoxKind, LayoutBox};
use crate::dimensions::{Dimensions, Rect};
use crate::float::FloatContext;
use crate::inline::translate_box;
use crate::measure::{ImageSource, TextMeasurer};

/// A fully-resolved set of tracks on one axis: each track's used px size and its
/// start offset (content-box relative, gaps folded into the offsets).
struct Tracks {
    sizes: Vec<f32>,
    offsets: Vec<f32>,
    gap: f32,
}

impl Tracks {
    fn count(&self) -> usize {
        self.sizes.len()
    }

    /// Build line offsets from sizes + gap once sizing is done.
    fn build_offsets(&mut self) {
        let n = self.sizes.len();
        self.offsets = Vec::with_capacity(n + 1);
        let mut off = 0.0;
        self.offsets.push(0.0);
        for (i, sz) in self.sizes.iter().enumerate() {
            off += sz;
            if i + 1 < n {
                off += self.gap;
            }
            self.offsets.push(off);
        }
    }

    /// Build line offsets, distributing `extra` content space per `dist` into a
    /// leading offset + between-track spacing (mirrors flex `justify_offsets`).
    /// `extra <= 0` ⇒ plain start (identical to `build_offsets`).
    fn build_offsets_distributed(&mut self, dist: JustifyContent, extra: f32) {
        let n = self.sizes.len();
        self.offsets = Vec::with_capacity(n + 1);
        if n == 0 {
            self.offsets.push(0.0);
            return;
        }
        let (lead, between) = content_offsets(dist, extra.max(0.0), n);
        let mut off = lead;
        self.offsets.push(off);
        for (i, sz) in self.sizes.iter().enumerate() {
            off += sz;
            if i + 1 < n {
                off += self.gap + between;
            }
            self.offsets.push(off);
        }
    }

    /// Sum of track sizes [start..end) + interior gaps.
    fn span_extent(&self, start: usize, end: usize) -> f32 {
        let mut e = 0.0;
        for t in start..end {
            e += self.sizes[t];
        }
        if end > start + 1 {
            e += self.gap * (end - start - 1) as f32;
        }
        e
    }

    /// Content-box-relative offset of line `i`.
    fn line_offset(&self, i: usize) -> f32 {
        self.offsets[i]
    }
}

/// Content distribution → (leading offset, extra between-track gap). Shared math
/// with flex's `justify_offsets`. `stretch`/`baseline` are not representable in
/// `JustifyContent`; for grid M2 the `space-*` set + start/end/center suffice.
fn content_offsets(dist: JustifyContent, extra: f32, n: usize) -> (f32, f32) {
    let nf = n as f32;
    match dist {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::FlexEnd => (extra, 0.0),
        JustifyContent::Center => (extra / 2.0, 0.0),
        JustifyContent::SpaceBetween => {
            if n > 1 {
                (0.0, extra / (nf - 1.0))
            } else {
                (0.0, 0.0)
            }
        }
        JustifyContent::SpaceAround => {
            let b = extra / nf;
            (b / 2.0, b)
        }
        JustifyContent::SpaceEvenly => {
            let b = extra / (nf + 1.0);
            (b, b)
        }
    }
}

/// Resolve an item's effective inline/block alignment, folding `Auto` → the
/// container default. Returns (justify = inline axis, align = block axis).
fn item_alignment(item: &ComputedStyle, container: &ComputedStyle) -> (AlignItems, AlignItems) {
    let resolve = |sf: AlignSelf, def: AlignItems| match sf {
        AlignSelf::Auto => def,
        AlignSelf::Stretch => AlignItems::Stretch,
        AlignSelf::FlexStart => AlignItems::FlexStart,
        AlignSelf::FlexEnd => AlignItems::FlexEnd,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Baseline => AlignItems::FlexStart, // baseline → start (M2)
    };
    let justify = resolve(item.justify_self, container.justify_items);
    let align = resolve(item.align_self, container.align_items);
    (justify, align)
}

/// Position offset of an item of outer size `outer` within an `area` extent.
fn axis_offset(align: AlignItems, area: f32, outer: f32) -> f32 {
    let free = (area - outer).max(0.0);
    match align {
        AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Baseline => 0.0,
        AlignItems::FlexEnd => free,
        AlignItems::Center => free / 2.0,
    }
}

/// Bounding rectangle (0-based col/row ranges, end-exclusive) of a named area in
/// the area grid: `(col_start, col_end, row_start, row_end)`. `None` if absent.
/// Non-rectangular occurrences are tolerated (min/max bounding box, no panic).
fn named_area_rect(areas: &[Vec<String>], name: &str) -> Option<(usize, usize, usize, usize)> {
    let (mut r0, mut c0) = (usize::MAX, usize::MAX);
    let (mut r1, mut c1) = (0usize, 0usize);
    let mut found = false;
    for (r, row) in areas.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if cell.eq_ignore_ascii_case(name) {
                found = true;
                r0 = r0.min(r);
                c0 = c0.min(c);
                r1 = r1.max(r + 1);
                c1 = c1.max(c + 1);
            }
        }
    }
    if found {
        Some((c0, c1, r0, r1))
    } else {
        None
    }
}

/// A placed grid item: resolved 0-based track ranges + its style/index.
struct PlacedItem {
    idx: usize,
    col_start: usize,
    col_end: usize,
    row_start: usize,
    row_end: usize,
    style: ComputedStyle,
}

/// Lay out a grid container's in-flow children. Returns the container's
/// content-box height (sum of row sizes + row gaps, or the explicit height).
pub(crate) fn layout_grid(
    b: &mut LayoutBox,
    containing: Dimensions,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
) -> f32 {
    let content_x = b.dimensions.content.x;
    let content_y = b.dimensions.content.y;
    let content_w = b.dimensions.content.width;
    let col_gap = resolve_or_zero(self_style.column_gap, content_w);
    let row_gap = resolve_or_zero(self_style.row_gap, content_w);
    let explicit_h = resolve(self_style.height, containing.content.height);

    // Column count: explicit columns, or one implicit full-width column.
    let cols = self_style.grid_template_columns.len().max(1);

    let mut children = std::mem::take(&mut b.children);

    // --- Place items (§4). ---
    let placed = place_items(&children, styled, self_style, cols);

    // Total row count (explicit + implicit), at least the explicit rows.
    let implicit_rows = placed.iter().map(|p| p.row_end).max().unwrap_or(0);
    let rows = self_style.grid_template_rows.len().max(implicit_rows);

    // --- Size columns (§3.3). ---
    let cols_tracks = size_columns(
        self_style,
        content_w,
        col_gap,
        cols,
        &placed,
        &mut children,
        styled,
        doc,
        m,
        images,
    );

    // --- Size rows (§3.4). ---
    let rows_tracks = size_rows(
        self_style,
        explicit_h,
        row_gap,
        rows,
        &cols_tracks,
        &placed,
        &mut children,
        styled,
        doc,
        m,
        images,
    );

    // --- Lay out + position items into their areas (§5). ---
    for p in &placed {
        let col_off = cols_tracks.line_offset(p.col_start);
        let row_off = rows_tracks.line_offset(p.row_start);
        let area_w = cols_tracks.span_extent(p.col_start, p.col_end);
        let area_h = rows_tracks.span_extent(p.row_start, p.row_end);
        let abs_x = content_x + col_off;
        let abs_y = content_y + row_off;

        let s = &p.style;
        let (justify, align) = item_alignment(s, self_style);

        let item = &mut children[p.idx];
        let cb = Dimensions {
            content: Rect { x: abs_x, y: abs_y, width: area_w, height: 0.0 },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats);

        // Pin margins tight (auto → 0).
        item.dimensions.margin.left = resolve_or_zero(s.margin_left, area_w);
        item.dimensions.margin.right = resolve_or_zero(s.margin_right, area_w);
        let hbpm = item.dimensions.border.left
            + item.dimensions.border.right
            + item.dimensions.padding.left
            + item.dimensions.padding.right
            + item.dimensions.margin.left
            + item.dimensions.margin.right;
        let vbpm = item.dimensions.border.top
            + item.dimensions.border.bottom
            + item.dimensions.padding.top
            + item.dimensions.padding.bottom
            + item.dimensions.margin.top
            + item.dimensions.margin.bottom;

        // Inline axis (width): stretch fills the cell (default, cheap); else the
        // item's intrinsic/explicit width clamped to the cell.
        let content_w = if justify == AlignItems::Stretch {
            (area_w - hbpm).max(0.0)
        } else {
            let intrinsic = measure_item_width(item, area_w, styled, doc, m, images);
            intrinsic.min((area_w - hbpm).max(0.0))
        };

        // Block axis (height): stretch fills the cell; else the natural height at
        // the chosen width (measured lazily, keeping the stretch path cheap).
        let content_h = if align == AlignItems::Stretch {
            (area_h - vbpm).max(0.0)
        } else {
            let natural_h = measure_item_height(item, content_w, styled, doc, m, images);
            natural_h.min((area_h - vbpm).max(0.0))
        };
        // `measure_item_height` re-runs `layout_block`, which clobbers
        // `content.width` with the bare auto-layout width. Re-pin both axes to
        // their alignment-resolved values so they survive that side effect.
        item.dimensions.content.width = content_w;
        item.dimensions.content.height = content_h;

        // Position the item's margin box within the area per the two offsets.
        let off_x = axis_offset(justify, area_w, content_w + hbpm);
        let off_y = axis_offset(align, area_h, content_h + vbpm);
        let cur = item.dimensions.margin_box();
        translate_box(item, (abs_x + off_x) - cur.x, (abs_y + off_y) - cur.y);
    }

    b.children = children;

    let total_rows_h = rows_tracks.sizes.iter().sum::<f32>()
        + row_gap * (rows_tracks.count().saturating_sub(1) as f32);
    explicit_h.unwrap_or(total_rows_h)
}

/// Resolve one axis's `GridLine` to a concrete `(start, end)` track range.
/// `n` is the explicit track count on that axis. `clamp_to_n` clamps results
/// into `[0, n]` (used for columns, which never grow implicitly).
fn resolve_axis(line: GridLine, n: usize, clamp_to_n: bool) -> Option<(usize, usize)> {
    let resolve_line = |k: i32| -> i32 {
        if k > 0 {
            k - 1
        } else {
            // negative: -1 => last line (n)
            (n as i32 + 1) + k
        }
    };

    let (mut s, mut e): (i32, i32) = match (line.start, line.end) {
        (GridPlacement::Auto, GridPlacement::Auto) => return None,
        (GridPlacement::Span(_), GridPlacement::Auto)
        | (GridPlacement::Auto, GridPlacement::Span(_)) => return None,
        (GridPlacement::Line(a), GridPlacement::Line(b)) => {
            let (ra, rb) = (resolve_line(a), resolve_line(b));
            let (lo, hi) = if ra <= rb { (ra, rb) } else { (rb, ra) };
            if lo == hi {
                (lo, lo + 1)
            } else {
                (lo, hi)
            }
        }
        (GridPlacement::Line(a), GridPlacement::Span(sp)) => {
            let ra = resolve_line(a);
            (ra, ra + sp as i32)
        }
        (GridPlacement::Span(sp), GridPlacement::Line(b)) => {
            let rb = resolve_line(b);
            (rb - sp as i32, rb)
        }
        (GridPlacement::Line(a), GridPlacement::Auto) => {
            let ra = resolve_line(a);
            (ra, ra + 1)
        }
        (GridPlacement::Auto, GridPlacement::Line(b)) => {
            let rb = resolve_line(b);
            (rb - 1, rb)
        }
        (GridPlacement::Span(_), GridPlacement::Span(_)) => return None,
    };

    if s < 0 {
        s = 0;
    }
    if e <= s {
        e = s + 1;
    }
    if clamp_to_n {
        let nm = n.max(1) as i32;
        if s > nm - 1 {
            s = nm - 1;
        }
        if e > nm {
            e = nm;
        }
        if e <= s {
            e = s + 1;
        }
    }
    Some((s as usize, e as usize))
}

/// Column/row span requested by an item (from `Span`/`Line a / Line b`), else 1.
fn axis_span(line: GridLine) -> usize {
    match (line.start, line.end) {
        (GridPlacement::Span(n), _) | (_, GridPlacement::Span(n)) => (n as usize).max(1),
        (GridPlacement::Line(a), GridPlacement::Line(b)) => {
            ((a - b).unsigned_abs() as usize).max(1)
        }
        _ => 1,
    }
}

/// Place items into a 2D occupancy grid (§4). Two passes: definitely-placed
/// (both axes resolved) first, then row-major auto-placement of the rest.
fn place_items(
    children: &[LayoutBox],
    styled: &StyledTree,
    self_style: &ComputedStyle,
    cols: usize,
) -> Vec<PlacedItem> {
    let n_cols_explicit = self_style.grid_template_columns.len();
    let n_rows_explicit = self_style.grid_template_rows.len();

    // Occupancy grid: occupied[row] is a row of `cols` booleans, grown on demand.
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let ensure_row = |occ: &mut Vec<Vec<bool>>, r: usize| {
        while occ.len() <= r {
            occ.push(vec![false; cols]);
        }
    };
    let cell_free = |occ: &[Vec<bool>], r: usize, c: usize| -> bool {
        occ.get(r).map(|row| !row[c]).unwrap_or(true)
    };

    let mut placed: Vec<PlacedItem> = Vec::new();
    // Collect grid items (skip out-of-flow children) with their resolved axes.
    struct Pending {
        idx: usize,
        style: ComputedStyle,
        col: Option<(usize, usize)>,
        row: Option<(usize, usize)>,
    }
    let mut pendings: Vec<Pending> = Vec::new();
    for (idx, child) in children.iter().enumerate() {
        let cstyle = style_of(styled, child);
        if !is_normal_flow(&cstyle) {
            continue; // float/abs/fixed are not grid items.
        }
        let mut col = resolve_axis(cstyle.grid_column, n_cols_explicit, true);
        let mut row = resolve_axis(cstyle.grid_row, n_rows_explicit, false);

        // Named area wins (E5-M2): a name found in the area grid sets BOTH axes
        // to its bounding rectangle (columns clamped to the explicit count).
        if let Some(name) = &cstyle.grid_area_name {
            if let Some((mut cs, mut ce, rs, re)) =
                named_area_rect(&self_style.grid_template_areas, name)
            {
                cs = cs.min(cols.saturating_sub(1));
                ce = ce.min(cols).max(cs + 1);
                col = Some((cs, ce));
                row = Some((rs, re));
            }
            // name absent / no areas → leave col/row as-is (auto-place, no panic).
        }

        pendings.push(Pending { idx, style: cstyle, col, row });
    }

    // Pass 1: items with BOTH axes definite.
    for p in &pendings {
        if let (Some((cs, ce)), Some((rs, re))) = (p.col, p.row) {
            for r in rs..re {
                ensure_row(&mut occupied, r);
                for slot in occupied[r].iter_mut().take(ce.min(cols)).skip(cs) {
                    *slot = true;
                }
            }
            placed.push(PlacedItem {
                idx: p.idx,
                col_start: cs,
                col_end: ce.min(cols),
                row_start: rs,
                row_end: re,
                style: p.style.clone(),
            });
        }
    }

    // Pass 2: auto-place the rest, row-major.
    let mut cur_row = 0usize;
    let mut cur_col = 0usize;
    for p in &pendings {
        if p.col.is_some() && p.row.is_some() {
            continue; // already placed in pass 1
        }

        let cspan = match p.col {
            Some((cs, ce)) => ce - cs,
            None => axis_span(p.style.grid_column).min(cols),
        };
        let rspan = match p.row {
            Some((rs, re)) => re - rs,
            None => axis_span(p.style.grid_row),
        };

        let (cs, ce, rs, re);
        if let Some((fcs, fce)) = p.col {
            // Fixed column, auto row: scan rows downward for a free band.
            let mut r = cur_row;
            loop {
                ensure_row(&mut occupied, r + rspan.saturating_sub(1));
                let fits = (r..r + rspan)
                    .all(|rr| (fcs..fce).all(|cc| cc >= cols || cell_free(&occupied, rr, cc)));
                if fits {
                    break;
                }
                r += 1;
            }
            cs = fcs;
            ce = fce;
            rs = r;
            re = r + rspan;
        } else if let Some((frs, fre)) = p.row {
            // Fixed row, auto column: scan columns in the band; if no free
            // `cspan`-wide band exists, advance the whole band downward (into
            // implicit rows) and retry — never overlap at col 0 (E5-M2 fix).
            let rspan = fre - frs;
            let mut band = frs;
            let mut found_c = 0usize;
            loop {
                let be = band + rspan;
                ensure_row(&mut occupied, be.saturating_sub(1));
                let mut c = 0usize;
                let mut hit = false;
                while c + cspan <= cols {
                    let fits = (band..be)
                        .all(|rr| (c..c + cspan).all(|cc| cell_free(&occupied, rr, cc)));
                    if fits {
                        found_c = c;
                        hit = true;
                        break;
                    }
                    c += 1;
                }
                if hit {
                    break;
                }
                band += 1;
                // Safety bound: advancing into fresh implicit rows always finds
                // an empty band, so this only guards a pathological grid.
                if band > occupied.len() + cols + 1 {
                    found_c = 0;
                    band = frs;
                    break;
                }
            }
            cs = found_c;
            ce = (found_c + cspan).min(cols);
            rs = band;
            re = band + rspan;
        } else {
            // Fully auto: cursor scan row-major.
            let mut r = cur_row;
            let mut c = cur_col;
            loop {
                if c + cspan > cols {
                    r += 1;
                    c = 0;
                    continue;
                }
                ensure_row(&mut occupied, r + rspan.saturating_sub(1));
                let fits = (r..r + rspan)
                    .all(|rr| (c..c + cspan).all(|cc| cell_free(&occupied, rr, cc)));
                if fits {
                    break;
                }
                c += 1;
            }
            cs = c;
            ce = c + cspan;
            rs = r;
            re = r + rspan;
            // Advance the cursor past the placed item.
            cur_row = r;
            cur_col = ce;
            if cur_col >= cols {
                cur_row += 1;
                cur_col = 0;
            }
        }

        for r in rs..re {
            ensure_row(&mut occupied, r);
            for slot in occupied[r].iter_mut().take(ce.min(cols)).skip(cs) {
                *slot = true;
            }
        }
        placed.push(PlacedItem {
            idx: p.idx,
            col_start: cs,
            col_end: ce.min(cols),
            row_start: rs,
            row_end: re,
            style: p.style.clone(),
        });
    }

    placed
}

/// Rightmost edge (absolute x) of any leaf content (text / image / inline-block)
/// in a subtree, or `None` if there is no such leaf. A LineBox always fills its
/// container width, so we measure the actual placed fragments instead.
fn max_content_right(b: &LayoutBox) -> Option<f32> {
    let mut right: Option<f32> = None;
    if matches!(b.kind, BoxKind::TextRun | BoxKind::Image | BoxKind::InlineBlock) {
        let r = b.dimensions.margin_box().x + b.dimensions.margin_box().width;
        right = Some(r);
    }
    for c in &b.children {
        if let Some(r) = max_content_right(c) {
            right = Some(right.map_or(r, |cur: f32| cur.max(r)));
        }
    }
    right
}

/// Intrinsic (max-content) width of an item laid out at `origin_x`: the extent
/// from its content origin to the rightmost leaf fragment.
fn intrinsic_width(item: &LayoutBox, origin_x: f32) -> f32 {
    match max_content_right(item) {
        Some(r) => (r - origin_x).max(0.0),
        None => item.dimensions.content.width,
    }
}

/// Measure an item's intrinsic content width (pragmatic max-content). If the
/// item has an explicit width, use it; otherwise lay it out at `avail` and take
/// the widest inline content (LineBox/inline-block) it produced.
fn measure_item_width(
    item: &mut LayoutBox,
    avail: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
) -> f32 {
    let s = style_of(styled, item);
    if let Length::Px(v) = s.width {
        return v.max(0.0);
    }
    let cb = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width: avail, height: 0.0 },
        ..Dimensions::default()
    };
    let mut floats = FloatContext::default();
    layout_block(item, cb, styled, doc, m, images, &mut floats);
    intrinsic_width(item, item.dimensions.content.x)
}

/// Measure an item's content height by laying it out at `width`.
fn measure_item_height(
    item: &mut LayoutBox,
    width: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
) -> f32 {
    let cb = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width, height: 0.0 },
        ..Dimensions::default()
    };
    let mut floats = FloatContext::default();
    layout_block(item, cb, styled, doc, m, images, &mut floats);
    item.dimensions.content.height
}

/// Size the column tracks against the definite container content width (§3.3).
#[allow(clippy::too_many_arguments)]
fn size_columns(
    self_style: &ComputedStyle,
    container_w: f32,
    gap: f32,
    cols: usize,
    placed: &[PlacedItem],
    children: &mut [LayoutBox],
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
) -> Tracks {
    let list = &self_style.grid_template_columns;
    let total_gap = gap * (cols.saturating_sub(1) as f32);
    let avail = (container_w - total_gap).max(0.0);

    let mut sizes = vec![0.0f32; cols];

    // Empty template → single implicit auto column filling the width.
    if list.is_empty() {
        sizes[0] = avail;
        let mut t = Tracks { sizes, offsets: Vec::new(), gap };
        t.build_offsets();
        return t;
    }

    // First pass: px + percent; mark auto/fr.
    let mut remaining = avail;
    let mut sum_fr = 0.0f32;
    for (i, ts) in list.iter().enumerate() {
        match ts {
            TrackSize::Px(v) => {
                sizes[i] = *v;
                remaining -= *v;
            }
            TrackSize::Percent(p) => {
                let v = p / 100.0 * container_w;
                sizes[i] = v;
                remaining -= v;
            }
            TrackSize::Fr(f) => {
                sum_fr += *f;
            }
            TrackSize::Auto => {}
        }
    }

    // Auto columns: max content width of single-column items placed in them.
    for (i, ts) in list.iter().enumerate() {
        if !matches!(ts, TrackSize::Auto) {
            continue;
        }
        let mut max_w = 0.0f32;
        for p in placed {
            if p.col_start == i && p.col_end == i + 1 {
                let w = measure_item_width(&mut children[p.idx], avail, styled, doc, m, images);
                max_w = max_w.max(w);
            }
        }
        sizes[i] = max_w;
        remaining -= max_w;
    }

    // Fr columns distribute the remainder.
    if sum_fr > 0.0 {
        let free = remaining.max(0.0);
        let denom = sum_fr.max(1.0);
        for (i, ts) in list.iter().enumerate() {
            if let TrackSize::Fr(f) = ts {
                sizes[i] = free * (f / denom);
            }
        }
    }

    // Content distribution (justify-content) of any leftover space (§2.3). With
    // fr tracks present `extra ≈ 0`, so this is a natural no-op.
    let used = sizes.iter().sum::<f32>() + gap * (cols.saturating_sub(1) as f32);
    let extra = container_w - used;
    let mut t = Tracks { sizes, offsets: Vec::new(), gap };
    t.build_offsets_distributed(self_style.justify_content, extra);
    t
}

/// Size the row tracks (§3.4). The container height may be indefinite, in which
/// case fr rows behave like auto rows.
#[allow(clippy::too_many_arguments)]
fn size_rows(
    self_style: &ComputedStyle,
    explicit_h: Option<f32>,
    gap: f32,
    rows: usize,
    cols_tracks: &Tracks,
    placed: &[PlacedItem],
    children: &mut [LayoutBox],
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
) -> Tracks {
    // Build the row track-size list: explicit list extended with Auto.
    let mut list: Vec<TrackSize> = self_style.grid_template_rows.clone();
    while list.len() < rows {
        list.push(TrackSize::Auto);
    }

    let mut sizes = vec![0.0f32; rows];
    if rows == 0 {
        let mut t = Tracks { sizes, offsets: Vec::new(), gap };
        t.build_offsets();
        return t;
    }

    let definite = explicit_h.is_some();
    let total_gap = gap * (rows.saturating_sub(1) as f32);
    let avail = explicit_h.map(|h| (h - total_gap).max(0.0)).unwrap_or(0.0);

    let mut remaining = avail;
    let mut sum_fr = 0.0f32;

    // Helper: max content height of items in row `r` at their column-span width.
    let auto_height = |r: usize, children: &mut [LayoutBox]| -> f32 {
        let mut max_h = 0.0f32;
        for p in placed {
            if p.row_start == r && p.row_end == r + 1 {
                let w = cols_tracks.span_extent(p.col_start, p.col_end);
                let h = measure_item_height(&mut children[p.idx], w, styled, doc, m, images);
                max_h = max_h.max(h);
            }
        }
        max_h
    };

    for (i, ts) in list.iter().enumerate() {
        match ts {
            TrackSize::Px(v) => {
                sizes[i] = *v;
                remaining -= *v;
            }
            TrackSize::Percent(p) => {
                // Percent rows resolve against the definite height, else auto.
                if let Some(h) = explicit_h {
                    let v = p / 100.0 * h;
                    sizes[i] = v;
                    remaining -= v;
                } else {
                    sizes[i] = auto_height(i, children);
                }
            }
            TrackSize::Auto => {
                sizes[i] = auto_height(i, children);
                remaining -= sizes[i];
            }
            TrackSize::Fr(f) => {
                if definite {
                    sum_fr += *f;
                } else {
                    // Indefinite height: fr behaves like auto.
                    sizes[i] = auto_height(i, children);
                }
            }
        }
    }

    if definite && sum_fr > 0.0 {
        let free = remaining.max(0.0);
        let denom = sum_fr.max(1.0);
        for (i, ts) in list.iter().enumerate() {
            if let TrackSize::Fr(f) = ts {
                sizes[i] = free * (f / denom);
            }
        }
    }

    // Content distribution (align-content) of leftover space, only when the
    // container height is definite (else rows exactly fill, extra == 0) (§2.3).
    let used = sizes.iter().sum::<f32>() + gap * (rows.saturating_sub(1) as f32);
    let extra = explicit_h.map(|h| h - used).unwrap_or(0.0);
    let mut t = Tracks { sizes, offsets: Vec::new(), gap };
    t.build_offsets_distributed(self_style.align_content, extra);
    t
}
