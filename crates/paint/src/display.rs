//! The flat display list (M5 §3): a pre-order walk of the box tree turning each
//! box into background/border fill-rects and each text run into a glyph run, in
//! correct paint order (parent before child; bg → border → text).

use starfish_dom::{Document, NodeId};
use starfish_layout::{parse_view_box, BoxKind, FontQuery, LayoutBox, Rect, ViewBox};
use starfish_style::{
    Background, BorderStyle, BoxShadow, ComputedStyle, Float, FontStyle, FontWeight, LengthPct,
    LinearGradient, Position, Rgba, StyledTree, TextDecorationLine, TransformFn,
};
use tiny_skia::Transform;

use crate::font::FontDb;
use crate::image_store::ImageStore;

/// A device-space (page-space) paint command. Coordinates are f32 page pixels;
/// the rasterizer rounds. Colors are straight (non-premultiplied) `Rgba`.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintCmd {
    /// A filled rectangle (a background or one border edge). `radius` is per
    /// corner (TL,TR,BR,BL); all-zero = sharp corners (the fast path).
    FillRect { rect: Rect, color: Rgba, radius: [f32; 4] },
    /// A run of text. `origin` is the content rect's top-left; the baseline is
    /// `origin.1 + ascent`.
    GlyphRun {
        origin: (f32, f32),
        text: String,
        font_size: f32,
        weight: FontWeight,
        style: FontStyle,
        /// The run's font-family list (owned; the run outlives the style borrow).
        family: Vec<String>,
        color: Rgba,
        ascent: f32,
        /// letter-spacing px added after each char (E6-M3 §6).
        letter_spacing: f32,
        /// word-spacing px added at each space (E6-M3 §6).
        word_spacing: f32,
    },
    /// Blit a decoded image scaled into `dest`. `src` is the raw `<img>` src; the
    /// rasterizer looks the pixels up in the `ImageStore` (E2-M4 §7).
    ImageBlit { dest: Rect, src: String },
    /// A linear-gradient-filled rect (E2-M5 §1.4), optionally rounded.
    GradientRect { rect: Rect, gradient: LinearGradient, radius: [f32; 4] },
    /// A rounded border drawn as a ring: the area between the outer
    /// (border-box) rounded path and the inner (padding-box) rounded path,
    /// filled with `color`. The interior is left untouched so a transparent
    /// background shows through (E2-M5 §5.2).
    FillRing {
        outer: Rect,
        outer_radius: [f32; 4],
        inner: Rect,
        inner_radius: [f32; 4],
        color: Rgba,
    },
    /// An outset box-shadow behind the box (E2-M5 §3.3).
    BoxShadow {
        rect: Rect,
        radius: [f32; 4],
        color: Rgba,
        blur: f32,
        spread: f32,
        offset: (f32, f32),
    },
    /// Begin an offscreen opacity layer wrapping the box + its subtree (§4.2).
    PushLayer { opacity: f32 },
    /// Composite the current opacity layer at its opacity (§4.2).
    PopLayer,
    /// Begin an offscreen transform layer wrapping the box + its subtree. The
    /// subtree is painted at its normal absolute position into the layer; the
    /// layer is composited back via `draw_pixmap` with `matrix` (E5-M3 §4).
    /// `matrix` is a,b,c,d,e,f (→ `Transform::from_row`).
    PushTransform { matrix: [f32; 6] },
    /// Composite the current transform layer through its matrix.
    PopTransform,
    /// A single SVG shape, flattened from an `<svg>` subtree at build time
    /// (E9-M1 §5.2). `transform` (a,b,c,d,e,f) is the user→canvas viewBox
    /// transform; geometry is in user coords. `fill`/`stroke` are already
    /// alpha-folded (shape opacity × paint opacity); `None` ⇒ no paint.
    SvgShape {
        geom: SvgGeom,
        transform: [f32; 6],
        fill: Option<Rgba>,
        /// Fill rule for the geometry (E9-M2 §5).
        fill_rule: SvgFillRule,
        stroke: Option<Rgba>,
        /// Stroke width in USER units (scaled by `transform`).
        stroke_width: f32,
        /// Stroke line cap (E9-M2 §5).
        stroke_cap: SvgLineCap,
        /// Stroke line join (E9-M2 §5).
        stroke_join: SvgLineJoin,
    },
}

/// The geometry of one SVG basic shape, in user-space coordinates (E9-M1 §5.2).
/// NOTE: not `Copy` — `Path` carries a `Vec` (E9-M2 §3.1).
#[derive(Debug, Clone, PartialEq)]
pub enum SvgGeom {
    /// Rectangle (`rx`/`ry` corner radii; 0 ⇒ sharp).
    Rect { x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32 },
    /// Ellipse (circle ⇒ `rx == ry == r`).
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    /// Line (stroke only; fill ignored).
    Line { x1: f32, y1: f32, x2: f32, y2: f32 },
    /// A `<path>` (or polygon/polyline) as an absolute op list (E9-M2).
    Path(Vec<crate::svg_path::PathOp>),
}

/// SVG `fill-rule` (E9-M2 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgFillRule {
    NonZero,
    EvenOdd,
}

/// SVG `stroke-linecap` (E9-M2 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgLineCap {
    Butt,
    Round,
    Square,
}

/// SVG `stroke-linejoin` (E9-M2 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgLineJoin {
    Miter,
    Round,
    Bevel,
}

/// Construct a sharp-cornered fill rect (the common case; radius `[0;4]`).
fn fill(rect: Rect, color: Rgba) -> PaintCmd {
    PaintCmd::FillRect { rect, color, radius: [0.0; 4] }
}

/// Paint role of a box, deciding which pass paints its subtree (§5). Order of
/// precedence: positioned > float > in-flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    InFlow,
    Float,
    Positioned,
}

fn role(b: &LayoutBox, styled: &StyledTree) -> Role {
    // Only genuine element boxes carry float/position; line/anonymous/text/marker
    // boxes borrow the container's style ref, so never reclassify them.
    if !matches!(
        b.kind(),
        BoxKind::BlockContainer | BoxKind::InlineBlock | BoxKind::InlineBox
    ) {
        return Role::InFlow;
    }
    let Some(s) = b.style(styled) else { return Role::InFlow };
    if s.position != Position::Static {
        Role::Positioned
    } else if s.float != Float::None {
        Role::Float
    } else {
        Role::InFlow
    }
}

/// Walk the laid-out box tree and produce the ordered display list, in three
/// passes (§5): in-flow content, then floats, then positioned boxes — each in
/// tree order, so floats/positioned paint on top. Floats/positioned subtrees
/// recursively re-run the three-pass ordering, so nesting layers correctly.
pub fn build_display_list(
    root: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
) -> Vec<PaintCmd> {
    let mut out = Vec::new();
    paint_subtree(root, styled, fonts, images, doc, &mut out);
    out
}

