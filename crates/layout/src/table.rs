//! Table layout (E7-M3): a parallel layout mode dispatched from `layout_block`
//! when a box's `display` is `table` / `inline-table`. See `docs/design/E7-M3.md`.
//!
//! The container's width/position is already resolved by `layout_block` before
//! `layout_table` is called. This module walks the table's box subtree to collect
//! rows×cells (with `colspan`/`rowspan`), places the cells into an occupancy grid
//! (mirroring `grid::place_items`), sizes columns by a pragmatic preferred-width
//! algorithm, sizes rows from cell content, positions each cell absolutely via
//! `translate_box` honouring `border-spacing`, sizes the row boxes (for row
//! background paint), and returns the table's content-box height.

use starfish_dom::Document;
use starfish_style::{BoxSizing, ComputedStyle, Display, Length, StyledTree};

use crate::block::{layout_block, resolve};
use crate::boxtree::{is_normal_flow, style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::cache::{LayoutCache, MeasureKind};
use crate::dimensions::{Dimensions, EdgeSizes, Rect};
use crate::float::FloatContext;
use crate::inline::translate_box;
use crate::measure::{ImageSource, TextMeasurer};

/// Access route to a cell box nested under the table tree. `group` is the index
/// of the owning row-group child of the table (None = directly under the table),
/// `row` the index of the row within that container (None = a synthetic row of
/// direct cells), `cell` the index of the cell within its row container.
#[derive(Clone, Copy)]
struct CellPath {
    group: Option<usize>,
    row: Option<usize>,
    cell: usize,
}

/// Access route to a row's own box (for sizing it, §3.7). `None` for synthetic
/// (anonymous) rows that have no box to size.
#[derive(Clone, Copy)]
struct RowPath {
    group: Option<usize>,
    row: Option<usize>,
}

/// One placed cell: its grid position (0-based, end-exclusive) + its access path
/// and resolved style.
struct Cell {
    path: CellPath,
    col_start: usize,
    col_end: usize,
    row_start: usize,
    row_end: usize,
    style: ComputedStyle,
}

/// A collected row: the access path to its box and the indices (into the cell
/// list) of the cells declared in it, in document order.
struct Row {
    box_path: RowPath,
    cells: Vec<RawCell>,
}

/// A cell before placement: its path + style + spans.
struct RawCell {
    path: CellPath,
    style: ComputedStyle,
    colspan: usize,
    rowspan: usize,
}

/// Maximum nesting depth for the table algorithm. Each cell is laid out via
/// `layout_block` up to three times (column measure, row measure, final) and each
/// pass walks the cell's whole subtree, so a deeply nested table-in-table-in-...
/// stack-overflows much shallower than plain blocks. Beyond this depth we fall
/// back to plain block layout to degrade gracefully instead of aborting. The
/// bound is kept conservative so even a 2 MiB worker stack survives; shallow
/// (real-world) tables are unaffected.
const MAX_TABLE_NESTING: usize = 6;

thread_local! {
    static TABLE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Lay out a table container's rows/cells. The container's width/position are
/// already resolved by `layout_block`. Returns the table's content-box height.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_table(
    b: &mut LayoutBox,
    containing: Dimensions,
    self_style: &ComputedStyle,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let depth = TABLE_DEPTH.with(|d| d.get());
    if depth >= MAX_TABLE_NESTING {
        // Too deeply nested: skip the table algorithm and lay the box out as a
        // plain block so we degrade gracefully rather than overflow the stack.
        let mut floats = FloatContext::default();
        crate::block::layout_block_children(
            b, self_style, containing, styled, doc, m, images, &mut floats, cache,
        );
        return b.dimensions.content.height;
    }
    TABLE_DEPTH.with(|d| d.set(depth + 1));
    let result = layout_table_inner(b, containing, self_style, styled, doc, m, images, cache);
    TABLE_DEPTH.with(|d| d.set(depth));
    result
}

#[allow(clippy::too_many_arguments)]
fn layout_table_inner(
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
    let table_content_w = b.dimensions.content.width;
    let (h_space, v_space) = self_style.border_spacing;
    let definite_width = !matches!(self_style.width, Length::Auto);
    let explicit_h = resolve(self_style.height, containing.content.height);

    let mut children = std::mem::take(&mut b.children);

    // --- (A) walk → rows of raw cells (§3.1). ---
    let rows = collect_rows(&children, styled, doc);

    // --- (B) occupancy-grid placement → per-cell ranges + column count (§3.2). ---
    let (cells, cols, n_rows) = place_cells(&rows);

    // --- (C) size columns (§3.3). ---
    let col_w = size_columns(
        &cells,
        cols,
        table_content_w,
        h_space,
        definite_width,
        &mut children,
        styled,
        doc,
        m,
        images,
        cache,
    );

    // Re-pin auto table width to shrink-to-fit (§3.3).
    let total_col_w: f32 = col_w.iter().sum();
    if !definite_width {
        b.dimensions.content.width = total_col_w + h_space * (cols as f32 + 1.0);
    }

    // Column x-offsets (content-relative): outer spacing + interior spacing.
    let x_off = col_offsets(&col_w, h_space);

    // --- (D) size rows (§3.5). ---
    let row_h = size_rows(
        &cells, &rows, cols, n_rows, &col_w, &x_off, h_space, v_space, &mut children, styled, doc,
        m, images, cache,
    );

    // Row y-offsets (content-relative).
    let y_off = row_offsets(&row_h, v_space);

    // --- (E) lay out + position cells; size row boxes (§3.7). ---
    for c in &cells {
        let cell_x = content_x + x_off[c.col_start];
        let cell_y = content_y + y_off[c.row_start];
        let cell_w = span_extent(&col_w, c.col_start, c.col_end, h_space);
        let cell_h = span_extent(&row_h, c.row_start, c.row_end, v_space);

        let Some(cell) = cell_mut(&mut children, &c.path) else {
            continue;
        };
        let cb = Dimensions {
            content: Rect { x: cell_x, y: cell_y, width: cell_w, height: 0.0 },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(cell, cb, styled, doc, m, images, &mut floats, cache);

        // Cells ignore margins; pin content so the border box fills the slot.
        let hbp = cell.dimensions.border.left
            + cell.dimensions.border.right
            + cell.dimensions.padding.left
            + cell.dimensions.padding.right;
        let vbp = cell.dimensions.border.top
            + cell.dimensions.border.bottom
            + cell.dimensions.padding.top
            + cell.dimensions.padding.bottom;
        cell.dimensions.margin = EdgeSizes::default();
        cell.dimensions.content.width = (cell_w - hbp).max(0.0);
        cell.dimensions.content.height = (cell_h - vbp).max(0.0);
        let cur = cell.dimensions.border_box();
        translate_box(cell, cell_x - cur.x, cell_y - cur.y);
    }

    // Size the row boxes so their backgrounds/borders paint at the right extent.
    let total_columns_width = if cols > 0 {
        total_col_w + h_space * (cols.saturating_sub(1) as f32)
    } else {
        0.0
    };
    for (r, row) in rows.iter().enumerate() {
        if let Some(rb) = row_mut(&mut children, &row.box_path) {
            let h = row_h.get(r).copied().unwrap_or(0.0);
            rb.dimensions.margin = EdgeSizes::default();
            rb.dimensions.padding = EdgeSizes::default();
            rb.dimensions.border = EdgeSizes::default();
            rb.dimensions.content = Rect {
                x: content_x + x_off.first().copied().unwrap_or(h_space),
                y: content_y + y_off[r],
                width: total_columns_width,
                height: h,
            };
        }
    }

    b.children = children;

    let rows_h = row_h.iter().sum::<f32>() + v_space * (n_rows as f32 + 1.0);
    explicit_h.unwrap_or(rows_h)
}

/// Walk the table's children collecting rows. A `table-row-group` is descended
/// into for its `table-row` children; a direct `table-row` is collected as its
/// own row; direct `table-cell`s (no row) are gathered into one synthetic row
/// (§2.3). Non-cell/row content is skipped.
fn collect_rows(children: &[LayoutBox], styled: &StyledTree, doc: &Document) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut pending_direct_cells: Vec<RawCell> = Vec::new();

    for (gi, child) in children.iter().enumerate() {
        let cstyle = style_of(styled, child);
        if !is_normal_flow(&cstyle) {
            continue;
        }
        match cstyle.display {
            Display::TableRowGroup => {
                let mut group_rows = collect_group_rows(child, Some(gi), styled, doc);
                if group_rows.is_empty() {
                    // Row-group with direct cells and no rows → one synthetic row.
                    let cells = collect_row_cells(child, CellPath { group: Some(gi), row: None, cell: 0 }, styled, doc);
                    if !cells.is_empty() {
                        rows.push(Row { box_path: RowPath { group: Some(gi), row: None }, cells });
                    }
                } else {
                    rows.append(&mut group_rows);
                }
            }
            Display::TableRow => {
                let cells = collect_row_cells(child, CellPath { group: None, row: Some(gi), cell: 0 }, styled, doc);
                rows.push(Row { box_path: RowPath { group: None, row: Some(gi) }, cells });
            }
            Display::TableCell => {
                let path = CellPath { group: None, row: None, cell: gi };
                pending_direct_cells.push(make_raw_cell(child, path, &cstyle, doc));
            }
            _ => {}
        }
    }

    if !pending_direct_cells.is_empty() {
        rows.push(Row { box_path: RowPath { group: None, row: None }, cells: pending_direct_cells });
    }
    rows
}

/// Collect the `table-row` rows inside a row-group box.
fn collect_group_rows(
    group: &LayoutBox,
    gi: Option<usize>,
    styled: &StyledTree,
    doc: &Document,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (ri, rchild) in group.children.iter().enumerate() {
        let rstyle = style_of(styled, rchild);
        if rstyle.display == Display::TableRow && is_normal_flow(&rstyle) {
            let cells = collect_row_cells(rchild, CellPath { group: gi, row: Some(ri), cell: 0 }, styled, doc);
            rows.push(Row { box_path: RowPath { group: gi, row: Some(ri) }, cells });
        }
    }
    rows
}

/// Collect the `table-cell` cells directly inside a row (or row-group treated as
/// a synthetic row). `proto` carries the group/row part of the path; the cell
/// index is filled per cell.
fn collect_row_cells(
    container: &LayoutBox,
    proto: CellPath,
    styled: &StyledTree,
    doc: &Document,
) -> Vec<RawCell> {
    let mut out = Vec::new();
    for (ci, cell) in container.children.iter().enumerate() {
        let cstyle = style_of(styled, cell);
        if cstyle.display == Display::TableCell && is_normal_flow(&cstyle) {
            let path = CellPath { group: proto.group, row: proto.row, cell: ci };
            out.push(make_raw_cell(cell, path, &cstyle, doc));
        }
    }
    out
}

/// Build a `RawCell` reading `colspan`/`rowspan` from the cell element.
fn make_raw_cell(cell: &LayoutBox, path: CellPath, style: &ComputedStyle, doc: &Document) -> RawCell {
    let node = cell.style.node();
    let colspan = span_attr(doc, node, "colspan");
    let rowspan = span_attr(doc, node, "rowspan");
    RawCell { path, style: style.clone(), colspan, rowspan }
}

/// Parse a `colspan`/`rowspan` attribute (default 1, clamp to `1..=1000`).
fn span_attr(doc: &Document, node: starfish_style::NodeId, name: &str) -> usize {
    doc.get_attribute(node, name)
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 1000)
}

