//! Rasterization (M5 §4 + E2-M5): a white canvas, fill-rects / gradients /
//! rounded rects / box-shadows via tiny-skia, opacity layers, and manual
//! src-over blitting of fontdue glyph coverage masks.

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop as SkStop, LineCap, LineJoin,
    LinearGradient as SkGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient as SkRadial, Rect as SkRect, Shader, SpreadMode, Transform,
};

use starfish_layout::{FontQuery, Rect};
use starfish_style::{LinearGradient, Rgba};

use crate::display::{
    GradKind, GradUnits, PaintCmd, SvgFillRule, SvgGeom, SvgGradient, SvgLineCap, SvgLineJoin,
    SvgPaint,
};
use crate::svg_path::PathOp;
use crate::font::{FontDb, GlyphBitmap};
use crate::image_store::ImageStore;

/// Paint the display list onto a fresh `width × height` white pixmap.
pub fn rasterize(
    cmds: &[PaintCmd],
    width: u32,
    height: u32,
    fonts: &FontDb,
    images: &ImageStore,
) -> Pixmap {
    // Dimensions are clamped to a sane range by callers, so this normally
    // succeeds; fall back to a 1×1 pixmap rather than panicking on bad input.
    let mut base = Pixmap::new(width, height)
        .or_else(|| Pixmap::new(1, 1))
        .expect("1x1 pixmap always allocatable");
    base.fill(Color::WHITE);

    // Layer stack: pushed layers are transparent full-size pixmaps; all draws
    // target the top of the stack (or `base` if empty). Each entry records how
    // it pops — by opacity (E2-M5 §4.3) or by a transform matrix (E5-M3 §4).
    let mut stack: Vec<(Pixmap, LayerPop)> = Vec::new();

    for cmd in cmds {
        match cmd {
            PaintCmd::PushLayer { opacity } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::Opacity(*opacity)));
                }
            }
            PaintCmd::PushTransform { matrix } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::Transform(*matrix)));
                }
            }
            PaintCmd::PopLayer | PaintCmd::PopTransform => {
                if let Some((layer, pop)) = stack.pop() {
                    let dst = stack.last_mut().map(|(p, _)| p).unwrap_or(&mut base);
                    match pop {
                        LayerPop::Opacity(o) => composite_layer(dst, &layer, o),
                        LayerPop::Transform(m) => composite_transform_layer(dst, &layer, m),
                    }
                }
            }
            other => {
                let target = stack.last_mut().map(|(p, _)| p).unwrap_or(&mut base);
                draw_into(target, other, fonts, images);
            }
        }
    }
    base
}

/// How a pushed layer is composited back when popped.
enum LayerPop {
    /// Scale the layer's contribution by this opacity (group opacity).
    Opacity(f32),
    /// Composite through this `[a,b,c,d,e,f]` transform matrix (E5-M3).
    Transform([f32; 6]),
}

/// Draw a single (non-layer) command into `pixmap`.
fn draw_into(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb, images: &ImageStore) {
    match cmd {
        PaintCmd::FillRect { rect, color, radius } => fill_rect_rounded(pixmap, rect, *color, radius),
        PaintCmd::GradientRect { rect, gradient, radius } => {
            fill_gradient(pixmap, rect, gradient, radius)
        }
        PaintCmd::FillRing { outer, outer_radius, inner, inner_radius, color } => {
            fill_ring(pixmap, outer, outer_radius, inner, inner_radius, *color)
        }
        PaintCmd::BoxShadow { rect, radius, color, blur, spread, offset } => {
            draw_box_shadow(pixmap, rect, radius, *color, *blur, *spread, *offset)
        }
        PaintCmd::GlyphRun { .. } => draw_glyph_run(pixmap, cmd, fonts),
        PaintCmd::ImageBlit { dest, src } => blit_image(pixmap, dest, src, images),
        PaintCmd::SvgShape {
            geom,
            transform,
            fill,
            fill_rule,
            stroke,
            stroke_width,
            stroke_cap,
            stroke_join,
            bbox,
        } => draw_svg_shape(
            pixmap,
            geom,
            transform,
            fill.as_ref(),
            *fill_rule,
            stroke.as_ref(),
            *stroke_width,
            *stroke_cap,
            *stroke_join,
            bbox,
        ),
        PaintCmd::PushLayer { .. }
        | PaintCmd::PopLayer
        | PaintCmd::PushTransform { .. }
        | PaintCmd::PopTransform => {} // handled by caller
    }
}

