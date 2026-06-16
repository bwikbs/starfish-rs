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
    AlignItems, AlignSelf, AutoRepeatKind, ComputedStyle, GridAutoRepeat, GridLine, GridPlacement,
    JustifyContent, Length, MinMaxSize, StyledTree, TrackSize,
};

use crate::block::{content_from_specified, layout_block, resolve, resolve_or_zero};
use crate::boxtree::{is_normal_flow, style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::cache::{LayoutCache, MeasureKind};
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
#[allow(clippy::too_many_arguments)]
/// E50-M1: resolve a `MinMaxSize` bound to a pixel floor/cap. `Fr`/`Auto` have
/// no intrinsic px size here (their flexing is handled by the fr phase), so
/// they resolve to 0 — used as the min floor or, for a fixed max, the cap.
fn minmax_size_px(s: MinMaxSize, basis: f32) -> f32 {
    match s {
        MinMaxSize::Px(v) => v,
        MinMaxSize::Percent(p) => p / 100.0 * basis,
        // E50-M2: intrinsic-keyword minmax bounds are not content-measured here
        // (the standalone intrinsic tracks are); they resolve to 0 (MVP — noted).
        MinMaxSize::Fr(_) | MinMaxSize::Auto | MinMaxSize::MinContent | MinMaxSize::MaxContent => {
            0.0
        }
    }
}

/// E50-M1: the fr weight a `minmax(..)` track contributes when its max is
/// flexible (`fr`/`auto` ≈ 1fr). A fixed (px/%) max contributes none.
fn minmax_fr_weight(max: MinMaxSize) -> Option<f32> {
    match max {
        MinMaxSize::Fr(f) => Some(f),
        MinMaxSize::Auto => Some(1.0),
        // E50-M2: intrinsic-keyword max bound ≈ auto (≈1fr) for MVP flexing.
        MinMaxSize::MinContent | MinMaxSize::MaxContent => Some(1.0),
        MinMaxSize::Px(_) | MinMaxSize::Percent(_) => None,
    }
}

/// E50-M1: distribute `free` px across flexible tracks honouring per-track min
/// floors. `flex[i] = Some((weight, floor))` for a flexible track (plain `fr` →
/// `(f, 0)`, `auto` → `(1, 0)`, fr-max `minmax` → `(weight, min_px)`); `None`
/// for a non-flexible track (left untouched). Tracks whose proportional share
/// would fall below their floor are locked at the floor and removed from the
/// pool, then the remainder is re-shared — a bounded single algorithm rather
/// than the full track-sizing iteration. Writes resolved px into `sizes`.
fn distribute_flex(sizes: &mut [f32], flex: &[Option<(f32, f32)>], free: f32) {
    let mut free = free.max(0.0);
    let mut locked = vec![false; flex.len()];
    loop {
        let mut sum_w = 0.0f32;
        for (i, fl) in flex.iter().enumerate() {
            if let Some((w, _)) = fl {
                if !locked[i] {
                    sum_w += *w;
                }
            }
        }
        if sum_w <= 0.0 {
            break;
        }
        let unit = free / sum_w;
        // Lock the worst (largest) floor violation this round; loop re-shares.
        let mut worst: Option<(usize, f32)> = None;
        for (i, fl) in flex.iter().enumerate() {
            if let (Some((w, floor)), false) = (fl, locked[i]) {
                if unit * w < *floor {
                    let deficit = *floor - unit * w;
                    if worst.is_none_or(|(_, d)| deficit > d) {
                        worst = Some((i, deficit));
                    }
                }
            }
        }
        match worst {
            Some((i, _)) => {
                let floor = flex[i].unwrap().1;
                sizes[i] = floor;
                free = (free - floor).max(0.0);
                locked[i] = true;
            }
            None => {
                for (i, fl) in flex.iter().enumerate() {
                    if let (Some((w, _)), false) = (fl, locked[i]) {
                        sizes[i] = unit * w;
                    }
                }
                break;
            }
        }
    }
}

/// Distribute `content_w` across the column tracks (E31-M2 masonry): px/% are
/// fixed, fr/auto share the remainder (auto ≈ 1fr). Empty list ⇒ one full column.
fn distribute_columns(tracks: &[TrackSize], content_w: f32, col_gap: f32) -> Vec<f32> {
    let n = tracks.len().max(1);
    if tracks.is_empty() {
        return vec![content_w];
    }
    let avail = (content_w - col_gap * (n as f32 - 1.0)).max(0.0);
    let mut widths = vec![0.0f32; n];
    let mut fixed = 0.0;
    // E50-M1: flex pool entry per track (weight, floor); None = non-flexible.
    let mut flex: Vec<Option<(f32, f32)>> = vec![None; n];
    for (i, t) in tracks.iter().enumerate() {
        match t {
            TrackSize::Px(v) => {
                widths[i] = *v;
                fixed += *v;
            }
            TrackSize::Percent(p) => {
                widths[i] = p / 100.0 * content_w;
                fixed += widths[i];
            }
            TrackSize::Fr(f) => flex[i] = Some((*f, 0.0)),
            TrackSize::Auto => flex[i] = Some((1.0, 0.0)),
            // E50-M1: fixed-max minmax reserves clamp(max, >=min) as fixed;
            // fr-max minmax joins the flex pool with the min as its floor.
            TrackSize::MinMax(min, max) => {
                let min_px = minmax_size_px(*min, content_w);
                match minmax_fr_weight(*max) {
                    Some(w) => flex[i] = Some((w, min_px)),
                    None => {
                        let v = minmax_size_px(*max, content_w).max(min_px);
                        widths[i] = v;
                        fixed += v;
                    }
                }
            }
            // E50-M2: masonry has no per-column item set to measure intrinsic
            // tracks against, so they behave like auto (≈1fr) here.
            TrackSize::MinContent | TrackSize::MaxContent | TrackSize::FitContent(_) => {
                flex[i] = Some((1.0, 0.0))
            }
        }
    }
    let fr_space = (avail - fixed).max(0.0);
    distribute_flex(&mut widths, &flex, fr_space);
    widths
}

/// Masonry layout (E31-M2): place each item into the shortest column so far.
#[allow(clippy::too_many_arguments)]
fn layout_masonry(
    b: &mut LayoutBox,
    content_x: f32,
    content_y: f32,
    content_w: f32,
    col_gap: f32,
    row_gap: f32,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let widths = distribute_columns(&self_style.grid_template_columns, content_w, col_gap);
    let n = widths.len();
    let mut col_x = vec![0.0f32; n];
    let mut acc = 0.0;
    for i in 0..n {
        col_x[i] = acc;
        acc += widths[i] + col_gap;
    }
    let mut children = std::mem::take(&mut b.children);
    let mut col_h = vec![0.0f32; n];
    for item in &mut children {
        // Pick the shortest column (ties → leftmost).
        let c = (0..n).fold(0, |best, i| if col_h[i] < col_h[best] { i } else { best });
        let cb = Dimensions {
            content: Rect {
                x: content_x + col_x[c],
                y: content_y + col_h[c],
                width: widths[c],
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats, cache);
        col_h[c] += item.dimensions.margin_box().height + row_gap;
    }
    b.children = children;
    let max_h = col_h.iter().cloned().fold(0.0_f32, f32::max);
    (max_h - row_gap).max(0.0)
}

#[allow(clippy::too_many_arguments)]
/// E50-M3: the fixed px size used to compute the auto-repeat count, for a single
/// auto-repeat track. `None` if the track has no fixed size (fr/auto/intrinsic),
/// in which case auto-repeat degenerates to a single track.
fn autorepeat_fixed_px(track: TrackSize, basis: f32) -> Option<f32> {
    match track {
        TrackSize::Px(v) => Some(v),
        TrackSize::Percent(p) => Some(p / 100.0 * basis),
        TrackSize::FitContent(v) => Some(v),
        // minmax with a fixed (px/%) min — size the fitting on that floor.
        TrackSize::MinMax(min, _) => match min {
            MinMaxSize::Px(v) => Some(v),
            MinMaxSize::Percent(p) => Some(p / 100.0 * basis),
            _ => None,
        },
        TrackSize::Fr(_) | TrackSize::Auto | TrackSize::MinContent | TrackSize::MaxContent => None,
    }
}

/// E50-M3: number of auto-repeat tracks that fit in `avail` given a per-track
/// fixed size `track_px` and inter-track `gap`. `floor((avail + gap) / (track +
/// gap))`, clamped to ≥1. A non-positive track size → 1 (avoid div-by-zero).
fn autorepeat_count(avail: f32, track_px: f32, gap: f32) -> usize {
    if track_px + gap <= 0.0 {
        return 1;
    }
    let n = ((avail + gap) / (track_px + gap)).floor() as i32;
    n.max(1) as usize
}

/// E50-M3: expand an `Option<GridAutoRepeat>` against a fixed track `list`,
/// returning the effective track list (fixed tracks then the repeated pattern)
/// or `None` when there is no auto-repeat (use the list as-is, byte-identical).
/// `avail` is the inner size on that axis (container minus the running gaps is
/// approximated using the fixed-track footprint).
fn expand_autorepeat(
    list: &[TrackSize],
    ar: Option<GridAutoRepeat>,
    container_size: f32,
    gap: f32,
) -> Option<Vec<TrackSize>> {
    let ar = ar?;
    let mut out: Vec<TrackSize> = list.to_vec();
    // Space already consumed by the fixed leading tracks (their footprint is a
    // rough px estimate; fr/auto count as 0). Subtract from the container size.
    let mut used = 0.0f32;
    for &t in list {
        if let Some(px) = autorepeat_fixed_px(t, container_size) {
            used += px + gap;
        }
    }
    let avail = (container_size - used).max(0.0);
    let count = match autorepeat_fixed_px(ar.track, container_size) {
        Some(px) => autorepeat_count(avail, px, gap),
        None => 1,
    };
    for _ in 0..count {
        out.push(ar.track);
    }
    Some(out)
}

pub(crate) fn layout_grid(
    b: &mut LayoutBox,
    containing: Dimensions,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let content_x = b.dimensions.content.x;
    let content_y = b.dimensions.content.y;
    let content_w = b.dimensions.content.width;
    let col_gap = resolve_or_zero(&self_style.column_gap, content_w);
    let row_gap = resolve_or_zero(&self_style.row_gap, content_w);
    // E31-M2: masonry packs items by shortest column instead of forming rows.
    if self_style.grid_masonry_rows {
        return layout_masonry(
            b, content_x, content_y, content_w, col_gap, row_gap, self_style, styled, doc, m,
            images, cache,
        );
    }
    let explicit_h = resolve(&self_style.height, containing.content.height);

    // E50-M3: expand `repeat(auto-fill|auto-fit, <track>)` against the container
    // size into a concrete track list. Only allocates a style clone when an
    // auto-repeat is actually present; otherwise `self_style` is used verbatim,
    // so non-auto-repeat grids lay out byte-identically.
    let eff_cols = expand_autorepeat(
        &self_style.grid_template_columns,
        self_style.grid_template_columns_autorepeat,
        content_w,
        col_gap,
    );
    let eff_rows = expand_autorepeat(
        &self_style.grid_template_rows,
        self_style.grid_template_rows_autorepeat,
        explicit_h.unwrap_or(0.0),
        row_gap,
    );
    // E50-M3: for `auto-fit` columns, the auto-repeat tracks start at this index
    // (after the fixed leading tracks); empty ones collapse to 0 after placement.
    let col_autofit_from: Option<usize> = match self_style.grid_template_columns_autorepeat {
        Some(ar) if ar.kind == AutoRepeatKind::AutoFit => {
            Some(self_style.grid_template_columns.len())
        }
        _ => None,
    };
    let mut eff_style_owned = None;
    if eff_cols.is_some() || eff_rows.is_some() {
        let mut s = self_style.clone();
        if let Some(c) = eff_cols {
            s.grid_template_columns = c;
        }
        if let Some(r) = eff_rows {
            s.grid_template_rows = r;
        }
        eff_style_owned = Some(s);
    }
    let self_style: &ComputedStyle = eff_style_owned.as_ref().unwrap_or(self_style);

    // E31-M3: a subgrid adopts the parent's spanned column widths (injected by
    // the parent into `subgrid_cols`) instead of sizing its own columns.
    let subgrid_w = b.subgrid_cols.take();

    // Column count: subgrid → adopted track count; else explicit columns, or
    // one implicit full-width column.
    let cols = match &subgrid_w {
        Some(w) if !w.is_empty() => w.len(),
        _ => self_style.grid_template_columns.len().max(1),
    };

    let mut children = std::mem::take(&mut b.children);

    // --- Place items (§4). ---
    let placed = place_items(&children, styled, self_style, cols);

    // Total row count (explicit + implicit), at least the explicit rows.
    let implicit_rows = placed.iter().map(|p| p.row_end).max().unwrap_or(0);
    let rows = self_style.grid_template_rows.len().max(implicit_rows);

    // --- Size columns (§3.3). ---
    let mut cols_tracks = match subgrid_w {
        Some(w) if !w.is_empty() => {
            let mut t = Tracks {
                sizes: w,
                offsets: Vec::new(),
                gap: col_gap,
            };
            t.build_offsets();
            t
        }
        _ => size_columns(
            self_style, content_w, col_gap, cols, &placed, &mut children, styled, doc, m, images,
            cache,
        ),
    };

    // E50-M3: `auto-fit` — collapse auto-repeat columns that received no item.
    // Trailing empties are truncated (gap removed); interior empties zero out
    // their width (MVP keeps their gap). A placed item's `col_start` marks the
    // start track it occupies.
    if let Some(from) = col_autofit_from {
        let mut has_item = vec![false; cols_tracks.sizes.len()];
        for p in &placed {
            for c in p.col_start..p.col_end.min(has_item.len()) {
                has_item[c] = true;
            }
        }
        // Truncate trailing empty auto-fit tracks.
        let mut end = cols_tracks.sizes.len();
        while end > from && !has_item[end - 1] {
            end -= 1;
        }
        cols_tracks.sizes.truncate(end);
        // Zero interior empty auto-fit tracks.
        for (sz, occupied) in cols_tracks
            .sizes
            .iter_mut()
            .zip(has_item.iter())
            .skip(from)
        {
            if !occupied {
                *sz = 0.0;
            }
        }
        cols_tracks.build_offsets();
    }

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
        cache,
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
        // E31-M3: if this item is a subgrid, hand it the parent column widths it
        // spans so its own grid pass adopts them.
        if p.style.subgrid_columns {
            // col_start/col_end are 0-based track indices ([start, end)).
            let lo = p.col_start.min(cols_tracks.sizes.len());
            let hi = p.col_end.min(cols_tracks.sizes.len());
            if lo < hi {
                item.subgrid_cols = Some(cols_tracks.sizes[lo..hi].to_vec());
            }
        }
        let cb = Dimensions {
            content: Rect {
                x: abs_x,
                y: abs_y,
                width: area_w,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats, cache);

        // Pin margins tight (auto → 0).
        item.dimensions.margin.left = resolve_or_zero(&s.margin_left, area_w);
        item.dimensions.margin.right = resolve_or_zero(&s.margin_right, area_w);
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
            let intrinsic = measure_item_width(item, area_w, styled, doc, m, images, cache);
            intrinsic.min((area_w - hbpm).max(0.0))
        };

        // Block axis (height): stretch fills the cell; else the natural height at
        // the chosen width (measured lazily, keeping the stretch path cheap).
        let content_h = if align == AlignItems::Stretch {
            (area_h - vbpm).max(0.0)
        } else {
            let natural_h = measure_item_height(item, content_w, styled, doc, m, images, cache);
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
    let dense = self_style.grid_auto_flow_dense; // E50-M3

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

        pendings.push(Pending {
            idx,
            style: cstyle,
            col,
            row,
        });
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
                    let fits =
                        (band..be).all(|rr| (c..c + cspan).all(|cc| cell_free(&occupied, rr, cc)));
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
            // Fully auto: row-major scan. Sparse (default) starts from the
            // running cursor; `dense` (E50-M3) restarts each item from the
            // origin so a small later item backfills an earlier hole.
            let (mut r, mut c) = if dense { (0, 0) } else { (cur_row, cur_col) };
            loop {
                if c + cspan > cols {
                    r += 1;
                    c = 0;
                    continue;
                }
                ensure_row(&mut occupied, r + rspan.saturating_sub(1));
                let fits =
                    (r..r + rspan).all(|rr| (c..c + cspan).all(|cc| cell_free(&occupied, rr, cc)));
                if fits {
                    break;
                }
                c += 1;
            }
            cs = c;
            ce = c + cspan;
            rs = r;
            re = r + rspan;
            // Sparse mode advances the cursor past the placed item; dense leaves
            // the cursor untouched (each item re-scans from the origin).
            if !dense {
                cur_row = r;
                cur_col = ce;
                if cur_col >= cols {
                    cur_row += 1;
                    cur_col = 0;
                }
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
    if matches!(
        b.kind,
        BoxKind::TextRun
            | BoxKind::Image
            | BoxKind::Svg
            | BoxKind::InlineBlock
            | BoxKind::FormControl
            | BoxKind::Media
            | BoxKind::Canvas
    ) {
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
#[allow(clippy::too_many_arguments)]
fn measure_item_width(
    item: &mut LayoutBox,
    avail: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let s = style_of(styled, item);
    if let Length::Px(v) = s.width {
        // Content-box: `width:v` is the content width → `v` (byte-identical to
        // pre-E13). Border-box: `width:v` is the border-box width, so the content
        // width is `v - (horizontal padding+border)`.
        let pb = s.border_left_width
            + s.border_right_width
            + resolve_or_zero(&s.padding_left, avail)
            + resolve_or_zero(&s.padding_right, avail);
        return content_from_specified(v.max(0.0), s.box_sizing, pb);
    }
    // E12-M1: memoize the layout-pass per (node, GridWidth, avail). Anonymous
    // boxes (NodeId is a shared parent ref) bypass the cache.
    let node = match item.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: avail,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats, cache);
        intrinsic_width(item, item.dimensions.content.x)
    };
    match node {
        Some(node) => cache.measure(node, MeasureKind::GridWidth, avail, compute),
        None => compute(),
    }
}

/// E50-M2: measure an item's min-content width — the widest unbreakable
/// fragment. Lay the item out at width 0 to force maximal wrapping, then take
/// the widest leaf right edge. An explicit-width box yields its box width (its
/// min == max content), giving deterministic intrinsic columns in tests.
#[allow(clippy::too_many_arguments)]
fn measure_item_min_content(
    item: &mut LayoutBox,
    avail: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let s = style_of(styled, item);
    if let Length::Px(v) = s.width {
        let pb = s.border_left_width
            + s.border_right_width
            + resolve_or_zero(&s.padding_left, avail)
            + resolve_or_zero(&s.padding_right, avail);
        return content_from_specified(v.max(0.0), s.box_sizing, pb);
    }
    let node = match item.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats, cache);
        intrinsic_width(item, item.dimensions.content.x)
    };
    match node {
        // Cache key folds avail in (its only effect here is padding %).
        Some(node) => cache.measure(node, MeasureKind::GridMinContent, avail, compute),
        None => compute(),
    }
}

/// Measure an item's content height by laying it out at `width`.
#[allow(clippy::too_many_arguments)]
fn measure_item_height(
    item: &mut LayoutBox,
    width: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    // E12-M1: memoize per (node, GridHeight, width); anonymous boxes bypass.
    let node = match item.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: 0.0,
            },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(item, cb, styled, doc, m, images, &mut floats, cache);
        item.dimensions.content.height
    };
    match node {
        Some(node) => cache.measure(node, MeasureKind::GridHeight, width, compute),
        None => compute(),
    }
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
    cache: &LayoutCache,
) -> Tracks {
    let list = &self_style.grid_template_columns;
    let total_gap = gap * (cols.saturating_sub(1) as f32);
    let avail = (container_w - total_gap).max(0.0);

    let mut sizes = vec![0.0f32; cols];

    // Empty template → single implicit auto column filling the width.
    if list.is_empty() {
        sizes[0] = avail;
        let mut t = Tracks {
            sizes,
            offsets: Vec::new(),
            gap,
        };
        t.build_offsets();
        return t;
    }

    // E50-M2: max over single-column items in column `i` of their max-content
    // width. Multi-span items contribute to no column's intrinsic size (MVP).
    let col_max_content = |i: usize, children: &mut [LayoutBox]| -> f32 {
        let mut w = 0.0f32;
        for p in placed {
            if p.col_start == i && p.col_end == i + 1 {
                w = w.max(measure_item_width(
                    &mut children[p.idx],
                    avail,
                    styled,
                    doc,
                    m,
                    images,
                    cache,
                ));
            }
        }
        w
    };
    // E50-M2: same, but each item's min-content width.
    let col_min_content = |i: usize, children: &mut [LayoutBox]| -> f32 {
        let mut w = 0.0f32;
        for p in placed {
            if p.col_start == i && p.col_end == i + 1 {
                w = w.max(measure_item_min_content(
                    &mut children[p.idx],
                    avail,
                    styled,
                    doc,
                    m,
                    images,
                    cache,
                ));
            }
        }
        w
    };

    // First pass: px + percent; mark auto/fr.
    let mut remaining = avail;
    let mut sum_fr = 0.0f32;
    // E50-M1: flex pool entry per track (weight, floor); None = non-flexible.
    let mut flex: Vec<Option<(f32, f32)>> = vec![None; cols];
    for i in 0..cols {
        match list[i] {
            TrackSize::Px(v) => {
                sizes[i] = v;
                remaining -= v;
            }
            TrackSize::Percent(p) => {
                let v = p / 100.0 * container_w;
                sizes[i] = v;
                remaining -= v;
            }
            TrackSize::Fr(f) => {
                sum_fr += f;
                flex[i] = Some((f, 0.0));
            }
            TrackSize::Auto => {}
            // E50-M1: fixed-max minmax reserves clamp(max, >=min); fr-max minmax
            // joins the flex pool below with the min as its floor.
            TrackSize::MinMax(min, max) => {
                let min_px = minmax_size_px(min, container_w);
                match minmax_fr_weight(max) {
                    Some(w) => {
                        sum_fr += w;
                        flex[i] = Some((w, min_px));
                    }
                    None => {
                        let v = minmax_size_px(max, container_w).max(min_px);
                        sizes[i] = v;
                        remaining -= v;
                    }
                }
            }
            // E50-M2: intrinsic tracks reserve their content-derived width as
            // fixed BEFORE fr distribution (like Px tracks).
            TrackSize::MaxContent => {
                let v = col_max_content(i, children);
                sizes[i] = v;
                remaining -= v;
            }
            TrackSize::MinContent => {
                let v = col_min_content(i, children);
                sizes[i] = v;
                remaining -= v;
            }
            // fit-content(L) = max(min-content, min(max-content, L)).
            TrackSize::FitContent(limit) => {
                let mn = col_min_content(i, children);
                let mx = col_max_content(i, children);
                let v = mn.max(mx.min(limit));
                sizes[i] = v;
                remaining -= v;
            }
        }
    }

    // Auto columns: max content width of single-column items placed in them.
    for i in 0..cols {
        if !matches!(list[i], TrackSize::Auto) {
            continue;
        }
        let mut max_w = 0.0f32;
        for p in placed {
            if p.col_start == i && p.col_end == i + 1 {
                let w =
                    measure_item_width(&mut children[p.idx], avail, styled, doc, m, images, cache);
                max_w = max_w.max(w);
            }
        }
        sizes[i] = max_w;
        remaining -= max_w;
    }

    // Fr (and fr-max minmax) columns distribute the remainder, honouring floors.
    if sum_fr > 0.0 {
        distribute_flex(&mut sizes, &flex, remaining.max(0.0));
    }

    // Content distribution (justify-content) of any leftover space (§2.3). With
    // fr tracks present `extra ≈ 0`, so this is a natural no-op.
    let used = sizes.iter().sum::<f32>() + gap * (cols.saturating_sub(1) as f32);
    let extra = container_w - used;
    let mut t = Tracks {
        sizes,
        offsets: Vec::new(),
        gap,
    };
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
    cache: &LayoutCache,
) -> Tracks {
    // Build the row track-size list: explicit list extended with Auto.
    let mut list: Vec<TrackSize> = self_style.grid_template_rows.clone();
    while list.len() < rows {
        list.push(TrackSize::Auto);
    }

    let mut sizes = vec![0.0f32; rows];
    if rows == 0 {
        let mut t = Tracks {
            sizes,
            offsets: Vec::new(),
            gap,
        };
        t.build_offsets();
        return t;
    }

    let definite = explicit_h.is_some();
    let total_gap = gap * (rows.saturating_sub(1) as f32);
    let avail = explicit_h.map(|h| (h - total_gap).max(0.0)).unwrap_or(0.0);

    let mut remaining = avail;
    let mut sum_fr = 0.0f32;
    // E50-M1: flex pool entry per track (weight, floor); None = non-flexible.
    let mut flex: Vec<Option<(f32, f32)>> = vec![None; rows];

    // Helper: max content height of items in row `r` at their column-span width.
    let auto_height = |r: usize, children: &mut [LayoutBox]| -> f32 {
        let mut max_h = 0.0f32;
        for p in placed {
            if p.row_start == r && p.row_end == r + 1 {
                let w = cols_tracks.span_extent(p.col_start, p.col_end);
                let h = measure_item_height(&mut children[p.idx], w, styled, doc, m, images, cache);
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
                    flex[i] = Some((*f, 0.0));
                } else {
                    // Indefinite height: fr behaves like auto.
                    sizes[i] = auto_height(i, children);
                }
            }
            // E50-M1: fixed-max minmax reserves clamp(max, >=min). fr-max minmax
            // joins the flex pool (definite height) with the min as its floor;
            // when indefinite it falls back to content sizing floored at min.
            TrackSize::MinMax(min, max) => {
                let min_px = minmax_size_px(*min, explicit_h.unwrap_or(0.0));
                match minmax_fr_weight(*max) {
                    Some(w) => {
                        if definite {
                            sum_fr += w;
                            flex[i] = Some((w, min_px));
                        } else {
                            sizes[i] = auto_height(i, children).max(min_px);
                            remaining -= sizes[i];
                        }
                    }
                    None => {
                        let v = minmax_size_px(*max, explicit_h.unwrap_or(0.0)).max(min_px);
                        sizes[i] = v;
                        remaining -= v;
                    }
                }
            }
            // E50-M2: on the block axis min-content/max-content collapse to the
            // row's content height (== auto); fit-content(L) caps it at L.
            TrackSize::MinContent | TrackSize::MaxContent => {
                sizes[i] = auto_height(i, children);
                remaining -= sizes[i];
            }
            TrackSize::FitContent(limit) => {
                sizes[i] = auto_height(i, children).min(*limit);
                remaining -= sizes[i];
            }
        }
    }

    if definite && sum_fr > 0.0 {
        distribute_flex(&mut sizes, &flex, remaining.max(0.0));
    }

    // Content distribution (align-content) of leftover space, only when the
    // container height is definite (else rows exactly fill, extra == 0) (§2.3).
    let used = sizes.iter().sum::<f32>() + gap * (rows.saturating_sub(1) as f32);
    let extra = explicit_h.map(|h| h - used).unwrap_or(0.0);
    let mut t = Tracks {
        sizes,
        offsets: Vec::new(),
        gap,
    };
    t.build_offsets_distributed(self_style.align_content, extra);
    t
}