/// Place cells into an occupancy grid, row by row left-to-right, honouring
/// colspan/rowspan (§3.2). Returns (placed cells, column count, row count).
fn place_cells(rows: &[Row]) -> (Vec<Cell>, usize, usize) {
    let n_rows = rows.len();
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let ensure = |occ: &mut Vec<Vec<bool>>, r: usize, want_cols: usize| {
        while occ.len() <= r {
            occ.push(Vec::new());
        }
        if occ[r].len() < want_cols {
            occ[r].resize(want_cols, false);
        }
    };
    let is_occ = |occ: &[Vec<bool>], r: usize, c: usize| -> bool {
        occ.get(r).and_then(|row| row.get(c).copied()).unwrap_or(false)
    };

    let mut placed: Vec<Cell> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut c = 0usize;
        for raw in &row.cells {
            // Skip slots already occupied by a rowspan from an earlier row.
            while is_occ(&occupied, r, c) {
                c += 1;
            }
            let col_start = c;
            let col_end = c + raw.colspan;
            let row_start = r;
            // Clamp the bottom of a rowspan to the table's actual row count: a
            // cell may declare more rows than exist (e.g. rowspan in the last
            // row). The occupancy grid below may grow past n_rows, but `row_h`
            // (sized to n_rows) is sliced with this `row_end`, so it must not
            // exceed the real row count.
            let row_end = (r + raw.rowspan).min(n_rows);
            for rr in row_start..row_end {
                ensure(&mut occupied, rr, col_end);
                for slot in occupied[rr].iter_mut().take(col_end).skip(col_start) {
                    *slot = true;
                }
            }
            placed.push(Cell {
                path: raw.path,
                col_start,
                col_end,
                row_start,
                row_end,
                style: raw.style.clone(),
            });
            c = col_end;
        }
    }

    let cols = placed.iter().map(|p| p.col_end).max().unwrap_or(0);
    (placed, cols, n_rows)
}