fn radius_is_zero(r: &[f32; 4]) -> bool {
    r.iter().all(|v| *v <= 0.0)
}

/// Fill a rect, sharp (the existing fast path, byte-identical) or rounded.
fn fill_rect_rounded(pixmap: &mut Pixmap, rect: &Rect, color: Rgba, radius: &[f32; 4]) {
    if radius_is_zero(radius) {
        fill_rect(pixmap, rect, color);
    } else if let Some(path) = rounded_rect_path(rect, radius) {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Fill a rounded border ring: the outer rounded path minus the inner
/// (padding-box) rounded path, via an even-odd fill so the interior is left
/// untouched. A degenerate inner rect (thick border on a small box) leaves no
/// hole, so the outer path fills solid. (E2-M5 §5.2)
fn fill_ring(
    pixmap: &mut Pixmap,
    outer: &Rect,
    outer_radius: &[f32; 4],
    inner: &Rect,
    inner_radius: &[f32; 4],
    color: Rgba,
) {
    let Some(mut pb) = rounded_rect_pathbuilder(outer, outer_radius) else { return };
    // Append the inner subpath only if it's non-degenerate; with even-odd this
    // carves the hole. If absent, the border fills the whole rounded box.
    if inner.width > 0.0 && inner.height > 0.0 {
        append_rounded_rect(&mut pb, inner, inner_radius);
    }
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
}

fn fill_rect(pixmap: &mut Pixmap, rect: &Rect, color: Rgba) {
    let mut paint = Paint {
        anti_alias: false,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    if let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0)) {
        pixmap.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// Fill `rect` (sharp or rounded) with a linear-gradient shader.
fn fill_gradient(pixmap: &mut Pixmap, rect: &Rect, gradient: &LinearGradient, radius: &[f32; 4]) {
    let Some(shader) = gradient_shader(gradient, rect) else { return };
    let paint = Paint {
        anti_alias: true,
        shader,
        ..Default::default()
    };
    if radius_is_zero(radius) {
        if let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0))
        {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    } else if let Some(path) = rounded_rect_path(rect, radius) {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Map the CSS gradient angle to start/end points across `rect` and build a
/// tiny-skia linear-gradient shader. (E2-M5 §1.5)
fn gradient_shader(g: &LinearGradient, rect: &Rect) -> Option<Shader<'static>> {
    let (w, h) = (rect.width, rect.height);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // CSS: 0deg = "to top". In device space y grows downward, so dir for angle θ
    // (clockwise) is (sin θ, -cos θ).
    let theta = g.angle_deg.to_radians();
    let (dx, dy) = (theta.sin(), -theta.cos());
    // CSS "magic corners": gradient-line length = |w·sinθ| + |h·cosθ|.
    let len = (w * theta.sin().abs()) + (h * theta.cos().abs());
    let (cx, cy) = (rect.x + w / 2.0, rect.y + h / 2.0);
    let start = Point::from_xy(cx - dx * len / 2.0, cy - dy * len / 2.0);
    let end = Point::from_xy(cx + dx * len / 2.0, cy + dy * len / 2.0);

    let stops = resolve_stops(&g.stops);
    SkGradient::new(start, end, stops, SpreadMode::Pad, Transform::identity())
}

/// Resolve our stops (with optional positions) to tiny-skia stops with
/// monotonic 0..1 positions: `None` positions are spread evenly; positions are
/// clamped non-decreasing (CSS rule). (E2-M5 §1.5)
fn resolve_stops(stops: &[starfish_style::GradientStop]) -> Vec<SkStop> {
    let n = stops.len();
    let denom = (n.saturating_sub(1)).max(1) as f32;
    let mut pos: Vec<f32> = (0..n)
        .map(|i| stops[i].pos.unwrap_or(i as f32 / denom))
        .collect();
    for i in 1..n {
        if pos[i] < pos[i - 1] {
            pos[i] = pos[i - 1];
        }
    }
    stops
        .iter()
        .zip(pos)
        .map(|(s, p)| {
            SkStop::new(
                p.clamp(0.0, 1.0),
                Color::from_rgba8(s.color.r, s.color.g, s.color.b, s.color.a),
            )
        })
        .collect()
}

/// Build a rounded-rect path with circular corner arcs (cubic Béziers, κ≈0.5523).
/// Radii are clamped so opposite radii on a side don't exceed it. (E2-M5 §2.4)
fn rounded_rect_path(rect: &Rect, radius: &[f32; 4]) -> Option<tiny_skia::Path> {
    rounded_rect_pathbuilder(rect, radius)?.finish()
}

/// Like `rounded_rect_path` but returns the open `PathBuilder` so a second
/// subpath (e.g. a ring's inner hole) can be appended before finishing.
fn rounded_rect_pathbuilder(rect: &Rect, radius: &[f32; 4]) -> Option<PathBuilder> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let mut pb = PathBuilder::new();
    append_rounded_rect(&mut pb, rect, radius);
    Some(pb)
}

/// Append a closed rounded-rect subpath (circular corner arcs, κ≈0.5523) to
/// `pb`. Radii are clamped so opposite radii on a side don't exceed it.
fn append_rounded_rect(pb: &mut PathBuilder, rect: &Rect, radius: &[f32; 4]) {
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    let m = (w.min(h)) / 2.0;
    let tl = radius[0].clamp(0.0, m);
    let tr = radius[1].clamp(0.0, m);
    let br = radius[2].clamp(0.0, m);
    let bl = radius[3].clamp(0.0, m);
    const K: f32 = 0.552_284_8;
    pb.move_to(x + tl, y);
    pb.line_to(x + w - tr, y); // top edge
    pb.cubic_to(x + w - tr + tr * K, y, x + w, y + tr - tr * K, x + w, y + tr); // TR
    pb.line_to(x + w, y + h - br); // right edge
    pb.cubic_to(x + w, y + h - br + br * K, x + w - br + br * K, y + h, x + w - br, y + h); // BR
    pb.line_to(x + bl, y + h); // bottom edge
    pb.cubic_to(x + bl - bl * K, y + h, x, y + h - bl + bl * K, x, y + h - bl); // BL
    pb.line_to(x, y + tl); // left edge
    pb.cubic_to(x, y + tl - tl * K, x + tl - tl * K, y, x + tl, y); // TL
    pb.close();
}

/// Draw one flattened SVG shape (E9-M1 §5.5): build the geometry path in user
/// coords, then fill / stroke it through the viewBox `transform`. The transform
/// scales both geometry and (uniform `meet`) stroke width.
#[allow(clippy::too_many_arguments)]
fn draw_svg_shape(
    pixmap: &mut Pixmap,
    geom: &SvgGeom,
    transform: &[f32; 6],
    fill: Option<&SvgPaint>,
    fill_rule: SvgFillRule,
    stroke: Option<&SvgPaint>,
    stroke_width: f32,
    cap: SvgLineCap,
    join: SvgLineJoin,
    bbox: &Rect,
) {
    let Some(path) = svg_path(geom) else { return };
    let m = transform;
    let t = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);

    if let Some(paint) = svg_fill_paint(fill, bbox) {
        let fr = match fill_rule {
            SvgFillRule::NonZero => FillRule::Winding,
            SvgFillRule::EvenOdd => FillRule::EvenOdd,
        };
        pixmap.fill_path(&path, &paint, fr, t, None);
    }
    if let Some(paint) = svg_fill_paint(stroke, bbox) {
        if stroke_width > 0.0 {
            let sk = tiny_skia::Stroke {
                width: stroke_width,
                line_cap: match cap {
                    SvgLineCap::Butt => LineCap::Butt,
                    SvgLineCap::Round => LineCap::Round,
                    SvgLineCap::Square => LineCap::Square,
                },
                line_join: match join {
                    SvgLineJoin::Miter => LineJoin::Miter,
                    SvgLineJoin::Round => LineJoin::Round,
                    SvgLineJoin::Bevel => LineJoin::Bevel,
                },
                ..Default::default()
            };
            pixmap.stroke_path(&path, &paint, &sk, t, None);
        }
    }
}

/// Build a `Paint` for an SVG fill/stroke (E9-M3 §4.4): a solid color or a
/// gradient shader (in user/bbox space; the effective transform maps it to
/// canvas). `None` for `None`, a fully transparent color, or a degenerate
/// gradient.
fn svg_fill_paint(paint: Option<&SvgPaint>, bbox: &Rect) -> Option<Paint<'static>> {
    match paint? {
        SvgPaint::Color(c) => {
            if c.a == 0 {
                return None;
            }
            let mut p = Paint { anti_alias: true, ..Default::default() };
            p.set_color_rgba8(c.r, c.g, c.b, c.a);
            Some(p)
        }
        SvgPaint::Gradient(g) => {
            let shader = svg_gradient_shader(g, bbox)?;
            Some(Paint { anti_alias: true, shader, ..Default::default() })
        }
    }
}