/// Paint one subtree rooted at `b` (whose own role is fixed by its caller): emit
/// `b` itself + its in-flow descendants, then its float descendants (tree
/// order), then its positioned descendants (tree order). Each deferred subtree
/// recurses through `paint_subtree`, so nested out-of-flow content layers right.
fn paint_subtree(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    let mut floats: Vec<&LayoutBox> = Vec::new();
    let mut positioned: Vec<&LayoutBox> = Vec::new();

    // A non-empty `transform` wraps the box + its whole subtree in an offscreen
    // layer composited back through the matrix (E5-M3 §3.2). Empty → no bracket
    // (fast path). The transform layer nests OUTSIDE the opacity layer.
    let xform = layer_transform(b, styled);
    if let Some(m) = xform {
        out.push(PaintCmd::PushTransform { matrix: m });
    }

    // Opacity < 1 wraps the box AND its whole subtree in an offscreen layer so
    // overlapping descendants composite as a group (E2-M5 §4.2). opacity == 1.0
    // → no layer (fast path, unchanged output).
    let layer = layer_opacity(b, styled);
    if let Some(o) = layer {
        out.push(PaintCmd::PushLayer { opacity: o });
    }

    emit_self(b, styled, fonts, images, doc, out);
    for child in b.children() {
        collect_inflow(child, styled, fonts, images, doc, out, &mut floats, &mut positioned);
    }
    for f in floats {
        paint_subtree(f, styled, fonts, images, doc, out);
    }
    for p in positioned {
        paint_subtree(p, styled, fonts, images, doc, out);
    }

    if layer.is_some() {
        out.push(PaintCmd::PopLayer);
    }
    if xform.is_some() {
        out.push(PaintCmd::PopTransform);
    }
}

/// Pre-order over the in-flow content of a subtree, emitting in-flow boxes and
/// deferring out-of-flow subtree roots into the float / positioned buckets.
#[allow(clippy::too_many_arguments)]
fn collect_inflow<'a>(
    b: &'a LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
    floats: &mut Vec<&'a LayoutBox>,
    positioned: &mut Vec<&'a LayoutBox>,
) {
    match role(b, styled) {
        Role::Float => floats.push(b),
        Role::Positioned => positioned.push(b),
        Role::InFlow => {
            // transform wraps this in-flow box + its in-flow descendants
            // (E5-M3 §3.2), outside any opacity layer. Empty → no bracket.
            let xform = layer_transform(b, styled);
            if let Some(m) = xform {
                out.push(PaintCmd::PushTransform { matrix: m });
            }
            // opacity < 1 wraps this in-flow box + its in-flow descendants in an
            // offscreen layer (E2-M5 §4.2). Out-of-flow descendants re-ordered
            // into the float/positioned buckets paint outside the bracket — an
            // accepted M5 edge (they are rare under opacity boxes).
            let layer = layer_opacity(b, styled);
            if let Some(o) = layer {
                out.push(PaintCmd::PushLayer { opacity: o });
            }
            emit_self(b, styled, fonts, images, doc, out);
            for child in b.children() {
                collect_inflow(child, styled, fonts, images, doc, out, floats, positioned);
            }
            if layer.is_some() {
                out.push(PaintCmd::PopLayer);
            }
            if xform.is_some() {
                out.push(PaintCmd::PopTransform);
            }
        }
    }
}

/// The opacity for a box that needs an offscreen layer (`< 1.0`), else `None`.
fn layer_opacity(b: &LayoutBox, styled: &StyledTree) -> Option<f32> {
    b.style(styled).map(|s| s.opacity).filter(|o| *o < 1.0)
}

/// The composed transform matrix for a box with a non-empty `transform`, else
/// `None`. Computed against the box's border-box + origin (E5-M3 §2).
fn layer_transform(b: &LayoutBox, styled: &StyledTree) -> Option<[f32; 6]> {
    let s = b.style(styled)?;
    if s.transform.is_empty() {
        return None;
    }
    Some(compose_transform(s, &b.dimensions().border_box()))
}

/// Resolve a `<length-percentage>` against `basis` (the relevant box extent).
fn resolve_lp(v: LengthPct, basis: f32) -> f32 {
    match v {
        LengthPct::Px(p) => p,
        LengthPct::Percent(p) => p / 100.0 * basis,
    }
}

/// One `TransformFn` → a tiny-skia `Transform` (E5-M3 §2.2). `from_row` order is
/// `sx, ky, kx, sy, tx, ty` (point maps `x'=x·sx+y·kx+tx`, `y'=x·ky+y·sy+ty`).
fn fn_matrix(f: &TransformFn, bb: &Rect) -> Transform {
    match *f {
        TransformFn::Translate(x, y) => Transform::from_row(
            1.0,
            0.0,
            0.0,
            1.0,
            resolve_lp(x, bb.width),
            resolve_lp(y, bb.height),
        ),
        TransformFn::Scale(sx, sy) => Transform::from_row(sx, 0.0, 0.0, sy, 0.0, 0.0),
        TransformFn::Rotate(t) => {
            Transform::from_row(t.cos(), t.sin(), -t.sin(), t.cos(), 0.0, 0.0)
        }
        TransformFn::Skew(ax, ay) => Transform::from_row(1.0, ay.tan(), ax.tan(), 1.0, 0.0, 0.0),
        TransformFn::Matrix(m) => Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]),
    }
}

/// Compose the function list left-to-right about the resolved origin (E5-M3 §2.3):
/// `M = Translate(o) · f1·f2·…·fn · Translate(-o)`, returned as a,b,c,d,e,f.
fn compose_transform(style: &ComputedStyle, bb: &Rect) -> [f32; 6] {
    let ox = bb.x + resolve_lp(style.transform_origin.0, bb.width);
    let oy = bb.y + resolve_lp(style.transform_origin.1, bb.height);

    // f1·f2·…·fn (pre_concat applies the next factor to points first).
    let mut acc = Transform::identity();
    for f in &style.transform {
        acc = acc.pre_concat(fn_matrix(f, bb));
    }
    let m = Transform::from_translate(ox, oy)
        .pre_concat(acc)
        .pre_concat(Transform::from_translate(-ox, -oy));
    [m.sx, m.ky, m.kx, m.sy, m.tx, m.ty]
}

/// Emit this box's own paint commands (bg/border, or text for text/marker runs).
fn emit_self(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    match b.kind() {
        BoxKind::TextRun | BoxKind::Marker => emit_text(b, styled, fonts, out),
        BoxKind::Image => emit_image(b, images, out),
        BoxKind::Svg => emit_svg(b, styled, doc, out),
        _ => emit_box(b, styled, out),
    }
}

/// Emit shadow + background + border for an element box. Routes between the
/// sharp fast path (no rounding → existing 4-edge borders) and the rounded
/// uniform-border approximation (E2-M5 §5.2).
fn emit_box(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    let radius = style.border_radius;
    let bb = b.dimensions().border_box();

    // 1. box-shadow (behind everything).
    if let Some(s) = style.box_shadow {
        emit_shadow(bb, radius, s, out);
    }

    let rounded = !radius_is_zero(radius);
    let d = b.dimensions();
    let has_border = style.border_style == BorderStyle::Solid
        && style.border_color.a != 0
        && (d.border.top > 0.0
            || d.border.right > 0.0
            || d.border.bottom > 0.0
            || d.border.left > 0.0);

    if rounded {
        // Rounded uniform border: outer rounded fill in the border color, then
        // the background fills the padding-box (inset) with reduced corners.
        if has_border {
            // Draw the border as a ring (outer rounded path minus inner
            // padding-box path) so a transparent background never shows the
            // border color through the interior. The background then fills the
            // padding-box on top of the (empty) interior.
            let pb = d.padding_box();
            let irad = inset_radius(radius, &d.border);
            out.push(PaintCmd::FillRing {
                outer: bb,
                outer_radius: radius,
                inner: pb,
                inner_radius: irad,
                color: style.border_color,
            });
            emit_background_at(pb, irad, &style.background, out);
        } else {
            emit_background_at(bb, radius, &style.background, out);
        }
    } else {
        emit_background_at(bb, radius, &style.background, out);
        emit_borders(b, styled, out);
    }
}