/// Column x-offsets (content-relative): `x_off[j]` = left edge of column j.
fn col_offsets(col_w: &[f32], h_space: f32) -> Vec<f32> {
    let mut offs = Vec::with_capacity(col_w.len());
    let mut x = h_space;
    for w in col_w {
        offs.push(x);
        x += w + h_space;
    }
    offs
}

/// Row y-offsets (content-relative).
fn row_offsets(row_h: &[f32], v_space: f32) -> Vec<f32> {
    let mut offs = Vec::with_capacity(row_h.len());
    let mut y = v_space;
    for h in row_h {
        offs.push(y);
        y += h + v_space;
    }
    offs
}

/// Sum of track sizes [start..end) + interior spacing (not the outer edges).
fn span_extent(sizes: &[f32], start: usize, end: usize, space: f32) -> f32 {
    let mut e = 0.0;
    for s in sizes.iter().take(end).skip(start) {
        e += *s;
    }
    if end > start + 1 {
        e += space * (end - start - 1) as f32;
    }
    e
}

/// Size columns (§3.3). Each single-column cell contributes its preferred width;
/// spanning cells widen narrow columns; then fit to the table width.
#[allow(clippy::too_many_arguments)]
fn size_columns(
    cells: &[Cell],
    cols: usize,
    table_content_w: f32,
    h_space: f32,
    definite_width: bool,
    children: &mut [LayoutBox],
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> Vec<f32> {
    if cols == 0 {
        return Vec::new();
    }
    let avail = (table_content_w - h_space * (cols as f32 + 1.0)).max(0.0);

    // Per-column preferred width from single-column cells.
    let mut pref = vec![0.0f32; cols];
    for c in cells {
        if c.col_end == c.col_start + 1 {
            let w = measure_cell_width(c, children, avail, styled, doc, m, images, cache);
            pref[c.col_start] = pref[c.col_start].max(w);
        }
    }

    // Spanning cells widen the narrow columns they cover (single pass).
    for c in cells {
        if c.col_end > c.col_start + 1 {
            let span = c.col_end - c.col_start;
            let interior = h_space * (span - 1) as f32;
            let cur: f32 = pref[c.col_start..c.col_end].iter().sum::<f32>() + interior;
            let cell_pref = measure_cell_width(c, children, avail, styled, doc, m, images, cache);
            if cell_pref > cur {
                let extra = (cell_pref - cur) / span as f32;
                for w in &mut pref[c.col_start..c.col_end] {
                    *w += extra;
                }
            }
        }
    }

    let sum_pref: f32 = pref.iter().sum();
    if definite_width {
        // Distribute to fill the available content width (grow or shrink).
        if sum_pref <= 0.0 {
            // No content → equal columns.
            return vec![avail / cols as f32; cols];
        }
        let scale = avail / sum_pref;
        pref.iter().map(|w| (w * scale).max(0.0)).collect()
    } else {
        // Auto table: shrink-to-fit (columns hug their content).
        pref
    }
}

/// Measure a cell's preferred content width (border+padding included). Explicit
/// `width:Npx` short-circuits.
#[allow(clippy::too_many_arguments)]
fn measure_cell_width(
    c: &Cell,
    children: &mut [LayoutBox],
    avail: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let (bp, explicit_w) = {
        let s = &c.style;
        let bp = s.border_left_width + s.border_right_width;
        // padding resolved against avail; auto/px only (percent → against avail).
        let pl = resolve(s.padding_left, avail).unwrap_or(0.0);
        let pr = resolve(s.padding_right, avail).unwrap_or(0.0);
        let explicit = match s.width {
            Length::Px(v) => Some(v.max(0.0)),
            _ => None,
        };
        (bp + pl + pr, explicit)
    };
    if let Some(w) = explicit_w {
        // Content-box: `width:w` is the content width → border-box = w + bp
        // (byte-identical to pre-E13). Border-box: `width:w` is already the
        // border-box width → return w (the cell content becomes w - bp inside
        // `layout_block`).
        return match c.style.box_sizing {
            BoxSizing::ContentBox => w + bp,
            BoxSizing::BorderBox => w,
        };
    }
    let Some(cell) = cell_mut(children, &c.path) else {
        return bp;
    };
    // E12-M1: memoize the layout pass per (cell node, TableCellWidth, inner avail).
    // Anonymous cell boxes (NodeId is a shared parent ref) bypass the cache.
    let avail_inner = (avail - bp).max(0.0);
    let node = match cell.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect { x: 0.0, y: 0.0, width: avail_inner, height: 0.0 },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(cell, cb, styled, doc, m, images, &mut floats, cache);
        // Extent from the cell's content origin to its rightmost leaf (grid's
        // `intrinsic_width` technique): the text's absolute right already includes
        // the border+padding offset, so measure relative to content.x.
        let origin = cell.dimensions.content.x;
        max_content_right(cell)
            .map(|r| (r - origin).max(0.0))
            .unwrap_or(cell.dimensions.content.width)
    };
    let content = match node {
        Some(node) => cache.measure(node, MeasureKind::TableCellWidth, avail_inner, compute),
        None => compute(),
    };
    content + bp
}