/// Build the tiny-skia gradient shader for an `SvgGradient`, mapping
/// objectBoundingBox coords through the shape's bbox (E9-M3 §4.4). The shader
/// transform is identity; the caller's effective transform composes on top.
fn svg_gradient_shader(g: &SvgGradient, bbox: &Rect) -> Option<Shader<'static>> {
    let stops = resolve_stops(&g.stops);
    let map = |ux: f32, uy: f32| -> (f32, f32) {
        match g.units {
            GradUnits::ObjectBoundingBox => {
                (bbox.x + ux * bbox.width, bbox.y + uy * bbox.height)
            }
            GradUnits::UserSpaceOnUse => (ux, uy),
        }
    };
    match g.kind {
        GradKind::Linear { x1, y1, x2, y2 } => {
            let (sx, sy) = map(x1, y1);
            let (ex, ey) = map(x2, y2);
            SkGradient::new(
                Point::from_xy(sx, sy),
                Point::from_xy(ex, ey),
                stops,
                SpreadMode::Pad,
                Transform::identity(),
            )
        }
        GradKind::Radial { cx, cy, r } => {
            let (ccx, ccy) = map(cx, cy);
            // objBox radius: scale by the averaged bbox extent (a true objBox
            // radial is elliptical — M3 approximates, §6).
            let rr = match g.units {
                GradUnits::ObjectBoundingBox => r * (bbox.width + bbox.height) / 2.0,
                GradUnits::UserSpaceOnUse => r,
            };
            let c = Point::from_xy(ccx, ccy);
            SkRadial::new(c, c, rr, stops, SpreadMode::Pad, Transform::identity())
        }
    }
}