/// Emit the background fill (gradient or solid color) for `rect` with `radius`.
fn emit_background_at(rect: Rect, radius: [f32; 4], bg: &Background, out: &mut Vec<PaintCmd>) {
    match bg {
        Background::Color(c) if c.a != 0 => {
            out.push(PaintCmd::FillRect { rect, color: *c, radius });
        }
        Background::Gradient(g) => {
            out.push(PaintCmd::GradientRect { rect, gradient: g.clone(), radius });
        }
        _ => {} // transparent solid → nothing
    }
}

fn emit_shadow(bb: Rect, radius: [f32; 4], s: BoxShadow, out: &mut Vec<PaintCmd>) {
    if s.color.a == 0 {
        return;
    }
    out.push(PaintCmd::BoxShadow {
        rect: bb,
        radius,
        color: s.color,
        blur: s.blur.max(0.0),
        spread: s.spread,
        offset: (s.offset_x, s.offset_y),
    });
}

fn radius_is_zero(r: [f32; 4]) -> bool {
    r.iter().all(|v| *v <= 0.0)
}

/// Reduce each corner radius by the adjacent border widths (clamped ≥0) for the
/// inset padding-box rounded fill. TL,TR,BR,BL each shrink by their min border.
fn inset_radius(r: [f32; 4], border: &starfish_layout::EdgeSizes) -> [f32; 4] {
    let inset = |radius: f32, a: f32, b: f32| (radius - a.min(b)).max(0.0);
    [
        inset(r[0], border.top, border.left),
        inset(r[1], border.top, border.right),
        inset(r[2], border.bottom, border.right),
        inset(r[3], border.bottom, border.left),
    ]
}

/// Emit an `<img>`: an `ImageBlit` if the image decoded, else a 1px grey
/// placeholder border around a non-zero broken-image box (§7.3). A 0×0 broken
/// image (no attrs) paints nothing.
fn emit_image(b: &LayoutBox, images: &ImageStore, out: &mut Vec<PaintCmd>) {
    let Some(src) = b.text() else { return };
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return; // collapsed broken image → nothing
    }
    if images.peek(src).is_some() {
        out.push(PaintCmd::ImageBlit { dest, src: src.to_string() });
        return;
    }
    // Broken image with a non-zero box → 1px grey placeholder border.
    let grey = Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 };
    let edges = [
        Rect { x: dest.x, y: dest.y, width: dest.width, height: 1.0 },
        Rect { x: dest.x, y: dest.y + dest.height - 1.0, width: dest.width, height: 1.0 },
        Rect { x: dest.x, y: dest.y, width: 1.0, height: dest.height },
        Rect { x: dest.x + dest.width - 1.0, y: dest.y, width: 1.0, height: dest.height },
    ];
    for rect in edges {
        out.push(fill(rect, grey));
    }
}

// --- E9-M1: inline SVG shape flattening ---

/// Flatten an `<svg>` box's DOM subtree into self-contained `SvgShape` commands
/// (E9-M1 §5.3). Computes the viewBox→box transform once, then walks the svg's
/// element children, emitting one command per recognized basic shape.
fn emit_svg(b: &LayoutBox, styled: &StyledTree, doc: &Document, out: &mut Vec<PaintCmd>) {
    let svg_id = b.style.node();
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return;
    }
    let vb = parse_view_box(doc.get_attribute(svg_id, "viewBox"));
    let t = svg_transform(dest, vb);
    for child in doc.children(svg_id) {
        if let Some(cmd) = build_shape(doc, styled, child, t) {
            out.push(cmd);
        }
    }
}

/// The user→canvas transform for the svg content rect under the default
/// `preserveAspectRatio: xMidYMid meet` (uniform fit + centering, E9-M1 §4). No
/// viewBox ⇒ user units are px, origin at the dest top-left.
fn svg_transform(dest: Rect, vb: Option<ViewBox>) -> [f32; 6] {
    let Some(vb) = vb else {
        return [1.0, 0.0, 0.0, 1.0, dest.x, dest.y];
    };
    let s = (dest.width / vb.w).min(dest.height / vb.h);
    let tx = dest.x + (dest.width - vb.w * s) / 2.0 - vb.x * s;
    let ty = dest.y + (dest.height - vb.h * s) / 2.0 - vb.y * s;
    [s, 0.0, 0.0, s, tx, ty]
}

/// Resolved paints for a shape (E9-M1 §5.4, extended E9-M2 §5).
struct Paints {
    fill: Option<Rgba>,
    stroke: Option<Rgba>,
    stroke_width: f32,
    fill_rule: SvgFillRule,
    cap: SvgLineCap,
    join: SvgLineJoin,
}

/// Build an `SvgShape` for one element child, or `None` for an unknown tag
/// (`g`/`defs`/`text`/`path`/…, deferred to M2/M3) or a degenerate shape.
fn build_shape(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    transform: [f32; 6],
) -> Option<PaintCmd> {
    let tag = doc.tag_name(id)?;
    let geom = match tag {
        "rect" => {
            let (x, y) = (attr_f(doc, id, "x"), attr_f(doc, id, "y"));
            let w = attr_f(doc, id, "width");
            let h = attr_f(doc, id, "height");
            if w <= 0.0 || h <= 0.0 {
                return None;
            }
            // rx/ry: if only one is given, it mirrors the other; clamp to half.
            let rx_a = attr_opt_f(doc, id, "rx");
            let ry_a = attr_opt_f(doc, id, "ry");
            let (rx, ry) = match (rx_a, ry_a) {
                (Some(rx), Some(ry)) => (rx, ry),
                (Some(rx), None) => (rx, rx),
                (None, Some(ry)) => (ry, ry),
                (None, None) => (0.0, 0.0),
            };
            let rx = rx.max(0.0).min(w / 2.0);
            let ry = ry.max(0.0).min(h / 2.0);
            SvgGeom::Rect { x, y, w, h, rx, ry }
        }
        "circle" => {
            let r = attr_f(doc, id, "r");
            if r <= 0.0 {
                return None;
            }
            SvgGeom::Ellipse {
                cx: attr_f(doc, id, "cx"),
                cy: attr_f(doc, id, "cy"),
                rx: r,
                ry: r,
            }
        }
        "ellipse" => {
            let rx = attr_f(doc, id, "rx");
            let ry = attr_f(doc, id, "ry");
            if rx <= 0.0 || ry <= 0.0 {
                return None;
            }
            SvgGeom::Ellipse {
                cx: attr_f(doc, id, "cx"),
                cy: attr_f(doc, id, "cy"),
                rx,
                ry,
            }
        }
        "line" => SvgGeom::Line {
            x1: attr_f(doc, id, "x1"),
            y1: attr_f(doc, id, "y1"),
            x2: attr_f(doc, id, "x2"),
            y2: attr_f(doc, id, "y2"),
        },
        "path" => {
            let d = doc.get_attribute(id, "d").unwrap_or("");
            let ops = crate::svg_path::parse_path_data(d);
            if ops.is_empty() {
                return None; // empty/missing d → nothing
            }
            SvgGeom::Path(ops)
        }
        "polygon" | "polyline" => {
            let pts = crate::svg_path::parse_points(doc.get_attribute(id, "points").unwrap_or(""));
            if pts.len() < 2 {
                return None;
            }
            SvgGeom::Path(crate::svg_path::points_to_ops(&pts, tag == "polygon"))
        }
        _ => return None, // unknown / unsupported tag (M3)
    };

    let p = resolve_paints(doc, styled, id);
    // A line never fills; a shape with neither fill nor stroke paints nothing.
    let fill = if matches!(geom, SvgGeom::Line { .. }) { None } else { p.fill };
    if fill.is_none() && p.stroke.is_none() {
        return None;
    }
    Some(PaintCmd::SvgShape {
        geom,
        transform,
        fill,
        fill_rule: p.fill_rule,
        stroke: p.stroke,
        stroke_width: p.stroke_width,
        stroke_cap: p.cap,
        stroke_join: p.join,
    })
}