/// Size rows (§3.5). Each single-row cell's outer height contributes; rowspan
/// cells push their last spanned row to fit. Explicit row height is a floor.
#[allow(clippy::too_many_arguments)]
fn size_rows(
    cells: &[Cell],
    rows: &[Row],
    _cols: usize,
    n_rows: usize,
    col_w: &[f32],
    _x_off: &[f32],
    h_space: f32,
    v_space: f32,
    children: &mut [LayoutBox],
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> Vec<f32> {
    let mut row_h = vec![0.0f32; n_rows];

    // Single-row cells.
    for c in cells {
        if c.row_end == c.row_start + 1 {
            let cell_w = span_extent(col_w, c.col_start, c.col_end, h_space);
            let oh = measure_cell_outer_height(c, children, cell_w, styled, doc, m, images, cache);
            row_h[c.row_start] = row_h[c.row_start].max(oh);
        }
    }

    // Explicit row height floors.
    for (r, row) in rows.iter().enumerate() {
        if let Some(rb) = row_ref(children, &row.box_path) {
            let rs = style_of(styled, rb);
            if let Some(h) = resolve(rs.height, 0.0) {
                row_h[r] = row_h[r].max(h);
            }
        }
    }

    // Rowspan cells push their last spanned row to fit.
    for c in cells {
        if c.row_end > c.row_start + 1 {
            let span = c.row_end - c.row_start;
            let interior = v_space * (span - 1) as f32;
            let cur: f32 = row_h[c.row_start..c.row_end].iter().sum::<f32>() + interior;
            let cell_w = span_extent(col_w, c.col_start, c.col_end, h_space);
            let oh = measure_cell_outer_height(c, children, cell_w, styled, doc, m, images, cache);
            if oh > cur {
                row_h[c.row_end - 1] += oh - cur;
            }
        }
    }

    row_h
}

/// A cell's outer height = content height (laid out at `cell_w`) + vertical
/// border+padding.
#[allow(clippy::too_many_arguments)]
fn measure_cell_outer_height(
    c: &Cell,
    children: &mut [LayoutBox],
    cell_w: f32,
    styled: &StyledTree,
    doc: &Document,
    m: &dyn TextMeasurer,
    images: &dyn ImageSource,
    cache: &LayoutCache,
) -> f32 {
    let Some(cell) = cell_mut(children, &c.path) else {
        return 0.0;
    };
    // E12-M1: memoize the layout pass per (cell node, TableCellHeight, cell_w);
    // anonymous cell boxes (NodeId is a shared parent ref) bypass the cache. The
    // cached scalar is the full outer height (content height + vertical bp), so a
    // hit needs no access to the stale `cell.dimensions`.
    let node = match cell.style {
        BoxStyleRef::Node(id) => Some(id),
        _ => None,
    };
    let mut compute = || {
        let cb = Dimensions {
            content: Rect { x: 0.0, y: 0.0, width: cell_w, height: 0.0 },
            ..Dimensions::default()
        };
        let mut floats = FloatContext::default();
        layout_block(cell, cb, styled, doc, m, images, &mut floats, cache);
        let vbp = cell.dimensions.border.top
            + cell.dimensions.border.bottom
            + cell.dimensions.padding.top
            + cell.dimensions.padding.bottom;
        // Note: layout_block already applied any explicit cell height.
        cell.dimensions.content.height + vbp
    };
    match node {
        Some(node) => cache.measure(node, MeasureKind::TableCellHeight, cell_w, compute),
        None => compute(),
    }
}

/// Rightmost edge of any leaf content (text/image/inline-block) in a subtree.
fn max_content_right(b: &LayoutBox) -> Option<f32> {
    let mut right: Option<f32> = None;
    if matches!(b.kind, BoxKind::TextRun | BoxKind::Image | BoxKind::Svg | BoxKind::InlineBlock | BoxKind::FormControl | BoxKind::Media) {
        let mb = b.dimensions.margin_box();
        right = Some(mb.x + mb.width);
    }
    for c in &b.children {
        if let Some(r) = max_content_right(c) {
            right = Some(right.map_or(r, |cur: f32| cur.max(r)));
        }
    }
    right
}

/// Resolve a cell path to a mutable cell box in the taken children tree.
fn cell_mut<'a>(children: &'a mut [LayoutBox], path: &CellPath) -> Option<&'a mut LayoutBox> {
    match (path.group, path.row) {
        (Some(gi), Some(ri)) => children
            .get_mut(gi)?
            .children
            .get_mut(ri)?
            .children
            .get_mut(path.cell),
        (Some(gi), None) => children.get_mut(gi)?.children.get_mut(path.cell),
        (None, Some(ri)) => children.get_mut(ri)?.children.get_mut(path.cell),
        (None, None) => children.get_mut(path.cell),
    }
}

/// Resolve a row path to a mutable row box (None for synthetic rows).
fn row_mut<'a>(children: &'a mut [LayoutBox], path: &RowPath) -> Option<&'a mut LayoutBox> {
    match (path.group, path.row) {
        (Some(gi), Some(ri)) => children.get_mut(gi)?.children.get_mut(ri),
        (None, Some(ri)) => children.get_mut(ri),
        // Synthetic row (direct cells / direct cells under a group) → no box.
        _ => None,
    }
}

/// Immutable variant of `row_mut`.
fn row_ref<'a>(children: &'a [LayoutBox], path: &RowPath) -> Option<&'a LayoutBox> {
    match (path.group, path.row) {
        (Some(gi), Some(ri)) => children.get(gi)?.children.get(ri),
        (None, Some(ri)) => children.get(ri),
        _ => None,
    }
}