/// Build a tiny-skia path for an SVG shape in user coords. `None` for a
/// degenerate shape (zero-size rect / non-finite coords).
fn svg_path(geom: &SvgGeom) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    match geom {
        &SvgGeom::Rect { x, y, w, h, rx, ry } => {
            let rect = Rect { x, y, width: w, height: h };
            if rx <= 0.0 && ry <= 0.0 {
                let sr = SkRect::from_xywh(x, y, w, h)?;
                pb.push_rect(sr);
            } else {
                // rx/ry rounded rect: the circular-corner helper approximates
                // asymmetric radii with a single corner radius (E9-M1 §5.5 note).
                let r = rx.max(ry);
                append_rounded_rect(&mut pb, &rect, &[r; 4]);
            }
        }
        &SvgGeom::Ellipse { cx, cy, rx, ry } => {
            let sr = SkRect::from_xywh(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry)?;
            pb.push_oval(sr);
        }
        &SvgGeom::Line { x1, y1, x2, y2 } => {
            pb.move_to(x1, y1);
            pb.line_to(x2, y2);
        }
        SvgGeom::Path(ops) => {
            for op in ops {
                match *op {
                    PathOp::MoveTo(x, y) => pb.move_to(x, y),
                    PathOp::LineTo(x, y) => pb.line_to(x, y),
                    PathOp::QuadTo(cx, cy, x, y) => pb.quad_to(cx, cy, x, y),
                    PathOp::CubicTo(a, b, c, d, x, y) => pb.cubic_to(a, b, c, d, x, y),
                    PathOp::Close => pb.close(),
                }
            }
        }
    }
    pb.finish()
}