/// Resolve fill/stroke/stroke-width with opacity folded into alpha (E9-M1 §5.4).
/// Lookup order per property: inline `style` declaration, then the presentation
/// attribute, then the SVG initial (fill=black, stroke=none, stroke-width=1).
fn resolve_paints(doc: &Document, styled: &StyledTree, id: NodeId) -> Paints {
    let style = doc.get_attribute(id, "style");
    let prop = |name: &str| -> Option<String> {
        svg_style_prop(style, name).or_else(|| doc.get_attribute(id, name).map(str::to_string))
    };
    let current = styled.get(id).map(|s| s.color).unwrap_or(BLACK);

    let fill = parse_paint(prop("fill").as_deref(), Some(BLACK), current);
    let stroke = parse_paint(prop("stroke").as_deref(), None, current);
    let sw = prop("stroke-width")
        .and_then(|s| parse_len(&s))
        .unwrap_or(1.0)
        .max(0.0);

    let op = prop("opacity").and_then(parse_opacity).unwrap_or(1.0);
    let fo = prop("fill-opacity").and_then(parse_opacity).unwrap_or(1.0);
    let so = prop("stroke-opacity").and_then(parse_opacity).unwrap_or(1.0);

    let fill_rule = match prop("fill-rule").as_deref().map(str::trim) {
        Some("evenodd") => SvgFillRule::EvenOdd,
        _ => SvgFillRule::NonZero,
    };
    let cap = match prop("stroke-linecap").as_deref().map(str::trim) {
        Some("round") => SvgLineCap::Round,
        Some("square") => SvgLineCap::Square,
        _ => SvgLineCap::Butt,
    };
    let join = match prop("stroke-linejoin").as_deref().map(str::trim) {
        Some("round") => SvgLineJoin::Round,
        Some("bevel") => SvgLineJoin::Bevel,
        _ => SvgLineJoin::Miter,
    };

    Paints {
        fill: fill.map(|c| with_alpha(c, op * fo)),
        stroke: stroke.map(|c| with_alpha(c, op * so)),
        stroke_width: sw,
        fill_rule,
        cap,
        join,
    }
}

const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 255 };

/// Parse an SVG paint value: absent ⇒ `default`; `none` ⇒ `None`;
/// `currentColor` ⇒ the element's CSS color; else `parse_color` (named/hex/rgb).
/// An unparseable value falls back to `default` (lenient, E9-M1 §6).
fn parse_paint(s: Option<&str>, default: Option<Rgba>, current: Rgba) -> Option<Rgba> {
    let Some(s) = s else { return default };
    let t = s.trim();
    if t.eq_ignore_ascii_case("none") {
        return None;
    }
    if t.eq_ignore_ascii_case("currentColor") {
        return Some(current);
    }
    starfish_css::parse_color(t).or(default)
}

/// Look up a property in an inline `style` string (`"fill:red;stroke:#00f"`).
/// Trivial `;`/`:`-split; the last matching declaration wins.
fn svg_style_prop(style: Option<&str>, name: &str) -> Option<String> {
    let style = style?;
    let mut found = None;
    for decl in style.split(';') {
        let (k, v) = decl.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            found = Some(v.trim().to_string());
        }
    }
    found
}

/// Parse a numeric shape attribute (px/unitless), default 0 if absent/invalid.
fn attr_f(doc: &Document, id: NodeId, name: &str) -> f32 {
    attr_opt_f(doc, id, name).unwrap_or(0.0)
}

/// Parse a numeric shape attribute, `None` if absent/invalid.
fn attr_opt_f(doc: &Document, id: NodeId, name: &str) -> Option<f32> {
    doc.get_attribute(id, name).and_then(parse_len)
}

