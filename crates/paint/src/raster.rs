//! Rasterization (M5 §4 + E2-M5): a white canvas, fill-rects / gradients /
//! rounded rects / box-shadows via tiny-skia, opacity layers, and manual
//! src-over blitting of fontdue glyph coverage masks.

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop as SkStop, LineCap, LineJoin,
    LinearGradient as SkGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient as SkRadial, Rect as SkRect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use starfish_layout::{FontQuery, Rect};
use starfish_style::{BorderStyle, ConicGradient, LinearGradient, RadialGradient, Rgba};

use crate::display::{
    GradKind, GradUnits, PaintCmd, SvgFillRule, SvgGeom, SvgGradient, SvgLineCap, SvgLineJoin,
    SvgPaint,
};
use crate::font::{FontDb, GlyphBitmap};
use crate::image_store::ImageStore;
use crate::svg_path::PathOp;
use starfish_dom::{CanvasColor, CanvasOp};

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
            PaintCmd::PushClip { rect, radius } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::Clip(*rect, *radius)));
                }
            }
            PaintCmd::PopLayer | PaintCmd::PopTransform | PaintCmd::PopClip => {
                if let Some((layer, pop)) = stack.pop() {
                    let dst = stack.last_mut().map(|(p, _)| p).unwrap_or(&mut base);
                    match pop {
                        LayerPop::Opacity(o) => composite_layer(dst, &layer, o),
                        LayerPop::Transform(m) => composite_transform_layer(dst, &layer, m),
                        LayerPop::Clip(rect, radius) => {
                            composite_clip_layer(dst, &layer, &rect, &radius, width, height)
                        }
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
    /// Composite clipped to this padding-box rect + corner radii (E13-M4).
    Clip(Rect, [f32; 4]),
}

/// Draw a single (non-layer) command into `pixmap`.
fn draw_into(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb, images: &ImageStore) {
    match cmd {
        PaintCmd::FillRect {
            rect,
            color,
            radius,
        } => fill_rect_rounded(pixmap, rect, *color, radius),
        PaintCmd::StrokeLine {
            from,
            to,
            width,
            color,
            style,
        } => draw_stroke_line(pixmap, *from, *to, *width, *color, *style),
        PaintCmd::GradientRect {
            rect,
            gradient,
            radius,
        } => fill_gradient(pixmap, rect, gradient, radius),
        PaintCmd::RadialRect {
            rect,
            gradient,
            radius,
        } => fill_radial(pixmap, rect, gradient, radius),
        PaintCmd::ConicRect {
            rect,
            gradient,
            radius,
        } => fill_conic(pixmap, rect, gradient, radius),
        PaintCmd::GlyphShadow { .. } => draw_glyph_shadow(pixmap, cmd, fonts),
        PaintCmd::FillRing {
            outer,
            outer_radius,
            inner,
            inner_radius,
            color,
        } => fill_ring(pixmap, outer, outer_radius, inner, inner_radius, *color),
        PaintCmd::BoxShadow {
            rect,
            radius,
            color,
            blur,
            spread,
            offset,
        } => draw_box_shadow(pixmap, rect, radius, *color, *blur, *spread, *offset),
        PaintCmd::GlyphRun { .. } => draw_glyph_run(pixmap, cmd, fonts),
        PaintCmd::ImageBlit {
            dest,
            src,
            src_crop,
            smooth,
        } => blit_image(pixmap, dest, src, src_crop, *smooth, images),
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
        PaintCmd::Canvas { rect, backing, ops } => replay_canvas(pixmap, rect, *backing, ops),
        PaintCmd::PushLayer { .. }
        | PaintCmd::PopLayer
        | PaintCmd::PushTransform { .. }
        | PaintCmd::PopTransform
        | PaintCmd::PushClip { .. }
        | PaintCmd::PopClip => {} // handled by caller
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
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
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
    let Some(mut pb) = rounded_rect_pathbuilder(outer, outer_radius) else {
        return;
    };
    // Append the inner subpath only if it's non-degenerate; with even-odd this
    // carves the hole. If absent, the border fills the whole rounded box.
    if inner.width > 0.0 && inner.height > 0.0 {
        append_rounded_rect(&mut pb, inner, inner_radius);
    }
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::EvenOdd,
        Transform::identity(),
        None,
    );
}

/// Draw a non-solid border edge (`dashed`/`dotted`/`double`, E13-M4). `from`/`to`
/// is the edge center line; `width` is the border width. Axis-aligned edges only
/// (the only producer is `emit_borders_stroke`).
fn draw_stroke_line(
    pixmap: &mut Pixmap,
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
    color: Rgba,
    style: BorderStyle,
) {
    if width <= 0.0 || color.a == 0 {
        return;
    }
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);

    match style {
        BorderStyle::Double => {
            // Two parallel solid strokes, each width/3, offset ±width/3 along the
            // edge normal (horizontal edge → normal (0,±1); vertical → (±1,0)).
            let stroke = Stroke {
                width: (width / 3.0).max(0.5),
                ..Default::default()
            };
            let off = width / 3.0;
            let (nx, ny) = if (from.1 - to.1).abs() <= f32::EPSILON {
                (0.0, off) // horizontal edge
            } else {
                (off, 0.0) // vertical edge
            };
            for s in [-1.0_f32, 1.0] {
                let mut pb = PathBuilder::new();
                pb.move_to(from.0 + s * nx, from.1 + s * ny);
                pb.line_to(to.0 + s * nx, to.1 + s * ny);
                if let Some(path) = pb.finish() {
                    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        BorderStyle::Dashed | BorderStyle::Dotted => {
            let (dash, cap) = if style == BorderStyle::Dotted {
                (
                    StrokeDash::new(vec![width, width * 2.0], 0.0),
                    LineCap::Round,
                )
            } else {
                (
                    StrokeDash::new(vec![width * 3.0, width * 3.0], 0.0),
                    LineCap::Butt,
                )
            };
            let Some(dash) = dash else { return };
            let stroke = Stroke {
                width,
                line_cap: cap,
                dash: Some(dash),
                ..Default::default()
            };
            let mut pb = PathBuilder::new();
            pb.move_to(from.0, from.1);
            pb.line_to(to.0, to.1);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        BorderStyle::None | BorderStyle::Solid => {}
    }
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
    let Some(shader) = gradient_shader(gradient, rect) else {
        return;
    };
    let paint = Paint {
        anti_alias: true,
        shader,
        ..Default::default()
    };
    if radius_is_zero(radius) {
        if let Some(r) =
            SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0))
        {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    } else if let Some(path) = rounded_rect_path(rect, radius) {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
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

/// Fill `rect` with a radial gradient (E16-M3 MVP): a circle centered on the
/// rect, radius = half the diagonal (farthest-corner). Mirrors `fill_gradient`.
fn fill_radial(pixmap: &mut Pixmap, rect: &Rect, gradient: &RadialGradient, radius: &[f32; 4]) {
    let (w, h) = (rect.width, rect.height);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let center = Point::from_xy(rect.x + w / 2.0, rect.y + h / 2.0);
    let r = 0.5 * (w * w + h * h).sqrt(); // farthest-corner distance
    let stops = resolve_stops(&gradient.stops);
    let Some(shader) = SkRadial::new(
        center,
        center,
        r,
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    let paint = Paint {
        anti_alias: true,
        shader,
        ..Default::default()
    };
    if radius_is_zero(radius) {
        if let Some(rr) = SkRect::from_xywh(rect.x, rect.y, w.max(0.0), h.max(0.0)) {
            pixmap.fill_rect(rr, &paint, Transform::identity(), None);
        }
    } else if let Some(path) = rounded_rect_path(rect, radius) {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Number of angular wedges for the conic-gradient triangle fan (E16-M3 MVP).
const CONIC_WEDGES: usize = 180;

/// Fill `rect` with a conic gradient (E16-M3 MVP): a triangle fan of flat-colored
/// wedges around the rect center, clipped to the (rounded) rect via a mask. Each
/// wedge is colored by sampling the resolved stop ramp at its turn-fraction
/// (0deg = up, clockwise, offset by `from_deg`).
fn fill_conic(pixmap: &mut Pixmap, rect: &Rect, gradient: &ConicGradient, radius: &[f32; 4]) {
    let (w, h) = (rect.width, rect.height);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    // Clip mask = the (rounded) rect path.
    let path = if radius_is_zero(radius) {
        let Some(r) = SkRect::from_xywh(rect.x, rect.y, w.max(0.0), h.max(0.0)) else {
            return;
        };
        PathBuilder::from_rect(r)
    } else {
        match rounded_rect_path(rect, radius) {
            Some(p) => p,
            None => return,
        }
    };
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());

    let stops = resolve_stop_ramp(&gradient.stops);
    let cx = rect.x + w / 2.0;
    let cy = rect.y + h / 2.0;
    let outer = (w * w + h * h).sqrt(); // reach every corner
    let from_turn = gradient.from_deg / 360.0;

    for i in 0..CONIC_WEDGES {
        let t0 = i as f32 / CONIC_WEDGES as f32;
        let t1 = (i + 1) as f32 / CONIC_WEDGES as f32;
        let color = conic_color_at(&stops, (t0 + t1) / 2.0 + from_turn);
        // CSS conic: 0 turn = up (-y), growing clockwise. Map turn → device angle.
        let a0 = t0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let a1 = t1 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let mut pb = PathBuilder::new();
        pb.move_to(cx, cy);
        pb.line_to(cx + outer * a0.cos(), cy + outer * a0.sin());
        pb.line_to(cx + outer * a1.cos(), cy + outer * a1.sin());
        pb.close();
        let Some(wedge) = pb.finish() else { continue };
        let paint = Paint {
            anti_alias: false,
            shader: Shader::SolidColor(color),
            ..Default::default()
        };
        pixmap.fill_path(
            &wedge,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            Some(&mask),
        );
    }
}

/// Resolve our stops to `(position, Color)` pairs with monotonic 0..1 positions —
/// the same spacing rule as `resolve_stops`, but keeping our own typed positions
/// so a turn-fraction can be sampled (E16-M3).
fn resolve_stop_ramp(stops: &[starfish_style::GradientStop]) -> Vec<(f32, Color)> {
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
            (
                p.clamp(0.0, 1.0),
                Color::from_rgba8(s.color.r, s.color.g, s.color.b, s.color.a),
            )
        })
        .collect()
}

/// Sample the resolved `(position, Color)` ramp at turn-fraction `turn` (wrapped
/// to 0..1), linearly interpolating between adjacent stops (E16-M3).
fn conic_color_at(stops: &[(f32, Color)], turn: f32) -> Color {
    let t = turn.rem_euclid(1.0);
    let Some(&(first_pos, first_col)) = stops.first() else {
        return Color::TRANSPARENT;
    };
    let &(last_pos, last_col) = stops.last().unwrap();
    if t <= first_pos {
        return first_col;
    }
    if t >= last_pos {
        return last_col;
    }
    for pair in stops.windows(2) {
        let (pa, ca) = pair[0];
        let (pb, cb) = pair[1];
        if t >= pa && t <= pb {
            let span = pb - pa;
            let f = if span > 0.0 { (t - pa) / span } else { 0.0 };
            return Color::from_rgba(
                ca.red() + (cb.red() - ca.red()) * f,
                ca.green() + (cb.green() - ca.green()) * f,
                ca.blue() + (cb.blue() - ca.blue()) * f,
                ca.alpha() + (cb.alpha() - ca.alpha()) * f,
            )
            .unwrap_or(ca);
        }
    }
    last_col
}

/// Render a text-shadow glyph layer (E16-M3): shape the run identically to
/// `draw_glyph_run`, accumulate each glyph's coverage into a full-pixmap mask,
/// blur it (if requested), then composite the shadow color through it.
fn draw_glyph_shadow(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb) {
    let PaintCmd::GlyphShadow {
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
        blur,
    } = cmd
    else {
        return;
    };
    if color.a == 0 {
        return;
    }
    let (pw, ph) = (pixmap.width(), pixmap.height());
    let Some(mut mask) = Mask::new(pw, ph) else {
        return;
    };
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
    let glyphs = fonts.shape(text, &q);
    let mdata = mask.data_mut();
    for g in glyphs.iter() {
        let bmp = fonts.rasterize_indexed_glyph(g.face, g.glyph_id, &q);
        if bmp.width > 0 && bmp.height > 0 {
            let gx = (pen_x + g.x_offset + bmp.left as f32).round() as i32;
            let gy = (baseline - g.y_offset - bmp.top as f32).round() as i32;
            accumulate_coverage(mdata, pw, ph, &bmp, gx, gy);
        }
        pen_x += g.x_advance;
    }
    if *blur > 0.0 {
        let cap = pw.max(ph) as i32;
        let r = ((*blur / 2.0).round() as i32).clamp(0, cap.max(1));
        box_blur_mask(&mut mask, pw, ph, r);
    }
    composite_mask_color(pixmap, &mask, *color);
}

/// Accumulate a glyph coverage bitmap into the full-pixmap mask at `(gx, gy)`,
/// taking the max coverage where glyphs overlap (E16-M3).
fn accumulate_coverage(mask: &mut [u8], pw: u32, ph: u32, g: &GlyphBitmap, gx: i32, gy: i32) {
    let (pw, ph) = (pw as i32, ph as i32);
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
            let cov = g.coverage[(row as usize) * g.width + col as usize];
            if cov == 0 {
                continue;
            }
            let idx = (py * pw + px) as usize;
            if cov > mask[idx] {
                mask[idx] = cov;
            }
        }
    }
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
    pb.cubic_to(
        x + w - tr + tr * K,
        y,
        x + w,
        y + tr - tr * K,
        x + w,
        y + tr,
    ); // TR
    pb.line_to(x + w, y + h - br); // right edge
    pb.cubic_to(
        x + w,
        y + h - br + br * K,
        x + w - br + br * K,
        y + h,
        x + w - br,
        y + h,
    ); // BR
    pb.line_to(x + bl, y + h); // bottom edge
    pb.cubic_to(
        x + bl - bl * K,
        y + h,
        x,
        y + h - bl + bl * K,
        x,
        y + h - bl,
    ); // BL
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
            let mut p = Paint {
                anti_alias: true,
                ..Default::default()
            };
            p.set_color_rgba8(c.r, c.g, c.b, c.a);
            Some(p)
        }
        SvgPaint::Gradient(g) => {
            let shader = svg_gradient_shader(g, bbox)?;
            Some(Paint {
                anti_alias: true,
                shader,
                ..Default::default()
            })
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
            GradUnits::ObjectBoundingBox => (bbox.x + ux * bbox.width, bbox.y + uy * bbox.height),
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
            let rect = Rect {
                x,
                y,
                width: w,
                height: h,
            };
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
    let Some(path) = rounded_rect_path(&sr, &srad) else {
        return;
    };
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
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

/// Composite a clip layer onto `dst`, masked to `rect` (padding box) with corner
/// `radius` (E13-M4). The layer holds the clipped descendants; the mask keeps
/// only pixels inside the clip path. If a mask can't be built, composite
/// unclipped (graceful degradation rather than dropping content).
fn composite_clip_layer(
    dst: &mut Pixmap,
    layer: &Pixmap,
    rect: &Rect,
    radius: &[f32; 4],
    w: u32,
    h: u32,
) {
    let path = if radius_is_zero(radius) {
        let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0))
        else {
            return;
        };
        PathBuilder::from_rect(r)
    } else {
        match rounded_rect_path(rect, radius) {
            Some(p) => p,
            None => return,
        }
    };
    let paint = PixmapPaint::default();
    if let Some(mut mask) = Mask::new(w, h) {
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        dst.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &paint,
            Transform::identity(),
            Some(&mask),
        );
    } else {
        dst.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
    }
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
    // Shape the SAME `text` the measurer summed (E10-M1): one rustybuzz run,
    // rasterized by glyph index. The pen advances by exactly the shaped
    // x_advances, so the painted width matches `advance_width` (measure == paint).
    let glyphs = fonts.shape(text, &q);
    for g in glyphs.iter() {
        let bmp = fonts.rasterize_indexed_glyph(g.face, g.glyph_id, &q);
        if bmp.width > 0 && bmp.height > 0 {
            let gx = (pen_x + g.x_offset + bmp.left as f32).round() as i32;
            // y_offset is +up; the baseline grows downward, so subtract it.
            let gy = (baseline - g.y_offset - bmp.top as f32).round() as i32;
            blit_coverage(pixmap, &bmp, gx, gy, *color);
        }
        pen_x += g.x_advance;
    }
}

/// Scale the `src_crop` sub-rect of the decoded RGBA into `dest`, src-over into
/// the pixmap, clipped to bounds (E15-M1). `smooth` selects bilinear; else
/// nearest. Missing image (broken) → no-op.
///
/// Byte-identity (the default `object-fit: fill` + `image-rendering: auto`):
/// `src_crop` is the full image rect `(0,0,width,height)` and `smooth` is false,
/// so `cx_u = cy_u = 0`, `cw_u = img.width`, `ch_u = img.height`, and the nearest
/// formula reduces to the original `(rx*img.width/dw).min(w-1)` INTEGER-for-INTEGER.
fn blit_image(
    pixmap: &mut Pixmap,
    dest: &Rect,
    src: &str,
    src_crop: &Rect,
    smooth: bool,
    images: &ImageStore,
) {
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
    // Source crop in source pixels. The default full crop gives cw_u==img.width
    // and ch_u==img.height exactly (emit passes width = img.width as f32).
    let cx_u = src_crop.x.max(0.0) as u32;
    let cy_u = src_crop.y.max(0.0) as u32;
    let cw_u = (src_crop.width.max(0.0) as u32)
        .min(img.width - cx_u)
        .max(1);
    let ch_u = (src_crop.height.max(0.0) as u32)
        .min(img.height - cy_u)
        .max(1);
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
        for px in x_start..x_end {
            let rx = px - dx0;
            let (r, g, b, a) = if smooth {
                sample_bilinear(
                    img,
                    cx_u as f32,
                    cy_u as f32,
                    cw_u as f32,
                    ch_u as f32,
                    rx,
                    dw,
                    ry,
                    dh,
                )
            } else {
                let sx = (cx_u + rx as u32 * cw_u / dw as u32).min(img.width - 1);
                let sy = (cy_u + ry as u32 * ch_u / dh as u32).min(img.height - 1);
                let si = ((sy * img.width + sx) * 4) as usize;
                (
                    img.rgba[si],
                    img.rgba[si + 1],
                    img.rgba[si + 2],
                    img.rgba[si + 3],
                )
            };
            if a == 0 {
                continue;
            }
            let idx = ((py * pw + px) as usize) * 4;
            src_over_pixel(buf, idx, Rgba { r, g, b, a }, a);
        }
    }
}

// --- E20-M1: <canvas> 2D op replay ---

/// Replay the recorded 2D ops into a transparent backing pixmap (in canvas
/// coords, no transform), then scale/composite that pixmap into `rect` on the
/// page. The op state machine mirrors the spec's drawing-state subset for M1:
/// fill/stroke color, line width, a current path, and immediate rect ops.
fn replay_canvas(pixmap: &mut Pixmap, rect: &Rect, backing: (f32, f32), ops: &[CanvasOp]) {
    let bw = backing.0.ceil().max(1.0) as u32;
    let bh = backing.1.ceil().max(1.0) as u32;
    let Some(mut back) = Pixmap::new(bw, bh) else {
        return;
    };

    // Drawing state (spec defaults: opaque black fill+stroke, 1px line).
    let mut fill = CanvasColor {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    let mut stroke = fill;
    let mut line_width = 1.0_f32;
    // The current path builder + current point (for arc's "line to start").
    let mut pb = PathBuilder::new();
    let mut cur: Option<(f32, f32)> = None;

    for op in ops {
        match op {
            CanvasOp::SetFillStyle(c) => fill = *c,
            CanvasOp::SetStrokeStyle(c) => stroke = *c,
            CanvasOp::SetLineWidth(w) => line_width = *w,
            CanvasOp::FillRect(x, y, w, h) => {
                fill_canvas_rect(&mut back, *x, *y, *w, *h, fill);
            }
            CanvasOp::StrokeRect(x, y, w, h) => {
                stroke_canvas_rect(&mut back, *x, *y, *w, *h, stroke, line_width);
            }
            CanvasOp::ClearRect(x, y, w, h) => {
                clear_canvas_rect(&mut back, *x, *y, *w, *h);
            }
            CanvasOp::BeginPath => {
                pb = PathBuilder::new();
                cur = None;
            }
            CanvasOp::MoveTo(x, y) => {
                pb.move_to(*x, *y);
                cur = Some((*x, *y));
            }
            CanvasOp::LineTo(x, y) => {
                if cur.is_none() {
                    pb.move_to(*x, *y); // implicit subpath start
                } else {
                    pb.line_to(*x, *y);
                }
                cur = Some((*x, *y));
            }
            CanvasOp::ClosePath => {
                pb.close();
            }
            CanvasOp::Rect(x, y, w, h) => {
                pb.move_to(*x, *y);
                pb.line_to(*x + *w, *y);
                pb.line_to(*x + *w, *y + *h);
                pb.line_to(*x, *y + *h);
                pb.close();
                cur = Some((*x, *y));
            }
            CanvasOp::Arc(x, y, r, a0, a1, ccw) => {
                cur = arc_center_to_path(&mut pb, *x, *y, *r, *a0, *a1, *ccw, cur);
            }
            CanvasOp::Fill => {
                // fill/stroke do NOT consume the path (clone the builder).
                if let Some(path) = pb.clone().finish() {
                    let mut paint = Paint {
                        anti_alias: true,
                        ..Default::default()
                    };
                    paint.set_color_rgba8(fill.r, fill.g, fill.b, fill.a);
                    back.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
            CanvasOp::Stroke => {
                if let Some(path) = pb.clone().finish() {
                    let mut paint = Paint {
                        anti_alias: true,
                        ..Default::default()
                    };
                    paint.set_color_rgba8(stroke.r, stroke.g, stroke.b, stroke.a);
                    let s = Stroke {
                        width: line_width.max(0.0),
                        ..Default::default()
                    };
                    back.stroke_path(&path, &paint, &s, Transform::identity(), None);
                }
            }
        }
    }

    // Composite the backing into `rect`: scale (bw×bh) → rect anisotropically
    // and draw source-over. The mapping puts the backing exactly onto `rect`,
    // so the clip to `rect` is implicit.
    let sx = rect.width / backing.0;
    let sy = rect.height / backing.1;
    let t = Transform::from_row(sx, 0.0, 0.0, sy, rect.x, rect.y);
    if !t.is_finite() {
        return;
    }
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    pixmap.draw_pixmap(0, 0, back.as_ref(), &paint, t, None);
}

/// Fill an axis-aligned rect on the backing in `color` (source-over).
fn fill_canvas_rect(back: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: CanvasColor) {
    let Some(r) = SkRect::from_xywh(x.min(x + w), y.min(y + h), w.abs(), h.abs()) else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    back.fill_rect(r, &paint, Transform::identity(), None);
}

/// Stroke an axis-aligned rect's outline on the backing.
fn stroke_canvas_rect(
    back: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: CanvasColor,
    line_width: f32,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    let s = Stroke {
        width: line_width.max(0.0),
        ..Default::default()
    };
    back.stroke_path(&path, &paint, &s, Transform::identity(), None);
}

/// Erase an axis-aligned rect to transparent on the backing (BlendMode::Clear).
fn clear_canvas_rect(back: &mut Pixmap, x: f32, y: f32, w: f32, h: f32) {
    let Some(r) = SkRect::from_xywh(x.min(x + w), y.min(y + h), w.abs(), h.abs()) else {
        return;
    };
    let paint = Paint {
        blend_mode: tiny_skia::BlendMode::Clear,
        ..Default::default()
    };
    back.fill_rect(r, &paint, Transform::identity(), None);
}

/// Append a center-parameterized arc to `pb` as ≤90° cubic Bézier segments
/// (the SVG `arc_to_cubics` is endpoint-form and not reusable). Mirrors the
/// canvas spec: if there is a current point, line to the arc's start; else move
/// there. `ccw` selects the sweep direction. Returns the new current point (the
/// arc's end).
#[allow(clippy::too_many_arguments)]
fn arc_center_to_path(
    pb: &mut PathBuilder,
    cx: f32,
    cy: f32,
    r: f32,
    a0: f32,
    a1: f32,
    ccw: bool,
    cur: Option<(f32, f32)>,
) -> Option<(f32, f32)> {
    if r < 0.0 || !r.is_finite() {
        return cur;
    }
    // Normalize the sweep to the canvas spec direction (clockwise = increasing
    // angle in the standard y-down coordinate space).
    let mut delta = a1 - a0;
    if ccw {
        // Counter-clockwise: sweep is negative; wrap into (-2π, 0].
        while delta > 0.0 {
            delta -= std::f32::consts::TAU;
        }
        if delta < -std::f32::consts::TAU {
            delta = -std::f32::consts::TAU;
        }
    } else {
        while delta < 0.0 {
            delta += std::f32::consts::TAU;
        }
        if delta > std::f32::consts::TAU {
            delta = std::f32::consts::TAU;
        }
    }

    let start = (cx + r * a0.cos(), cy + r * a0.sin());
    match cur {
        Some(_) => pb.line_to(start.0, start.1),
        None => pb.move_to(start.0, start.1),
    }
    if delta == 0.0 {
        return Some(start);
    }

    // Split into ≤90° segments; each is one cubic Bézier approximating the arc.
    let segs = (delta.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as i32;
    let seg = delta / segs as f32;
    let mut theta = a0;
    for _ in 0..segs {
        let next = theta + seg;
        // Cubic control-handle factor for a circular arc spanning angle `seg`:
        // k = (4/3)·tan(seg/4). Carries `seg`'s sign so cw/ccw bulge correctly.
        let k = (4.0 / 3.0) * (seg / 4.0).tan();
        let p0 = (cx + r * theta.cos(), cy + r * theta.sin());
        let p3 = (cx + r * next.cos(), cy + r * next.sin());
        let c1 = (p0.0 - k * r * theta.sin(), p0.1 + k * r * theta.cos());
        let c2 = (p3.0 + k * r * next.sin(), p3.1 - k * r * next.cos());
        pb.cubic_to(c1.0, c1.1, c2.0, c2.1, p3.0, p3.1);
        theta = next;
    }
    Some((cx + r * a1.cos(), cy + r * a1.sin()))
}

/// 4-neighbour bilinear sample of a decoded image (E15-M1). `(cx,cy,cw,ch)` is
/// the source crop in source pixels; `(rx,dw)` / `(ry,dh)` are the dest pixel
/// index + dest span on each axis. The dest pixel CENTER maps to a continuous
/// source coordinate inside the crop; neighbours are floor/ceil clamped to the
/// crop and lerped in straight (non-premultiplied) RGBA.
#[allow(clippy::too_many_arguments)]
fn sample_bilinear(
    img: &crate::image_store::DecodedImage,
    cx: f32,
    cy: f32,
    cw: f32,
    ch: f32,
    rx: i32,
    dw: i32,
    ry: i32,
    dh: i32,
) -> (u8, u8, u8, u8) {
    // Dest pixel center → fractional source coord within the crop, minus 0.5 so
    // the sample falls between texel centers.
    let fx = (rx as f32 + 0.5) / dw as f32 * cw + cx - 0.5;
    let fy = (ry as f32 + 0.5) / dh as f32 * ch + cy - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let max_x = (img.width - 1) as i32;
    let max_y = (img.height - 1) as i32;
    let clamp = |v: f32, max: i32| (v as i32).clamp(0, max);
    let x0i = clamp(x0, max_x);
    let x1i = clamp(x0 + 1.0, max_x);
    let y0i = clamp(y0, max_y);
    let y1i = clamp(y0 + 1.0, max_y);
    let texel = |x: i32, y: i32| {
        let i = ((y as u32 * img.width + x as u32) * 4) as usize;
        [
            img.rgba[i],
            img.rgba[i + 1],
            img.rgba[i + 2],
            img.rgba[i + 3],
        ]
    };
    let p00 = texel(x0i, y0i);
    let p10 = texel(x1i, y0i);
    let p01 = texel(x0i, y1i);
    let p11 = texel(x1i, y1i);
    let lerp = |a: u8, b: u8, t: f32| a as f32 + (b as f32 - a as f32) * t;
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = lerp(p00[c], p10[c], tx);
        let bot = lerp(p01[c], p11[c], tx);
        out[c] = (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8;
    }
    (out[0], out[1], out[2], out[3])
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
        ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        )
    }

    #[test]
    fn blend_black_over_white() {
        // 1x1 white pixmap, blit full-coverage black → black.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(
            &mut buf,
            0,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            255,
        );
        assert_eq!(buf, vec![0, 0, 0, 255]);
    }

    #[test]
    fn blend_half_coverage() {
        // half coverage of red over white → ~ (255,128,128) premultiplied.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(
            &mut buf,
            0,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            128,
        );
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
            &Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            "q.png",
            &Rect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            }, // full crop
            false, // nearest
            &images,
        );
        let p0 = pm.pixel(0, 0).unwrap();
        let p1 = pm.pixel(1, 0).unwrap();
        assert_eq!((p0.red(), p0.green(), p0.blue()), (255, 0, 0)); // red half
        assert_eq!((p1.red(), p1.green(), p1.blue()), (0, 0, 255)); // blue half
    }

    #[test]
    fn blit_smooth_vs_nearest_differ_on_upscale() {
        // 2×1 source: left red, right blue. Upscale to 8×1. The middle dest pixels
        // straddle the red↔blue boundary: nearest stays hard (pure red or blue),
        // bilinear blends them (some green-free purple-ish mix → both r and b > 0).
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-bilin-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        img.save(dir.join("g.png")).unwrap();
        let mut images = store_for(&dir);
        images.get("g.png").expect("decoded 2x1");

        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: 2.0,
            height: 1.0,
        };
        let dest = Rect {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 1.0,
        };

        // Nearest: every pixel is pure red or pure blue (no blend).
        let mut near = Pixmap::new(8, 1).unwrap();
        near.fill(Color::WHITE);
        blit_image(&mut near, &dest, "g.png", &full, false, &images);
        for x in 0..8 {
            let p = near.pixel(x, 0).unwrap();
            let pure = (p.red() == 255 && p.blue() == 0) || (p.red() == 0 && p.blue() == 255);
            assert!(pure, "nearest pixel {x} should be pure red/blue: {p:?}");
        }

        // Bilinear: at least one pixel near the boundary blends (both r and b > 0).
        let mut smooth = Pixmap::new(8, 1).unwrap();
        smooth.fill(Color::WHITE);
        blit_image(&mut smooth, &dest, "g.png", &full, true, &images);
        let blended = (0..8).any(|x| {
            let p = smooth.pixel(x, 0).unwrap();
            p.red() > 0 && p.blue() > 0
        });
        assert!(blended, "bilinear should blend red↔blue somewhere");
    }

    #[test]
    fn fill_rect_paints_red() {
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::FillRect {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            },
            color: Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
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
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            color: Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            radius: [40.0; 4],
        }];
        let pm = rasterize(&cmds, 100, 100, &fonts, &empty_store());
        let corner = pm.pixel(1, 1).unwrap();
        assert_eq!(
            (corner.red(), corner.green(), corner.blue()),
            (255, 255, 255)
        );
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

    #[test]
    fn dashed_stroke_line_has_gaps() {
        // A horizontal dashed blue line across a white canvas should leave white
        // gaps between dashes (not a solid line).
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::StrokeLine {
            from: (0.0, 10.0),
            to: (100.0, 10.0),
            width: 4.0,
            color: Rgba {
                r: 0,
                g: 0,
                b: 255,
                a: 255,
            },
            style: BorderStyle::Dashed,
        }];
        let pm = rasterize(&cmds, 100, 20, &fonts, &empty_store());
        // Scan the center row: some pixels blue (dash), some white (gap).
        let mut blue = 0;
        let mut white = 0;
        for x in 0..100 {
            let px = pm.pixel(x, 10).unwrap();
            if px.blue() > 200 && px.red() < 60 {
                blue += 1;
            } else if px.red() > 240 && px.green() > 240 && px.blue() > 240 {
                white += 1;
            }
        }
        assert!(blue > 0, "expected dash pixels");
        assert!(
            white > 0,
            "expected gap (white) pixels along the dashed line"
        );
    }

    #[test]
    fn push_clip_clips_children() {
        // A 50×50 clip around a 100×100 red fill: inside the clip is red, outside
        // is white (clipped).
        let fonts = FontDb::load().unwrap();
        let cmds = vec![
            PaintCmd::PushClip {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 50.0,
                    height: 50.0,
                },
                radius: [0.0; 4],
            },
            PaintCmd::FillRect {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                color: Rgba {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                radius: [0.0; 4],
            },
            PaintCmd::PopClip,
        ];
        let pm = rasterize(&cmds, 100, 100, &fonts, &empty_store());
        let inside = pm.pixel(25, 25).unwrap();
        assert_eq!((inside.red(), inside.green(), inside.blue()), (255, 0, 0));
        let outside = pm.pixel(60, 60).unwrap();
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }
}