/// Render the (rounded) shadow shape into an alpha mask, blur it, then composite
/// the shadow color through it. (E2-M5 §3.4)
fn draw_box_shadow(
    pixmap: &mut Pixmap,
    rect: &Rect,
    radius: &[f32; 4],
    color: Rgba,
    blur: f32,
    spread: f32,
    offset: (f32, f32),
) {
    if color.a == 0 {
        return;
    }
    // Shadow rect: grow by spread on all sides, offset; corner radii += spread.
    let sr = Rect {
        x: rect.x - spread + offset.0,
        y: rect.y - spread + offset.1,
        width: rect.width + 2.0 * spread,
        height: rect.height + 2.0 * spread,
    };
    if sr.width <= 0.0 || sr.height <= 0.0 {
        return;
    }
    let srad = [
        (radius[0] + spread).max(0.0),
        (radius[1] + spread).max(0.0),
        (radius[2] + spread).max(0.0),
        (radius[3] + spread).max(0.0),
    ];
    let Some(path) = rounded_rect_path(&sr, &srad) else { return };
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else { return };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    if blur > 0.0 {
        // Clamp the blur radius: blurring further than the pixmap's largest
        // dimension can't change the (already-saturated) coverage, and an
        // unclamped huge `blur` (e.g. 99999999px) overflows the box-blur window
        // arithmetic. Bound `r` to the pixmap extent.
        let cap = pixmap.width().max(pixmap.height()) as i32;
        let r = ((blur / 2.0).round() as i32).clamp(0, cap.max(1));
        box_blur_mask(&mut mask, pixmap.width(), pixmap.height(), r);
    }
    composite_mask_color(pixmap, &mask, color);
}

/// 3-pass separable box blur over the mask's `u8` coverage (approximate
/// Gaussian). O(W·H) per pass independent of radius. (E2-M5 §3.4)
fn box_blur_mask(mask: &mut Mask, w: u32, h: u32, r: i32) {
    if r <= 0 {
        return;
    }
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return;
    }
    let data = mask.data_mut();
    let mut tmp = vec![0u8; w * h];
    for _ in 0..3 {
        // horizontal pass: data -> tmp
        box_blur_pass_h(data, &mut tmp, w, h, r as usize);
        // vertical pass: tmp -> data
        box_blur_pass_v(&tmp, data, w, h, r as usize);
    }
}