/// Parse a length value (px/unitless) to f32. `None` if non-finite/unparseable.
fn parse_len(s: &str) -> Option<f32> {
    s.trim()
        .trim_end_matches("px")
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Parse an opacity value (clamped to 0..=1).
fn parse_opacity(s: String) -> Option<f32> {
    s.trim().parse::<f32>().ok().map(|v| v.clamp(0.0, 1.0))
}

/// Scale a color's alpha by `a` (0..=1).
fn with_alpha(c: Rgba, a: f32) -> Rgba {
    Rgba {
        a: ((c.a as f32) * a).round().clamp(0.0, 255.0) as u8,
        ..c
    }
}

fn emit_borders(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    if style.border_style != BorderStyle::Solid {
        return;
    }
    let bc = style.border_color;
    if bc.a == 0 {
        return;
    }
    let d = b.dimensions();
    let bb = d.border_box();

    if d.border.top > 0.0 {
        out.push(fill(
            Rect { x: bb.x, y: bb.y, width: bb.width, height: d.border.top },
            bc,
        ));
    }
    if d.border.bottom > 0.0 {
        out.push(fill(
            Rect {
                x: bb.x,
                y: bb.y + bb.height - d.border.bottom,
                width: bb.width,
                height: d.border.bottom,
            },
            bc,
        ));
    }
    if d.border.left > 0.0 {
        out.push(fill(
            Rect {
                x: bb.x,
                y: bb.y + d.border.top,
                width: d.border.left,
                height: bb.height - d.border.top - d.border.bottom,
            },
            bc,
        ));
    }
    if d.border.right > 0.0 {
        out.push(fill(
            Rect {
                x: bb.x + bb.width - d.border.right,
                y: bb.y + d.border.top,
                width: d.border.right,
                height: bb.height - d.border.top - d.border.bottom,
            },
            bc,
        ));
    }
}

fn emit_text(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    let initial = ComputedStyle::initial();
    let style = b.style(styled).unwrap_or(&initial);
    let text = b.text().unwrap_or("");
    if text.is_empty() {
        return;
    }
    let c = b.dimensions().content;
    let q = FontQuery {
        family: &style.font_family,
        style: style.font_style,
        weight: style.font_weight,
        size: style.font_size,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
    };
    let lm = fonts.line_metrics(&q);
    out.push(PaintCmd::GlyphRun {
        origin: (c.x, c.y),
        text: text.to_string(),
        font_size: style.font_size,
        weight: style.font_weight,
        style: style.font_style,
        family: style.font_family.clone(),
        color: style.color,
        ascent: lm.ascent,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
    });

    // text-decoration lines — only for real text runs, never markers (§4.1).
    if b.kind() != BoxKind::TextRun {
        return;
    }
    let deco = style.text_decoration_line;
    if deco.is_none() {
        return;
    }
    let thickness = (style.font_size / 16.0).max(1.0);
    let color = style.color; // decoration color = text color
    let baseline = c.y + lm.ascent;
    let mut line = |y: f32| {
        out.push(fill(
            Rect { x: c.x, y, width: c.width, height: thickness },
            color,
        ));
    };
    if deco.contains(TextDecorationLine::UNDERLINE) {
        line(baseline + 1.0); // just below baseline
    }
    if deco.contains(TextDecorationLine::LINE_THROUGH) {
        line(baseline - lm.ascent * 0.3); // ~middle / x-height
    }
    if deco.contains(TextDecorationLine::OVERLINE) {
        line(c.y); // top of the content box
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;
    use starfish_layout::layout;
    use starfish_style::style_tree;

    use crate::font::FontMeasurer;
    use starfish_net::{file_url_from_path, LocalLoader, Url};

    fn list(html: &str, css: &str) -> Vec<PaintCmd> {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let images = ImageStore::new(Url::parse("file:///").unwrap(), &LocalLoader);
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    #[test]
    fn background_before_border_before_text() {
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{background:#ff0000;border:2px solid #0000ff}",
        );
        // first fill is the div background (red), then border fills (blue),
        // then the glyph run.
        let first_bg = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0)
        });
        let first_border = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0)
        });
        let first_glyph = cmds.iter().position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (bg, border, glyph) = (
            first_bg.expect("bg"),
            first_border.expect("border"),
            first_glyph.expect("glyph"),
        );
        assert!(bg < border, "bg {bg} before border {border}");
        assert!(border < glyph, "border {border} before glyph {glyph}");
    }

    #[test]
    fn div_with_bg_emits_fillrect_at_its_rect() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00}",
        );
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.g == 255 && color.r == 0 && rect.width == 100.0
        ));
        assert!(found, "expected a green 100px-wide fill rect: {cmds:?}");
    }

    #[test]
    fn transparent_no_border_emits_no_fillrect() {
        let cmds = list("<html><body><p>hi</p></body></html>", "body{margin:0}");
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::FillRect { .. })),
            "no fills expected for a plain paragraph: {cmds:?}"
        );
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { .. })));
    }

    #[test]
    fn text_run_carries_parent_color_and_size() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{color:#0000ff;font-size:20px}",
        );
        let glyph = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, font_size, text, .. } => Some((*color, *font_size, text.clone())),
            _ => None,
        });
        let (color, fs, text) = glyph.expect("glyph run");
        assert_eq!(color, Rgba { r: 0, g: 0, b: 255, a: 255 });
        assert_eq!(fs, 20.0);
        assert_eq!(text, "hi");
    }

    // --- E2-M1: text-decoration, list markers, inline-block ---

    /// The glyph run + its content rect for the first text run matching `t`.
    fn glyph_with_origin(cmds: &[PaintCmd], t: &str) -> (f32, f32, f32) {
        cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { origin, text, .. } if text == t => Some((origin.0, origin.1, 0.0)),
            _ => None,
        }).unwrap_or_else(|| panic!("no glyph run {t:?}"))
    }

    #[test]
    fn underline_emits_fillrect_below_baseline() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;text-decoration:underline}",
        );
        // exactly one fill rect (the underline) at baseline+1.
        let fills: Vec<&PaintCmd> = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).collect();
        assert_eq!(fills.len(), 1, "expected one underline rect: {cmds:?}");
        // locate the glyph run to recover its content x/y and width.
        let (gx, gy, _) = glyph_with_origin(&cmds, "hi");
        let (rect, color) = match fills[0] {
            PaintCmd::FillRect { rect, color, .. } => (*rect, *color),
            _ => unreachable!(),
        };
        assert_eq!(rect.x, gx);
        assert!(rect.width > 0.0);
        assert_eq!(rect.height, (20.0f32 / 16.0).max(1.0));
        assert_eq!(color, Rgba { r: 0, g: 0, b: 0, a: 255 });
        // y ≈ content.y + ascent + 1; assert it's below the glyph origin.
        assert!(rect.y > gy);
    }

    #[test]
    fn combined_decoration_emits_three_rects() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration-line:underline overline line-through}",
        );
        let fills = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).count();
        assert_eq!(fills, 3, "underline+overline+line-through: {cmds:?}");
    }

    #[test]
    fn marker_emits_bullet_glyph() {
        let cmds = list("<html><body><ul><li>a</li></ul></body></html>", "body{margin:0}");
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")),
            "expected a bullet glyph run: {cmds:?}"
        );
    }

    #[test]
    fn decimal_marker_emits_number_glyph() {
        let cmds = list("<html><body><ol><li>x</li></ol></body></html>", "body{margin:0}");
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "1.")),
            "expected a '1.' glyph run: {cmds:?}"
        );
    }

    #[test]
    fn inline_block_paints_its_background() {
        let cmds = list(
            "<html><body><div><span class='ib'>x</span></div></body></html>",
            "body{margin:0} div{margin:0} \
             .ib{display:inline-block;width:50px;height:20px;background:#00ff00}",
        );
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.g == 255 && color.r == 0 && rect.width == 50.0
        ));
        assert!(found, "expected a green 50px-wide inline-block bg: {cmds:?}");
    }

    // --- E2-M2: float / positioned paint ordering ---

    /// Index of the first FillRect whose color matches a predicate.
    fn first_fill(cmds: &[PaintCmd], pred: impl Fn(&Rgba) -> bool) -> Option<usize> {
        cmds.iter()
            .position(|c| matches!(c, PaintCmd::FillRect { color, .. } if pred(color)))
    }

    #[test]
    fn paint_order_inflow_then_float_then_positioned() {
        // An in-flow div (red bg), a left float (green bg), and an absolute div
        // (blue bg): float bg paints after in-flow bg; absolute after float.
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='n'></div>\
             <div id='f'></div>\
             <div id='a'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #n{background:#ff0000;height:20px} \
             #f{float:left;width:40px;height:20px;background:#00ff00} \
             #a{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff}",
        );
        let red = first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("in-flow red bg");
        let green = first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("float green bg");
        let blue = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("abs blue bg");
        assert!(red < green, "in-flow {red} before float {green}");
        assert!(green < blue, "float {green} before positioned {blue}");
    }

    #[test]
    fn inflow_only_display_list_unchanged() {
        // The existing in-flow-only corpus must produce an identical display list
        // under the new three-pass build_display_list (passes 2/3 empty).
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{background:#ff0000;border:2px solid #0000ff}",
        );
        // Same shape/order as background_before_border_before_text expects.
        let bg = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0)
        });
        let border = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0)
        });
        let glyph = cmds.iter().position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (bg, border, glyph) = (bg.expect("bg"), border.expect("border"), glyph.expect("glyph"));
        assert!(bg < border && border < glyph);
        // No float/positioned content → display list is exactly the in-flow walk:
        // div bg + 4 border edges + glyph = 6 commands.
        assert_eq!(cmds.len(), 6, "unexpected extra commands: {cmds:?}");
    }

    #[test]
    fn marker_is_not_decorated() {
        // <ul> with underline; the bullet glyph must have no decoration rect.
        let cmds = list(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0} ul{text-decoration:underline} li{text-decoration:underline}",
        );
        // the bullet glyph exists...
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")));
        // ...but only the "a" TextRun produces an underline rect, not the marker.
        // There's exactly one decoration FillRect (for "a").
        let fills = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).count();
        assert_eq!(fills, 1, "only the text run is decorated, not the marker: {cmds:?}");
    }

    // --- E2-M4: <img> display-list emission ---

    /// Write a 2×2 PNG into a fresh temp dir, returning the dir, and build the
    /// display list for `html` resolving images against that dir.
    fn list_with_fixture(html: &str, css: &str) -> Vec<PaintCmd> {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-dl-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("px.png")).unwrap();

        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images =
            ImageStore::new(file_url_from_path(&dir.join("index.html")).unwrap(), &LocalLoader);
        // Pre-pass decode (mirror render_html).
        images.get("px.png");
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    #[test]
    fn decoded_img_emits_imageblit() {
        let cmds = list_with_fixture(
            "<html><body><img src='px.png' width='4' height='4'></body></html>",
            "body{margin:0}",
        );
        let blit = cmds.iter().find_map(|c| match c {
            PaintCmd::ImageBlit { dest, src } => Some((*dest, src.clone())),
            _ => None,
        });
        let (dest, src) = blit.expect("an ImageBlit command");
        assert_eq!(dest.width, 4.0);
        assert_eq!(src, "px.png");
    }

    #[test]
    fn broken_img_emits_placeholder_border() {
        let cmds = list_with_fixture(
            "<html><body><img src='nope.png' width='10' height='10'></body></html>",
            "body{margin:0}",
        );
        // no blit
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // 4 grey placeholder edges
        let grey = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 0x80 && color.g == 0x80 && color.b == 0x80))
            .count();
        assert_eq!(grey, 4, "expected 4 placeholder border rects: {cmds:?}");
    }

    // --- E2-M5: gradient / shadow / opacity display-list emission ---

    #[test]
    fn gradient_div_emits_gradient_rect_not_fillrect_bg() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;\
             background:linear-gradient(to right, #ff0000, #0000ff)}",
        );
        let grads = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::GradientRect { .. }))
            .count();
        assert_eq!(grads, 1, "exactly one GradientRect: {cmds:?}");
        // no solid background FillRect for this div.
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { rect, .. } if rect.width == 100.0
            )),
            "gradient bg should not emit a FillRect: {cmds:?}"
        );
    }

    #[test]
    fn box_shadow_emitted_before_background() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;background:#ffffff;\
             box-shadow:5px 5px 0 0 #000000}",
        );
        let shadow = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::BoxShadow { .. }))
            .expect("a BoxShadow command");
        let bg = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::FillRect { rect, .. } if rect.width == 50.0))
            .expect("the white background fill");
        assert!(shadow < bg, "shadow {shadow} before bg {bg}: {cmds:?}");
    }

    #[test]
    fn opacity_brackets_subtree_with_push_pop() {
        let cmds = list(
            "<html><body><div id='d'><p>x</p></div></body></html>",
            "body{margin:0} #d{opacity:0.5;background:#ff0000;width:50px;height:50px}",
        );
        let push = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushLayer { opacity } if *opacity == 0.5))
            .expect("PushLayer{0.5}");
        let pop = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopLayer))
            .expect("PopLayer");
        let glyph = cmds.iter().position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        assert!(push < pop, "push {push} before pop {pop}");
        if let Some(g) = glyph {
            assert!(push < g && g < pop, "glyph {g} inside the layer bracket");
        }
        // No layer for opacity == 1.
        let plain = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:50px;height:50px}",
        );
        assert!(!plain.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })));
    }

    // --- E5-M1: grid item paint (no paint change — items are ordinary boxes) ---

    #[test]
    fn grid_item_backgrounds_paint_at_their_cells() {
        // 2x2 fixed grid; each item a distinct background. They paint as ordinary
        // absolutely-positioned FillRects at their grid cells.
        let cmds = list(
            "<html><body><div id='g'>\
             <div class='c' id='a'></div><div class='c' id='b'></div>\
             <div class='c' id='c'></div><div class='c' id='d'></div>\
             </div></body></html>",
            "body{margin:0} #g{margin:0;display:grid;width:200px;\
             grid-template-columns:100px 100px;grid-template-rows:50px 50px;gap:0} \
             .c{margin:0} #a{background:#ff0000} #b{background:#00ff00} \
             #c{background:#0000ff} #d{background:#ffff00}",
        );
        // item b (green) sits in the top-right cell at (100,0) 100x50.
        let green = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.g == 255 && color.r == 0 && color.b == 0
                    && rect.x == 100.0 && rect.y == 0.0
                    && rect.width == 100.0 && rect.height == 50.0
        ));
        assert!(green, "green item at top-right cell (100,0): {cmds:?}");
        // item c (blue) sits in the bottom-left cell at (0,50).
        let blue = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.b == 255 && color.r == 0 && color.g == 0
                    && rect.x == 0.0 && rect.y == 50.0
                    && rect.width == 100.0 && rect.height == 50.0
        ));
        assert!(blue, "blue item at bottom-left cell (0,50): {cmds:?}");
        // item d (yellow) in the bottom-right cell at (100,50).
        let yellow = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.r == 255 && color.g == 255 && color.b == 0
                    && rect.x == 100.0 && rect.y == 50.0
        ));
        assert!(yellow, "yellow item at bottom-right cell (100,50): {cmds:?}");
    }

    // --- E5-M3: transform display-list emission ---

    #[test]
    fn transform_brackets_subtree_with_push_pop() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:translate(20px,10px);background:#ff0000;\
             width:40px;height:40px}",
        );
        let push = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushTransform { .. }))
            .expect("PushTransform");
        let pop = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopTransform))
            .expect("PopTransform");
        let bg = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::FillRect { rect, .. } if rect.width == 40.0))
            .expect("bg");
        assert!(push < bg && bg < pop, "bg {bg} inside [{push},{pop}]");
        // PushTransform is the first command of the subtree.
        assert_eq!(push, 0, "PushTransform should open the subtree: {cmds:?}");
    }

    #[test]
    fn no_transform_emits_no_transform_cmds() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:40px;height:40px}",
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::PushTransform { .. })));
        // transform:none likewise.
        let none = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:none;background:#ff0000;width:40px;height:40px}",
        );
        assert!(!none.iter().any(|c| matches!(c, PaintCmd::PushTransform { .. })));
    }

    #[test]
    fn transform_outside_opacity_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:translate(10px);opacity:0.5;\
             background:#ff0000;width:40px;height:40px}",
        );
        let pt = cmds.iter().position(|c| matches!(c, PaintCmd::PushTransform { .. })).unwrap();
        let pl = cmds.iter().position(|c| matches!(c, PaintCmd::PushLayer { .. })).unwrap();
        let popl = cmds.iter().position(|c| matches!(c, PaintCmd::PopLayer)).unwrap();
        let popt = cmds.iter().position(|c| matches!(c, PaintCmd::PopTransform)).unwrap();
        // PushTransform, PushLayer, …, PopLayer, PopTransform
        assert!(pt < pl && pl < popl && popl < popt, "{cmds:?}");
    }

    #[test]
    fn translate_matrix_is_origin_independent() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:translate(20px,10px);width:40px;height:40px}",
        );
        let m = cmds.iter().find_map(|c| match c {
            PaintCmd::PushTransform { matrix } => Some(*matrix),
            _ => None,
        }).expect("a matrix");
        // pure translate is origin-independent → [1,0,0,1,20,10].
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(m[0], 1.0) && approx(m[1], 0.0) && approx(m[2], 0.0) && approx(m[3], 1.0));
        assert!(approx(m[4], 20.0) && approx(m[5], 10.0), "tx,ty = {},{}", m[4], m[5]);
    }

    // --- E6-M1: GlyphRun carries family + style ---

    #[test]
    fn glyph_run_carries_family_and_style() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{font-family:monospace;font-style:italic}",
        );
        let run = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { family, style, .. } => Some((family.clone(), *style)),
            _ => None,
        });
        let (family, style) = run.expect("glyph run");
        assert_eq!(family, vec!["monospace".to_string()]);
        assert_eq!(style, FontStyle::Italic);
    }

    #[test]
    fn glyph_run_inherits_family() {
        // child <span> with no font-family inherits the parent's list.
        let cmds = list(
            "<html><body><p>a<span>b</span></p></body></html>",
            "body{margin:0} p{font-family:serif}",
        );
        let any_serif = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::GlyphRun { family, .. } if family == &vec!["serif".to_string()]
        ));
        assert!(any_serif, "child glyph run inherits serif family: {cmds:?}");
    }

    // --- E6-M3: text-transform / spacing in the GlyphRun ---

    #[test]
    fn text_transform_uppercase_bakes_into_glyphrun() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{text-transform:uppercase}",
        );
        let text = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { text, .. } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(text.expect("glyph run"), "HI");
    }

    #[test]
    fn glyph_run_carries_spacing() {
        let cmds = list(
            "<html><body><p>a b</p></body></html>",
            "body{margin:0} p{letter-spacing:4px;word-spacing:7px}",
        );
        let sp = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { letter_spacing, word_spacing, .. } => {
                Some((*letter_spacing, *word_spacing))
            }
            _ => None,
        });
        assert_eq!(sp.expect("glyph run"), (4.0, 7.0));
    }

    #[test]
    fn rounded_bg_emits_fillrect_with_radius() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;background:#ff0000;border-radius:10px}",
        );
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { radius, rect, .. }
                if rect.width == 50.0 && radius == &[10.0; 4]
        ));
        assert!(found, "expected a rounded FillRect bg: {cmds:?}");
    }

    // --- E7-M2: ::before / ::after generated content paints ---

    #[test]
    fn before_text_emits_colored_glyph() {
        let cmds = list(
            "<html><body><div>hi</div></body></html>",
            "body{margin:0} div::before { content: \"x\"; color: #ff0000 }",
        );
        let glyph = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text == "x" => Some(*color),
            _ => None,
        });
        assert_eq!(glyph, Some(Rgba { r: 255, g: 0, b: 0, a: 255 }));
    }

    #[test]
    fn li_custom_bullet_before() {
        // li::before red bullet renders before the list item's text.
        let cmds = list(
            "<html><body><ul style='list-style:none'><li>One</li></ul></body></html>",
            "body{margin:0} li::before { content: \"\u{2022} \"; color: #ff0000 }",
        );
        let texts: Vec<_> = cmds.iter().filter_map(|c| match c {
            PaintCmd::GlyphRun { text, .. } => Some(text.clone()),
            _ => None,
        }).collect();
        let bullet = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text.contains('\u{2022}') => Some(*color),
            _ => None,
        });
        assert_eq!(bullet, Some(Rgba { r: 255, g: 0, b: 0, a: 255 }), "glyphs={texts:?}");
    }

    #[test]
    fn after_text_appends() {
        let cmds = list(
            "<html><body><p><a>link</a></p></body></html>",
            "body{margin:0} a::after { content: \" \u{2197}\"; color: #0000ff }",
        );
        let texts: Vec<_> = cmds.iter().filter_map(|c| match c {
            PaintCmd::GlyphRun { text, .. } => Some(text.clone()),
            _ => None,
        }).collect();
        let mark = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text.contains('\u{2197}') => Some(*color),
            _ => None,
        });
        assert_eq!(mark, Some(Rgba { r: 0, g: 0, b: 255, a: 255 }), "glyphs={texts:?}");
    }

    #[test]
    fn attr_chip_carries_pseudo_font_weight() {
        // `[data-tag]::before { content: attr(data-tag); font-weight: bold }`
        // — the generated glyph carries the resolved attr text + bold weight.
        // (Inline-box background painting is out of scope: this engine flattens
        // InlineBox into line fragments, so no inline-box bg FillRect is emitted
        // for any inline element — a pre-existing limitation, see §6.)
        let cmds = list(
            "<html><body><span data-tag='NEW'>item</span></body></html>",
            "body{margin:0} [data-tag]::before { content: attr(data-tag); font-weight: bold }",
        );
        let chip = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { text, weight, .. } if text == "NEW" => Some(*weight),
            _ => None,
        });
        assert_eq!(chip, Some(FontWeight(700)));
    }

    // --- E7-M3: table paint (no paint-code change — cells/rows are normal boxes) ---

    #[test]
    fn table_cell_background_paints_at_slot() {
        // A 2×2 table where cell (0,0) has a red background → a red FillRect at
        // the cell's border box (top-left of the table, border-spacing 0).
        let cmds = list(
            "<html><body><table>\
               <tr><td id='a'>xx</td><td>yy</td></tr>\
               <tr><td>zz</td><td>ww</td></tr>\
             </table></body></html>",
            "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0} \
             #a{background:#ff0000}",
        );
        let red = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.r == 255 && color.g == 0 && color.b == 0
                   && rect.x == 0.0 && rect.y == 0.0
        ));
        assert!(red, "expected a red cell fill at (0,0): {cmds:?}");
    }

    #[test]
    fn table_row_background_paints() {
        // A <tr> with a background → a grey FillRect spanning the row (confirms
        // the table algorithm sizes the row box).
        let cmds = list(
            "<html><body><table>\
               <tr id='r'><td>xx</td><td>yy</td></tr>\
             </table></body></html>",
            "body{margin:0} table{margin:0;border-spacing:0} td{padding:0;border:0} \
             #r{background:#888888}",
        );
        let grey = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect, .. }
                if color.r == 0x88 && color.g == 0x88 && color.b == 0x88 && rect.width > 0.0
        ));
        assert!(grey, "expected a grey row fill: {cmds:?}");
    }

    #[test]
    fn table_header_cell_borders_and_colspan_grid() {
        // The milestone visual: a styled table with a header row (UA bold/center
        // + a background) and a body row containing a colspan=2 cell, plus
        // per-cell borders. Confirm header bg, cell borders, and the spanning
        // cell all emit fills.
        let cmds = list(
            "<html><body><table>\
               <thead><tr><th>A</th><th>B</th></tr></thead>\
               <tbody><tr><td colspan='2'>wide</td></tr></tbody>\
             </table></body></html>",
            "body{margin:0} table{margin:0;border-spacing:2px} \
             td,th{border:1px solid #888888;padding:6px} th{background:#ddddee}",
        );
        // header background present.
        let header_bg = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, .. } if color.r == 0xdd && color.g == 0xdd && color.b == 0xee
        ));
        assert!(header_bg, "header bg: {cmds:?}");
        // some border fills (the #888 border color).
        let border = cmds.iter().filter(|c| matches!(
            c,
            PaintCmd::FillRect { color, .. } if color.r == 0x88 && color.g == 0x88 && color.b == 0x88
        )).count();
        assert!(border >= 2, "expected cell border fills, got {border}");
        // the colspan cell's text is laid out (a glyph run exists).
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "wide")));
    }

    // --- E9-M1: inline SVG shape display-list emission ---

    /// Collect the SvgShape commands from a display list.
    fn svg_shapes(cmds: &[PaintCmd]) -> Vec<&PaintCmd> {
        cmds.iter().filter(|c| matches!(c, PaintCmd::SvgShape { .. })).collect()
    }

    fn red() -> Rgba { Rgba { r: 255, g: 0, b: 0, a: 255 } }
    fn blue() -> Rgba { Rgba { r: 0, g: 0, b: 255, a: 255 } }

    #[test]
    fn svg_rect_emits_red_fill_at_translated_coords() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='10' y='10' width='80' height='80' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1, "one rect shape: {cmds:?}");
        match shapes[0] {
            PaintCmd::SvgShape { geom, transform, fill, stroke, .. } => {
                assert_eq!(*geom, SvgGeom::Rect { x: 10.0, y: 10.0, w: 80.0, h: 80.0, rx: 0.0, ry: 0.0 });
                assert_eq!(*fill, Some(red()));
                assert_eq!(*stroke, None);
                // no viewBox → identity scale, translate to the svg box origin (0,0).
                assert_eq!(*transform, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_circle_ellipse_line_geom() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <circle cx='50' cy='50' r='20' fill='blue'/>\
             <ellipse cx='50' cy='25' rx='40' ry='10' fill='red'/>\
             <line x1='0' y1='0' x2='100' y2='100' stroke='black' stroke-width='4'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 3, "circle+ellipse+line: {cmds:?}");
        // circle → ellipse rx==ry==r, blue fill.
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { geom: SvgGeom::Ellipse { cx, cy, rx, ry }, fill: Some(c), .. }
            if *cx == 50.0 && *cy == 50.0 && *rx == 20.0 && *ry == 20.0 && *c == blue()));
        // ellipse rx!=ry.
        assert!(matches!(shapes[1],
            PaintCmd::SvgShape { geom: SvgGeom::Ellipse { rx, ry, .. }, .. }
            if *rx == 40.0 && *ry == 10.0));
        // line → stroke only, no fill, stroke-width honored.
        assert!(matches!(shapes[2],
            PaintCmd::SvgShape { geom: SvgGeom::Line { x2, y2, .. }, fill: None, stroke: Some(_), stroke_width, .. }
            if *x2 == 100.0 && *y2 == 100.0 && *stroke_width == 4.0));
    }

    #[test]
    fn svg_viewbox_scales_transform() {
        // viewBox 0 0 10 10 mapped onto a 100x100 box → uniform scale ×10.
        let cmds = list(
            "<html><body><svg width='100' height='100' viewBox='0 0 10 10'>\
             <rect x='1' y='1' width='8' height='8' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        match shapes[0] {
            PaintCmd::SvgShape { geom, transform, .. } => {
                // geometry stays in user coords; the transform scales.
                assert_eq!(*geom, SvgGeom::Rect { x: 1.0, y: 1.0, w: 8.0, h: 8.0, rx: 0.0, ry: 0.0 });
                assert_eq!(*transform, [10.0, 0.0, 0.0, 10.0, 0.0, 0.0]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_fill_none_with_stroke_emits_stroke_only() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='10' y='10' width='80' height='80' fill='none' stroke='black' stroke-width='2'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { fill: None, stroke: Some(_), stroke_width, .. } if *stroke_width == 2.0));
    }

    #[test]
    fn svg_color_formats_all_parse() {
        let cmds = list(
            "<html><body><svg width='90' height='30'>\
             <rect x='0' y='0' width='10' height='10' fill='#00ff00'/>\
             <rect x='10' y='0' width='10' height='10' fill='rgb(0,0,255)'/>\
             <rect x='20' y='0' width='10' height='10' fill='blue'/></svg></body></html>",
            "body{margin:0}",
        );
        let fills: Vec<Rgba> = svg_shapes(&cmds).iter().filter_map(|c| match c {
            PaintCmd::SvgShape { fill, .. } => *fill,
            _ => None,
        }).collect();
        assert_eq!(fills.len(), 3);
        assert_eq!(fills[0], Rgba { r: 0, g: 255, b: 0, a: 255 });
        assert_eq!(fills[1], blue());
        assert_eq!(fills[2], blue());
    }

    #[test]
    fn svg_default_fill_is_black() {
        // No fill attribute → SVG initial fill = black.
        let cmds = list(
            "<html><body><svg width='50' height='50'>\
             <rect x='0' y='0' width='50' height='50'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { fill: Some(c), .. } if *c == BLACK));
    }

    #[test]
    fn svg_fill_opacity_folds_into_alpha() {
        let cmds = list(
            "<html><body><svg width='50' height='50'>\
             <rect x='0' y='0' width='50' height='50' fill='red' fill-opacity='0.5'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        match shapes[0] {
            PaintCmd::SvgShape { fill: Some(c), .. } => {
                assert_eq!(c.r, 255);
                // 0.5 × 255 ≈ 128.
                assert!((c.a as i32 - 128).abs() <= 1, "alpha={}", c.a);
            }
            _ => panic!("expected a filled rect: {cmds:?}"),
        }
    }

    #[test]
    fn svg_inline_style_overrides_presentation_attr() {
        let cmds = list(
            "<html><body><svg width='50' height='50'>\
             <rect x='0' y='0' width='50' height='50' fill='red' style='fill:blue'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { fill: Some(c), .. } if *c == blue()));
    }

    #[test]
    fn svg_unknown_tag_skipped() {
        // <g>/<text>/<path> are deferred → no shape; the rect still emits.
        let cmds = list(
            "<html><body><svg width='50' height='50'>\
             <g></g><text>hi</text><path/>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert_eq!(svg_shapes(&cmds).len(), 1, "only the rect: {cmds:?}");
    }

    #[test]
    fn non_svg_page_emits_no_svg_shapes() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:40px;height:40px}",
        );
        assert!(svg_shapes(&cmds).is_empty());
    }

    // --- E9-M2: <path> / <polygon> / <polyline> + fill-rule / cap / join ---

    use crate::svg_path::PathOp;

    #[test]
    fn svg_path_emits_path_geom() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <path d='M10 10 L90 10 L90 90 Z' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1, "one path shape: {cmds:?}");
        match shapes[0] {
            PaintCmd::SvgShape { geom, fill, fill_rule, .. } => {
                assert_eq!(
                    *geom,
                    SvgGeom::Path(vec![
                        PathOp::MoveTo(10.0, 10.0),
                        PathOp::LineTo(90.0, 10.0),
                        PathOp::LineTo(90.0, 90.0),
                        PathOp::Close,
                    ])
                );
                assert_eq!(*fill, Some(red()));
                assert_eq!(*fill_rule, SvgFillRule::NonZero);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_path_fill_rule_evenodd_parsed() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <path d='M0 0 L10 0 L10 10 Z' fill='red' fill-rule='evenodd'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { fill_rule: SvgFillRule::EvenOdd, .. }));
    }

    #[test]
    fn svg_polygon_closes_polyline_does_not() {
        let poly = list(
            "<html><body><svg width='100' height='100'>\
             <polygon points='50,5 90,90 10,90' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&poly)[0] {
            PaintCmd::SvgShape { geom: SvgGeom::Path(ops), .. } => {
                assert_eq!(*ops.last().unwrap(), PathOp::Close);
            }
            _ => panic!("expected polygon path"),
        }
        let line = list(
            "<html><body><svg width='100' height='100'>\
             <polyline points='0,0 10,0 10,10' fill='none' stroke='black'/></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&line)[0] {
            PaintCmd::SvgShape { geom: SvgGeom::Path(ops), .. } => {
                assert!(!ops.iter().any(|o| matches!(o, PathOp::Close)), "polyline has no Close");
            }
            _ => panic!("expected polyline path"),
        }
    }

    #[test]
    fn svg_empty_path_emits_no_command() {
        let empty = list(
            "<html><body><svg width='50' height='50'>\
             <path d='' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(svg_shapes(&empty).is_empty(), "empty d → no command");
        let missing = list(
            "<html><body><svg width='50' height='50'>\
             <path fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(svg_shapes(&missing).is_empty(), "missing d → no command");
    }

    #[test]
    fn svg_stroke_cap_and_join_parsed() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <polyline points='0,0 10,10' fill='none' stroke='black' \
             stroke-linecap='round' stroke-linejoin='bevel'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(matches!(svg_shapes(&cmds)[0],
            PaintCmd::SvgShape { stroke_cap: SvgLineCap::Round, stroke_join: SvgLineJoin::Bevel, .. }));
    }
}