fn box_blur_pass_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    // u64 accumulation: window (2r+1) × 255 can exceed u32 for very large `r`.
    let window = (2 * r + 1) as u64;
    for y in 0..h {
        let row = y * w;
        let mut sum: u64 = 0;
        // initial window [-r ..= r] clamped to edges
        for x in 0..=r.min(w - 1) {
            sum += src[row + x] as u64;
        }
        // pre-roll: positions [-r..0] are clamped to col 0
        sum += src[row] as u64 * r as u64;
        for x in 0..w {
            dst[row + x] = (sum / window) as u8;
            let add_idx = (x + r + 1).min(w - 1);
            let sub_idx = x.saturating_sub(r);
            sum += src[row + add_idx] as u64;
            sum -= src[row + sub_idx] as u64;
        }
    }
}

fn box_blur_pass_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u64;
    for x in 0..w {
        let mut sum: u64 = 0;
        for y in 0..=r.min(h - 1) {
            sum += src[y * w + x] as u64;
        }
        sum += src[x] as u64 * r as u64;
        for y in 0..h {
            dst[y * w + x] = (sum / window) as u8;
            let add_idx = (y + r + 1).min(h - 1);
            let sub_idx = y.saturating_sub(r);
            sum += src[add_idx * w + x] as u64;
            sum -= src[sub_idx * w + x] as u64;
        }
    }
}

/// Composite a solid color through a coverage mask via per-pixel src-over.
fn composite_mask_color(pixmap: &mut Pixmap, mask: &Mask, color: Rgba) {
    let w = pixmap.width() as usize;
    let h = pixmap.height() as usize;
    let cov = mask.data();
    let buf = pixmap.data_mut();
    for (i, &c) in cov.iter().enumerate().take(w * h) {
        if c == 0 {
            continue;
        }
        let a = (c as u32 * color.a as u32 / 255) as u8;
        if a == 0 {
            continue;
        }
        src_over_pixel(buf, i * 4, color, a);
    }
}

/// Composite a transparent layer pixmap onto `dst`, scaling the layer's
/// contribution by `opacity` (correct group opacity). (E2-M5 §4.3)
fn composite_layer(dst: &mut Pixmap, layer: &Pixmap, opacity: f32) {
    let paint = PixmapPaint {
        opacity: opacity.clamp(0.0, 1.0),
        ..Default::default()
    };
    dst.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
}

/// Composite a transform layer onto `dst` through its `[a,b,c,d,e,f]` matrix,
/// using bilinear filtering so rotation/scale don't alias (E5-M3 §4). A
/// non-finite (NaN/inf) matrix is skipped (no panic); a singular/degenerate
/// matrix maps the source to a zero-area region (`draw_pixmap` draws nothing).
fn composite_transform_layer(dst: &mut Pixmap, layer: &Pixmap, m: [f32; 6]) {
    let t = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);
    if !t.is_finite() {
        return;
    }
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    dst.draw_pixmap(0, 0, layer.as_ref(), &paint, t, None);
}

fn draw_glyph_run(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb) {
    let PaintCmd::GlyphRun {
        origin,
        text,
        font_size,
        weight,
        style,
        family,
        color,
        ascent,
        letter_spacing,
        word_spacing,
    } = cmd
    else {
        return;
    };
    // Rebuild the identical FontQuery the measurer used → same resolved face.
    let q = FontQuery {
        family,
        style: *style,
        weight: *weight,
        size: *font_size,
        letter_spacing: *letter_spacing,
        word_spacing: *word_spacing,
    };
    let baseline = origin.1 + ascent;
    let mut pen_x = origin.0;
    for ch in text.chars() {
        let g = fonts.rasterize_glyph(ch, &q);
        if g.width > 0 && g.height > 0 {
            let gx = (pen_x + g.left as f32).round() as i32;
            let gy = (baseline - g.top as f32).round() as i32;
            blit_coverage(pixmap, &g, gx, gy, *color);
        }
        // Same additive formula as the measurer → measure == paint (§4.3).
        pen_x += g.advance + letter_spacing + if ch == ' ' { *word_spacing } else { 0.0 };
    }
}

/// Nearest-neighbour scale the decoded RGBA into `dest`, src-over into the
/// pixmap, clipped to bounds. Missing image (broken) → no-op.
fn blit_image(pixmap: &mut Pixmap, dest: &Rect, src: &str, images: &ImageStore) {
    let Some(img) = images.peek(src) else { return };
    if img.width == 0 || img.height == 0 {
        return;
    }
    let dx0 = dest.x.round() as i32;
    let dy0 = dest.y.round() as i32;
    let dw = dest.width.round().max(0.0) as i32;
    let dh = dest.height.round().max(0.0) as i32;
    if dw == 0 || dh == 0 {
        return;
    }
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    // Clamp the iteration to the visible pixmap region so a huge `dw`/`dh`
    // (e.g. an upscaled-to-1e8 image) doesn't spin through trillions of
    // offscreen pixels. The scale ratio still uses the full `dw`/`dh` below so
    // the visible portion samples the source correctly.
    let x_start = dx0.max(0);
    let x_end = (dx0 + dw).min(pw);
    let y_start = dy0.max(0);
    let y_end = (dy0 + dh).min(ph);
    if x_start >= x_end || y_start >= y_end {
        return; // fully offscreen
    }
    let buf = pixmap.data_mut();
    for py in y_start..y_end {
        let ry = py - dy0;
        let sy = (ry as u32 * img.height / dh as u32).min(img.height - 1);
        for px in x_start..x_end {
            let rx = px - dx0;
            let sx = (rx as u32 * img.width / dw as u32).min(img.width - 1);
            let si = ((sy * img.width + sx) * 4) as usize;
            let (r, g, b, a) = (img.rgba[si], img.rgba[si + 1], img.rgba[si + 2], img.rgba[si + 3]);
            if a == 0 {
                continue;
            }
            let idx = ((py * pw + px) as usize) * 4;
            src_over_pixel(buf, idx, Rgba { r, g, b, a }, a);
        }
    }
}

/// Per-pixel src-over of `color` weighted by glyph coverage, clipped to bounds.
fn blit_coverage(pixmap: &mut Pixmap, g: &GlyphBitmap, gx: i32, gy: i32, color: Rgba) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let buf = pixmap.data_mut();
    for row in 0..g.height as i32 {
        let py = gy + row;
        if py < 0 || py >= ph {
            continue;
        }
        for col in 0..g.width as i32 {
            let px = gx + col;
            if px < 0 || px >= pw {
                continue;
            }
            let cov = g.coverage[row as usize * g.width + col as usize];
            if cov == 0 {
                continue;
            }
            let a = (cov as u32 * color.a as u32 / 255) as u8;
            if a == 0 {
                continue;
            }
            let idx = ((py * pw + px) as usize) * 4;
            src_over_pixel(buf, idx, color, a);
        }
    }
}

/// Composite a straight-color source (premultiplied by `a`) over the
/// premultiplied destination pixel at `idx`: `dst = src*a + dst*(1-a)`.
fn src_over_pixel(buf: &mut [u8], idx: usize, color: Rgba, a: u8) {
    let inv = 255 - a as u32;
    // source premultiplied by its alpha:
    let sr = color.r as u32 * a as u32 / 255;
    let sg = color.g as u32 * a as u32 / 255;
    let sb = color.b as u32 * a as u32 / 255;
    buf[idx] = (sr + buf[idx] as u32 * inv / 255) as u8;
    buf[idx + 1] = (sg + buf[idx + 1] as u32 * inv / 255) as u8;
    buf[idx + 2] = (sb + buf[idx + 2] as u32 * inv / 255) as u8;
    buf[idx + 3] = (a as u32 + buf[idx + 3] as u32 * inv / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_layout::Rect;
    use starfish_net::{file_url_from_path, LocalLoader, Url};
    use std::path::Path;

    /// An empty store with a throwaway `file:///` base (no fetches happen).
    fn empty_store() -> ImageStore<'static> {
        ImageStore::new(Url::parse("file:///").unwrap(), &LocalLoader)
    }

    /// A store whose `file://` base is the document `index.html` in `dir`.
    fn store_for(dir: &Path) -> ImageStore<'static> {
        ImageStore::new(file_url_from_path(&dir.join("index.html")).unwrap(), &LocalLoader)
    }

    #[test]
    fn blend_black_over_white() {
        // 1x1 white pixmap, blit full-coverage black → black.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(&mut buf, 0, Rgba { r: 0, g: 0, b: 0, a: 255 }, 255);
        assert_eq!(buf, vec![0, 0, 0, 255]);
    }

    #[test]
    fn blend_half_coverage() {
        // half coverage of red over white → ~ (255,128,128) premultiplied.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(&mut buf, 0, Rgba { r: 255, g: 0, b: 0, a: 255 }, 128);
        assert_eq!(buf[0], 255); // red stays full
        assert!(buf[1] > 120 && buf[1] < 135);
        assert!(buf[2] > 120 && buf[2] < 135);
        assert_eq!(buf[3], 255);
    }

    #[test]
    fn blit_downscale_samples_correct_color() {
        // 4×4 source (left half red, right half blue) downscaled to a 2×2 dest.
        // Nearest-neighbour: dest col 0 → src col 0 (red), dest col 1 → src col 2
        // (blue).
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-blit-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let px = if x < 2 {
                    image::Rgba([255, 0, 0, 255])
                } else {
                    image::Rgba([0, 0, 255, 255])
                };
                img.put_pixel(x, y, px);
            }
        }
        img.save(dir.join("q.png")).unwrap();

        let mut images = store_for(&dir);
        images.get("q.png").expect("decoded 4x4");

        let mut pm = Pixmap::new(2, 2).unwrap();
        pm.fill(Color::WHITE);
        blit_image(
            &mut pm,
            &Rect { x: 0.0, y: 0.0, width: 2.0, height: 2.0 },
            "q.png",
            &images,
        );
        let p0 = pm.pixel(0, 0).unwrap();
        let p1 = pm.pixel(1, 0).unwrap();
        assert_eq!((p0.red(), p0.green(), p0.blue()), (255, 0, 0)); // red half
        assert_eq!((p1.red(), p1.green(), p1.blue()), (0, 0, 255)); // blue half
    }

    #[test]
    fn fill_rect_paints_red() {
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::FillRect {
            rect: Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 },
            color: Rgba { r: 255, g: 0, b: 0, a: 255 },
            radius: [0.0; 4],
        }];
        let pm = rasterize(&cmds, 4, 4, &fonts, &empty_store());
        let px = pm.pixel(1, 1).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
    }

    #[test]
    fn rounded_rect_corner_is_transparent_through() {
        // A red rounded fill on white: the extreme corner stays white, the
        // center is red.
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::FillRect {
            rect: Rect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
            color: Rgba { r: 255, g: 0, b: 0, a: 255 },
            radius: [40.0; 4],
        }];
        let pm = rasterize(&cmds, 100, 100, &fonts, &empty_store());
        let corner = pm.pixel(1, 1).unwrap();
        assert_eq!((corner.red(), corner.green(), corner.blue()), (255, 255, 255));
        let center = pm.pixel(50, 50).unwrap();
        assert_eq!((center.red(), center.green(), center.blue()), (255, 0, 0));
    }

    #[test]
    fn box_blur_spreads_alpha() {
        // A crisp 255-block in the middle, blurred → edge values become partial.
        let mut mask = Mask::new(20, 20).unwrap();
        {
            let d = mask.data_mut();
            for y in 8..12 {
                for x in 8..12 {
                    d[y * 20 + x] = 255;
                }
            }
        }
        box_blur_mask(&mut mask, 20, 20, 3);
        let d = mask.data();
        // a pixel just outside the original block is now non-zero (spread).
        assert!(d[10 * 20 + 6] > 0, "blur should spread coverage outward");
    }
}
