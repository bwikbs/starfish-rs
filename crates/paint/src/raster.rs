//! Rasterization (M5 §4 + E2-M5): a white canvas, fill-rects / gradients /
//! rounded rects / box-shadows via tiny-skia, opacity layers, and manual
//! src-over blitting of fontdue glyph coverage masks.

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop as SkStop, LineCap, LineJoin,
    LinearGradient as SkGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient as SkRadial, Rect as SkRect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use starfish_layout::{FontQuery, FontStyle, Rect};
use starfish_style::{
    BlendMode, BorderStyle, ClipRadius, ClipShape, ConicGradient, FilterFn, FontWeight, LengthPct,
    LinearGradient, RadialGradient, Rgba,
};

use crate::display::{
    align, bg_tile_size, tile_starts, GradKind, GradUnits, MaskBox, MaskGeometryBox, MaskImage,
    MaskMode, PaintCmd,
    SvgFillRule, SvgGeom, SvgGradient, SvgLineCap, SvgLineJoin, SvgPaint,
};
use crate::font::{FontDb, GlyphBitmap};
use crate::image_store::ImageStore;
use crate::svg_path::PathOp;
use starfish_dom::{
    CanvasColor, CanvasGradient, CanvasGradientKind, CanvasImageSrc, CanvasLineCap, CanvasLineJoin,
    CanvasOp, CanvasTextAlign, CanvasTextBaseline,
};

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
            PaintCmd::PushLayer {
                opacity,
                filter,
                blend,
                mask,
            } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((
                        layer,
                        LayerPop::Layer {
                            opacity: *opacity,
                            filter: filter.clone(),
                            blend: *blend,
                            mask: mask.clone(),
                        },
                    ));
                }
            }
            PaintCmd::ApplyBackdropFilter { rect, filter } => {
                let target = stack.last_mut().map(|(p, _)| p).unwrap_or(&mut base);
                apply_backdrop_filter(target, rect, filter);
            }
            PaintCmd::PushTransform { matrix } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::Transform(*matrix)));
                }
            }
            PaintCmd::PushBgGroup => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::BgGroup));
                }
            }
            PaintCmd::PushClip { rect, radius } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::Clip(*rect, *radius)));
                }
            }
            PaintCmd::PushClipPath { shape, border_box } => {
                if let Some(layer) = Pixmap::new(width, height) {
                    stack.push((layer, LayerPop::ClipPath(shape.clone(), *border_box)));
                }
            }
            PaintCmd::PopLayer
            | PaintCmd::PopTransform
            | PaintCmd::PopBgGroup
            | PaintCmd::PopClip => {
                if let Some((mut layer, pop)) = stack.pop() {
                    let dst = stack.last_mut().map(|(p, _)| p).unwrap_or(&mut base);
                    match pop {
                        LayerPop::Layer {
                            opacity,
                            filter,
                            blend,
                            mask,
                        } => {
                            apply_filters(&mut layer, &filter);
                            if let Some(mb) = &mask {
                                apply_mask(&mut layer, mb, images);
                            }
                            composite_layer(dst, &layer, opacity, blend);
                        }
                        LayerPop::Transform(m) => composite_transform_layer(dst, &layer, m),
                        // The bg group is composited source-over (isolation): the
                        // inter-layer blending already happened inside the group.
                        LayerPop::BgGroup => composite_layer(dst, &layer, 1.0, BlendMode::Normal),
                        LayerPop::Clip(rect, radius) => {
                            composite_clip_layer(dst, &layer, &rect, &radius, width, height)
                        }
                        LayerPop::ClipPath(shape, bbox) => {
                            composite_clip_path_layer(dst, &layer, &shape, &bbox, width, height)
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
    /// Apply `filter` to the layer (E21-M1), then composite it with `blend`
    /// (E21-M2 `mix-blend-mode`) scaled by `opacity` (group opacity). Empty filter
    /// + Normal blend → opacity-only source-over (byte-identical).
    Layer {
        opacity: f32,
        filter: Vec<FilterFn>,
        blend: BlendMode,
        /// `mask-image` (E21-M3): multiplies the layer's alpha by the mask source's
        /// coverage (after `filter`) before compositing. `None` = no mask.
        mask: Option<MaskBox>,
    },
    /// Composite through this `[a,b,c,d,e,f]` transform matrix (E5-M3).
    Transform([f32; 6]),
    /// An isolated `background-blend-mode` group (E21-M2): composited source-over.
    BgGroup,
    /// Composite clipped to this padding-box rect + corner radii (E13-M4).
    Clip(Rect, [f32; 4]),
    /// Composite clipped to a `clip-path` shape against its border box (E32-M1).
    ClipPath(ClipShape, Rect),
}

/// Draw a single (non-layer) command into `pixmap`.
fn draw_into(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb, images: &ImageStore) {
    match cmd {
        PaintCmd::FillRect {
            rect,
            color,
            radius,
            blend,
        } => fill_rect_rounded(pixmap, rect, *color, radius, *blend),
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
            blend,
        } => fill_gradient(pixmap, rect, gradient, radius, *blend),
        PaintCmd::RadialRect {
            rect,
            gradient,
            radius,
            blend,
        } => fill_radial(pixmap, rect, gradient, radius, *blend),
        PaintCmd::ConicRect {
            rect,
            gradient,
            radius,
            blend,
        } => fill_conic(pixmap, rect, gradient, radius, *blend),
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
            blend,
        } => blit_image(pixmap, dest, src, src_crop, *smooth, *blend, images),
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
        PaintCmd::Canvas { rect, backing, ops } => {
            replay_canvas(pixmap, rect, *backing, ops, fonts, images)
        }
        PaintCmd::PushLayer { .. }
        | PaintCmd::PopLayer
        | PaintCmd::ApplyBackdropFilter { .. }
        | PaintCmd::PushTransform { .. }
        | PaintCmd::PopTransform
        | PaintCmd::PushBgGroup
        | PaintCmd::PopBgGroup
        | PaintCmd::PushClip { .. }
        | PaintCmd::PushClipPath { .. }
        | PaintCmd::PopClip => {} // handled by caller
    }
}

fn radius_is_zero(r: &[f32; 4]) -> bool {
    r.iter().all(|v| *v <= 0.0)
}

/// Map a CSS [`BlendMode`] to a tiny-skia blend mode (E21-M2). `Normal` →
/// `SourceOver` (the default, byte-identical to no blending); the other 15 modes
/// map 1:1.
fn to_sk_blend(b: BlendMode) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as Sk;
    match b {
        BlendMode::Normal => Sk::SourceOver,
        BlendMode::Multiply => Sk::Multiply,
        BlendMode::Screen => Sk::Screen,
        BlendMode::Overlay => Sk::Overlay,
        BlendMode::Darken => Sk::Darken,
        BlendMode::Lighten => Sk::Lighten,
        BlendMode::ColorDodge => Sk::ColorDodge,
        BlendMode::ColorBurn => Sk::ColorBurn,
        BlendMode::HardLight => Sk::HardLight,
        BlendMode::SoftLight => Sk::SoftLight,
        BlendMode::Difference => Sk::Difference,
        BlendMode::Exclusion => Sk::Exclusion,
        BlendMode::Hue => Sk::Hue,
        BlendMode::Saturation => Sk::Saturation,
        BlendMode::Color => Sk::Color,
        BlendMode::Luminosity => Sk::Luminosity,
    }
}

/// Fill a rect, sharp (the existing fast path, byte-identical) or rounded.
fn fill_rect_rounded(
    pixmap: &mut Pixmap,
    rect: &Rect,
    color: Rgba,
    radius: &[f32; 4],
    blend: BlendMode,
) {
    if radius_is_zero(radius) {
        fill_rect(pixmap, rect, color, blend);
    } else if let Some(path) = rounded_rect_path(rect, radius) {
        let mut paint = Paint {
            anti_alias: true,
            blend_mode: to_sk_blend(blend),
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

fn fill_rect(pixmap: &mut Pixmap, rect: &Rect, color: Rgba, blend: BlendMode) {
    let mut paint = Paint {
        anti_alias: false,
        blend_mode: to_sk_blend(blend),
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    if let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0)) {
        pixmap.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// Fill `rect` (sharp or rounded) with a linear-gradient shader.
fn fill_gradient(
    pixmap: &mut Pixmap,
    rect: &Rect,
    gradient: &LinearGradient,
    radius: &[f32; 4],
    blend: BlendMode,
) {
    let Some(shader) = gradient_shader(gradient, rect) else {
        return;
    };
    let paint = Paint {
        anti_alias: true,
        shader,
        blend_mode: to_sk_blend(blend),
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
fn fill_radial(
    pixmap: &mut Pixmap,
    rect: &Rect,
    gradient: &RadialGradient,
    radius: &[f32; 4],
    blend: BlendMode,
) {
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
        blend_mode: to_sk_blend(blend),
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
fn fill_conic(
    pixmap: &mut Pixmap,
    rect: &Rect,
    gradient: &ConicGradient,
    radius: &[f32; 4],
    blend: BlendMode,
) {
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
            blend_mode: to_sk_blend(blend),
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

// --- filter (E21-M1) ---

/// Apply each `filter` function to `pixmap` in source order, mutating its
/// premultiplied RGBA8 in place. Returns immediately on an empty list, so the
/// opacity-only path is byte-identical to before this milestone.
fn apply_filters(pixmap: &mut Pixmap, filter: &[FilterFn]) {
    if filter.is_empty() {
        return;
    }
    for f in filter {
        match *f {
            FilterFn::Blur(stddev) => {
                let cap = pixmap.width().max(pixmap.height()) as i32;
                // Match the box-shadow radius convention (stddev → box radius).
                let r = ((stddev / 2.0).round() as i32).clamp(0, cap.max(1));
                box_blur_rgba(pixmap, r);
            }
            FilterFn::Brightness(b) => {
                color_transfer(pixmap, |r, g, b2| (r * b, g * b, b2 * b));
            }
            FilterFn::Contrast(k) => {
                let f = |c: f32| (c - 0.5) * k + 0.5;
                color_transfer(pixmap, |r, g, b2| (f(r), f(g), f(b2)));
            }
            FilterFn::Invert(x) => {
                // c + x*(1 - 2c)
                let f = |c: f32| c + x * (1.0 - 2.0 * c);
                color_transfer(pixmap, |r, g, b2| (f(r), f(g), f(b2)));
            }
            FilterFn::Grayscale(x) => {
                color_transfer(pixmap, |r, g, b2| {
                    apply_mat3(&grayscale_matrix(x), r, g, b2)
                });
            }
            FilterFn::Sepia(x) => {
                color_transfer(pixmap, |r, g, b2| apply_mat3(&sepia_matrix(x), r, g, b2));
            }
            FilterFn::Saturate(s) => {
                color_transfer(pixmap, |r, g, b2| apply_mat3(&saturate_matrix(s), r, g, b2));
            }
            FilterFn::HueRotate(theta) => {
                color_transfer(pixmap, |r, g, b2| {
                    apply_mat3(&hue_rotate_matrix(theta), r, g, b2)
                });
            }
            FilterFn::Opacity(o) => opacity_filter(pixmap, o),
            FilterFn::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => drop_shadow_filter(pixmap, dx, dy, blur, color),
        }
    }
}

/// `opacity(o)`: scale all four premultiplied bytes (alpha + premult rgb) by `o`.
fn opacity_filter(pixmap: &mut Pixmap, o: f32) {
    let o = o.clamp(0.0, 1.0);
    for px in pixmap.data_mut().chunks_exact_mut(4) {
        for b in px.iter_mut() {
            *b = (*b as f32 * o).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Apply a `mask-image` (E21-M3) to a popped layer: render the mask source into a
/// full-size scratch pixmap, derive per-pixel coverage (alpha or luminance), and
/// multiply all four premultiplied bytes of `layer` by `cov/255`. Applied AFTER
/// the layer's `filter`, BEFORE compositing.
fn apply_mask(layer: &mut Pixmap, mb: &MaskBox, images: &ImageStore) {
    let Some(mut msrc) = Pixmap::new(layer.width(), layer.height()) else {
        return;
    };
    render_mask_source(&mut msrc, mb, images);
    // Snapshot the mask bytes first (we mutate `layer` below).
    let mdata = msrc.data().to_vec();
    let buf = layer.data_mut();
    for (px, m) in buf.chunks_exact_mut(4).zip(mdata.chunks_exact(4)) {
        let cov = match mb.spec.mode {
            MaskMode::Alpha => m[3] as u32,
            // Luminance over the PREMULTIPLIED bytes (so a transparent source
            // contributes 0). Rec.601-ish integer weights.
            MaskMode::Luminance => {
                (299 * m[0] as u32 + 587 * m[1] as u32 + 114 * m[2] as u32) / 1000
            }
        };
        for b in px.iter_mut() {
            *b = ((*b as u32 * cov) / 255) as u8;
        }
    }
}

/// Render the mask source into the full-size `msrc` (E21-M3). `mask-origin`
/// (E32-M2) selects the box that `mask-size`-percent and `mask-position` resolve
/// against; `mask-clip` selects the box the paint is clipped to (`no-clip` =
/// the border box). Gradients box-fill the clip box (ignoring mask-size/repeat —
/// MVP); `url` sources tile per background-size/position/repeat, clipped to it.
fn render_mask_source(msrc: &mut Pixmap, mb: &MaskBox, images: &ImageStore) {
    let origin_box = match mb.spec.origin {
        MaskGeometryBox::PaddingBox => &mb.padding_box,
        MaskGeometryBox::ContentBox => &mb.content_box,
        _ => &mb.rect,
    };
    let clip_box = match mb.spec.clip {
        MaskGeometryBox::PaddingBox => &mb.padding_box,
        MaskGeometryBox::ContentBox => &mb.content_box,
        MaskGeometryBox::BorderBox | MaskGeometryBox::NoClip => &mb.rect,
    };
    match &mb.spec.image {
        MaskImage::Gradient(g) => fill_gradient(msrc, clip_box, g, &mb.radius, BlendMode::Normal),
        MaskImage::Radial(g) => fill_radial(msrc, clip_box, g, &mb.radius, BlendMode::Normal),
        MaskImage::Url(src) => {
            let Some(img) = images.peek(src) else { return };
            let (iw, ih) = (img.width as f32, img.height as f32);
            if iw <= 0.0 || ih <= 0.0 {
                return;
            }
            // Size-percent and position resolve against the origin box; tiling and
            // painting are clipped to the clip box.
            let (tw, th) = bg_tile_size(mb.spec.size, iw, ih, origin_box.width, origin_box.height);
            if tw <= 0.0 || th <= 0.0 {
                return;
            }
            let ox = origin_box.x + align(mb.spec.position.0, origin_box.width - tw);
            let oy = origin_box.y + align(mb.spec.position.1, origin_box.height - th);
            let (rep_x, rep_y) = match mb.spec.repeat {
                starfish_style::BgRepeat::Repeat => (true, true),
                starfish_style::BgRepeat::NoRepeat => (false, false),
                starfish_style::BgRepeat::RepeatX => (true, false),
                starfish_style::BgRepeat::RepeatY => (false, true),
            };
            let src_crop = Rect {
                x: 0.0,
                y: 0.0,
                width: iw,
                height: ih,
            };
            for ty in tile_starts(oy, th, clip_box.y, clip_box.height, rep_y) {
                for tx in tile_starts(ox, tw, clip_box.x, clip_box.width, rep_x) {
                    blit_image(
                        msrc,
                        &Rect {
                            x: tx,
                            y: ty,
                            width: tw,
                            height: th,
                        },
                        src,
                        &src_crop,
                        false,
                        BlendMode::Normal,
                        images,
                    );
                }
            }
        }
    }
}

/// Apply a `backdrop-filter` (E21-M3) to the destination's `rect` region in place:
/// snapshot the box region, filter the snapshot, and draw it back clipped to
/// `rect`. Empty `filter` is a no-op (the emit site already gates on this).
fn apply_backdrop_filter(dst: &mut Pixmap, rect: &Rect, filter: &[FilterFn]) {
    if filter.is_empty() {
        return;
    }
    let (w, h) = (dst.width(), dst.height());
    let ix = rect.x.floor().max(0.0) as u32;
    let iy = rect.y.floor().max(0.0) as u32;
    let iw = ((rect.x + rect.width).ceil().min(w as f32) as i64 - ix as i64).max(0) as u32;
    let ih = ((rect.y + rect.height).ceil().min(h as f32) as i64 - iy as i64).max(0) as u32;
    if iw == 0 || ih == 0 {
        return;
    }
    let Some(mut snapshot) = Pixmap::new(w, h) else {
        return;
    };
    let Some(mask) = rect_mask(w, h, ix, iy, iw, ih) else {
        return;
    };
    // Copy only the box region of `dst` into the (full-size) snapshot.
    snapshot.draw_pixmap(
        0,
        0,
        dst.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        Some(&mask),
    );
    apply_filters(&mut snapshot, filter);
    // Draw the filtered snapshot back, clipped to the box region.
    dst.draw_pixmap(
        0,
        0,
        snapshot.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        Some(&mask),
    );
}

/// A sharp box clip mask covering `(x,y,w,h)` over a `(width,height)` canvas.
fn rect_mask(width: u32, height: u32, x: u32, y: u32, w: u32, h: u32) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let r = SkRect::from_xywh(x as f32, y as f32, w as f32, h as f32)?;
    mask.fill_path(
        &PathBuilder::from_rect(r),
        FillRule::Winding,
        true,
        Transform::identity(),
    );
    Some(mask)
}

/// Apply a per-channel straight-color transform to every pixel. Reads premult
/// RGBA, un-premultiplies to straight 0..1, applies `f`, clamps, re-premultiplies.
/// Fully transparent pixels are skipped (their straight color is undefined).
fn color_transfer(pixmap: &mut Pixmap, f: impl Fn(f32, f32, f32) -> (f32, f32, f32)) {
    for px in pixmap.data_mut().chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            continue;
        }
        let af = a as f32;
        let (r, g, b) = (px[0] as f32 / af, px[1] as f32 / af, px[2] as f32 / af);
        let (r, g, b) = f(r, g, b);
        px[0] = (r.clamp(0.0, 1.0) * af).round().clamp(0.0, 255.0) as u8;
        px[1] = (g.clamp(0.0, 1.0) * af).round().clamp(0.0, 255.0) as u8;
        px[2] = (b.clamp(0.0, 1.0) * af).round().clamp(0.0, 255.0) as u8;
    }
}

/// Apply a row-major 3×3 color matrix to a straight RGB triple.
fn apply_mat3(m: &[f32; 9], r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        m[0] * r + m[1] * g + m[2] * b,
        m[3] * r + m[4] * g + m[5] * b,
        m[6] * r + m[7] * g + m[8] * b,
    )
}

// Rec. 709 luma weights (per the CSS filter spec).
const LR: f32 = 0.2126;
const LG: f32 = 0.7152;
const LB: f32 = 0.0722;

/// `grayscale(x)`: lerp identity ↔ luminance-broadcast matrix.
fn grayscale_matrix(x: f32) -> [f32; 9] {
    let x = x.clamp(0.0, 1.0);
    let i = 1.0 - x;
    [
        LR * x + i,
        LG * x,
        LB * x,
        LR * x,
        LG * x + i,
        LB * x,
        LR * x,
        LG * x,
        LB * x + i,
    ]
}

/// `sepia(x)`: lerp identity ↔ the spec sepia matrix.
fn sepia_matrix(x: f32) -> [f32; 9] {
    let x = x.clamp(0.0, 1.0);
    let i = 1.0 - x;
    [
        0.393 * x + i,
        0.769 * x,
        0.189 * x,
        0.349 * x,
        0.686 * x + i,
        0.168 * x,
        0.272 * x,
        0.534 * x,
        0.131 * x + i,
    ]
}

/// `saturate(s)`: the spec saturation matrix (s=0 → grayscale, s=1 → identity).
fn saturate_matrix(s: f32) -> [f32; 9] {
    [
        LR + s * (1.0 - LR),
        LG - s * LG,
        LB - s * LB,
        LR - s * LR,
        LG + s * (1.0 - LG),
        LB - s * LB,
        LR - s * LR,
        LG - s * LG,
        LB + s * (1.0 - LB),
    ]
}

/// `hue-rotate(θ)`: the spec luma-weighted hue rotation matrix (θ in radians).
fn hue_rotate_matrix(theta: f32) -> [f32; 9] {
    let (s, c) = theta.sin_cos();
    [
        LR + c * (1.0 - LR) + s * (-LR),
        LG + c * (-LG) + s * (-LG),
        LB + c * (-LB) + s * (1.0 - LB),
        LR + c * (-LR) + s * 0.143,
        LG + c * (1.0 - LG) + s * 0.140,
        LB + c * (-LB) + s * (-0.283),
        LR + c * (-LR) + s * (-(1.0 - LR)),
        LG + c * (-LG) + s * LG,
        LB + c * (1.0 - LB) + s * LB,
    ]
}

/// `drop-shadow(dx dy blur color)`: build a recolored, blurred copy of the
/// layer's alpha, composite it at `(dx,dy)` UNDER the original layer, and replace
/// the layer with the result. (E21-M1)
fn drop_shadow_filter(pixmap: &mut Pixmap, dx: f32, dy: f32, blur: f32, color: Rgba) {
    let (w, h) = (pixmap.width(), pixmap.height());
    // (1) shadow = recolored copy of the current layer's alpha (premultiplied).
    let Some(mut shadow) = Pixmap::new(w, h) else {
        return;
    };
    {
        let src = pixmap.data();
        let dst = shadow.data_mut();
        let ca = color.a as u32;
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            let a = s[3] as u32;
            // shadow straight color = color.rgb; shadow alpha = a * color.a / 255.
            let sa = a * ca / 255;
            d[0] = (color.r as u32 * sa / 255) as u8;
            d[1] = (color.g as u32 * sa / 255) as u8;
            d[2] = (color.b as u32 * sa / 255) as u8;
            d[3] = sa as u8;
        }
    }
    // (2) blur the shadow.
    if blur > 0.0 {
        let cap = w.max(h) as i32;
        let r = ((blur / 2.0).round() as i32).clamp(0, cap.max(1));
        box_blur_rgba(&mut shadow, r);
    }
    // (3) composite the shadow into a fresh out pixmap at offset (dx, dy).
    let Some(mut out) = Pixmap::new(w, h) else {
        return;
    };
    let paint = PixmapPaint::default();
    out.draw_pixmap(
        0,
        0,
        shadow.as_ref(),
        &paint,
        Transform::from_translate(dx, dy),
        None,
    );
    // (4) draw the original layer over the shadow (source-over).
    out.draw_pixmap(0, 0, pixmap.as_ref(), &paint, Transform::identity(), None);
    // (5) replace the layer.
    *pixmap = out;
}

/// 3-pass separable box blur over a premultiplied RGBA8 pixmap (sibling to
/// `box_blur_mask`, which blurs a single u8 channel). Blurring premultiplied
/// channels directly avoids edge halos. (E21-M1)
fn box_blur_rgba(pixmap: &mut Pixmap, r: i32) {
    if r <= 0 {
        return;
    }
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    if w == 0 || h == 0 {
        return;
    }
    let data = pixmap.data_mut();
    let mut tmp = vec![0u8; w * h * 4];
    for _ in 0..3 {
        box_blur_rgba_pass_h(data, &mut tmp, w, h, r as usize);
        box_blur_rgba_pass_v(&tmp, data, w, h, r as usize);
    }
}

/// Horizontal box-blur pass over interleaved RGBA8 (stride 4), per channel.
fn box_blur_rgba_pass_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u64;
    for y in 0..h {
        let row = y * w * 4;
        for ch in 0..4 {
            let mut sum: u64 = 0;
            for x in 0..=r.min(w - 1) {
                sum += src[row + x * 4 + ch] as u64;
            }
            sum += src[row + ch] as u64 * r as u64;
            for x in 0..w {
                dst[row + x * 4 + ch] = (sum / window) as u8;
                let add_idx = (x + r + 1).min(w - 1);
                let sub_idx = x.saturating_sub(r);
                sum += src[row + add_idx * 4 + ch] as u64;
                sum -= src[row + sub_idx * 4 + ch] as u64;
            }
        }
    }
}

/// Vertical box-blur pass over interleaved RGBA8 (stride 4), per channel.
fn box_blur_rgba_pass_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u64;
    let stride = w * 4;
    for x in 0..w {
        for ch in 0..4 {
            let col = x * 4 + ch;
            let mut sum: u64 = 0;
            for y in 0..=r.min(h - 1) {
                sum += src[y * stride + col] as u64;
            }
            sum += src[col] as u64 * r as u64;
            for y in 0..h {
                dst[y * stride + col] = (sum / window) as u8;
                let add_idx = (y + r + 1).min(h - 1);
                let sub_idx = y.saturating_sub(r);
                sum += src[add_idx * stride + col] as u64;
                sum -= src[sub_idx * stride + col] as u64;
            }
        }
    }
}

/// Composite a transparent layer pixmap onto `dst`, scaling the layer's
/// contribution by `opacity` (correct group opacity), with `blend` (E21-M2
/// `mix-blend-mode`; `Normal` → source-over, byte-identical). (E2-M5 §4.3)
fn composite_layer(dst: &mut Pixmap, layer: &Pixmap, opacity: f32, blend: BlendMode) {
    let paint = PixmapPaint {
        opacity: opacity.clamp(0.0, 1.0),
        blend_mode: to_sk_blend(blend),
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

/// Composite `layer` clipped to a `clip-path` shape (E32-M1).
fn composite_clip_path_layer(
    dst: &mut Pixmap,
    layer: &Pixmap,
    shape: &ClipShape,
    rect: &Rect,
    w: u32,
    h: u32,
) {
    let paint = PixmapPaint::default();
    let path = match clip_shape_path(shape, rect) {
        Some(p) => p,
        None => {
            dst.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
            return;
        }
    };
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

fn lp_resolve(lp: LengthPct, dim: f32) -> f32 {
    match lp {
        LengthPct::Px(v) => v,
        LengthPct::Percent(p) => p / 100.0 * dim,
    }
}

/// Build a `tiny-skia` path for a `clip-path` shape against the border box.
fn clip_shape_path(shape: &ClipShape, rect: &Rect) -> Option<tiny_skia::Path> {
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    match shape {
        ClipShape::Inset {
            top,
            right,
            bottom,
            left,
        } => {
            let t = lp_resolve(*top, h);
            let r = lp_resolve(*right, w);
            let b = lp_resolve(*bottom, h);
            let l = lp_resolve(*left, w);
            let ir = SkRect::from_xywh(x + l, y + t, (w - l - r).max(0.0), (h - t - b).max(0.0))?;
            Some(PathBuilder::from_rect(ir))
        }
        ClipShape::Circle { r, cx, cy } => {
            let (clx, cly) = (lp_resolve(*cx, w), lp_resolve(*cy, h));
            let rad = resolve_circle_radius(*r, w, h, clx, cly);
            let mut pb = PathBuilder::new();
            pb.push_circle(x + clx, y + cly, rad);
            pb.finish()
        }
        ClipShape::Ellipse { rx, ry, cx, cy } => {
            let (clx, cly) = (lp_resolve(*cx, w), lp_resolve(*cy, h));
            let rrx = resolve_axis_radius(*rx, w, clx);
            let rry = resolve_axis_radius(*ry, h, cly);
            let oval = SkRect::from_xywh(x + clx - rrx, y + cly - rry, 2.0 * rrx, 2.0 * rry)?;
            let mut pb = PathBuilder::new();
            pb.push_oval(oval);
            pb.finish()
        }
        ClipShape::Polygon(pts) => {
            let mut pb = PathBuilder::new();
            for (i, (px, py)) in pts.iter().enumerate() {
                let (ax, ay) = (x + lp_resolve(*px, w), y + lp_resolve(*py, h));
                if i == 0 {
                    pb.move_to(ax, ay);
                } else {
                    pb.line_to(ax, ay);
                }
            }
            pb.close();
            pb.finish()
        }
    }
}

fn resolve_circle_radius(r: ClipRadius, w: f32, h: f32, cx: f32, cy: f32) -> f32 {
    match r {
        ClipRadius::Length(LengthPct::Px(v)) => v,
        // Percentage radius is relative to sqrt(w²+h²)/√2 (CSS Shapes).
        ClipRadius::Length(LengthPct::Percent(p)) => {
            p / 100.0 * (w * w + h * h).sqrt() / std::f32::consts::SQRT_2
        }
        ClipRadius::ClosestSide => cx.min(w - cx).min(cy).min(h - cy).max(0.0),
        ClipRadius::FarthestSide => cx.max(w - cx).max(cy).max(h - cy),
    }
}

fn resolve_axis_radius(r: ClipRadius, dim: f32, c: f32) -> f32 {
    match r {
        ClipRadius::Length(lp) => lp_resolve(lp, dim),
        ClipRadius::ClosestSide => c.min(dim - c).max(0.0),
        ClipRadius::FarthestSide => c.max(dim - c),
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
#[allow(clippy::too_many_arguments)]
fn blit_image(
    pixmap: &mut Pixmap,
    dest: &Rect,
    src: &str,
    src_crop: &Rect,
    smooth: bool,
    blend: BlendMode,
    images: &ImageStore,
) {
    // E21-M2: a non-Normal blend (only inside a `background-blend-mode` group)
    // can't go through the manual src-over loop, so paint the blit into a fresh
    // transparent layer, then composite it with the blend mode. Normal blend keeps
    // the existing manual fast path (byte-identical).
    if blend != BlendMode::Normal {
        if let Some(mut tmp) = Pixmap::new(pixmap.width(), pixmap.height()) {
            blit_image(
                &mut tmp,
                dest,
                src,
                src_crop,
                smooth,
                BlendMode::Normal,
                images,
            );
            composite_layer(pixmap, &tmp, 1.0, blend);
        }
        return;
    }
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

// --- E20-M1/M2: <canvas> 2D op replay ---

/// The source of a canvas fill/stroke: a solid color or a gradient. (E20-M2)
#[derive(Clone)]
enum CanvasPaintSrc {
    Color(CanvasColor),
    Gradient(CanvasGradient),
}

/// The canvas drawing state subset replayed by `replay_canvas` (E20-M2). Cloned
/// onto a stack by `save`/`restore`. Spec defaults: opaque black fill+stroke,
/// 1px line, alpha 1, butt cap, miter join, no dash, identity transform, no clip.
/// The current PATH is intentionally NOT part of the state (save/restore don't
/// affect the current path) — it stays a loop-local in `replay_canvas`.
#[derive(Clone)]
struct CanvasState {
    fill: CanvasPaintSrc,
    stroke: CanvasPaintSrc,
    line_width: f32,
    global_alpha: f32,
    cap: LineCap,
    join: LineJoin,
    dash: Vec<f32>,
    /// The canvas CTM (current transform matrix) in backing coordinates.
    transform: Transform,
    /// The current clip mask (intersection of all `clip()` calls in scope), or
    /// `None` for the default unclipped state.
    clip: Option<Mask>,
    // --- E20-M3 text state (defaults are inert for M1/M2 streams) ---
    font_size: f32,
    font_family: Vec<String>,
    font_weight: u16,
    font_italic: bool,
    text_align: CanvasTextAlign,
    text_baseline: CanvasTextBaseline,
}

impl CanvasState {
    fn default_state() -> Self {
        let black = CanvasColor {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        CanvasState {
            fill: CanvasPaintSrc::Color(black),
            stroke: CanvasPaintSrc::Color(black),
            line_width: 1.0,
            global_alpha: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            dash: Vec::new(),
            transform: Transform::identity(),
            clip: None,
            font_size: 10.0,
            font_family: vec!["sans-serif".to_string()],
            font_weight: 400,
            font_italic: false,
            text_align: CanvasTextAlign::Start,
            text_baseline: CanvasTextBaseline::Alphabetic,
        }
    }
}

/// Replay the recorded 2D ops into a transparent backing pixmap (in canvas
/// coords), then scale/composite that pixmap into `rect` on the page. The op
/// state machine mirrors the spec's drawing-state subset: fill/stroke source,
/// line width/cap/join/dash, global alpha, transform, clip, a current path, and
/// immediate rect/path ops. M1 op streams (no M2 ops) replay byte-identically:
/// the default state is exactly the loose M1 locals (identity transform, full
/// alpha, no clip, solid color, butt/miter/no-dash).
fn replay_canvas(
    pixmap: &mut Pixmap,
    rect: &Rect,
    backing: (f32, f32),
    ops: &[CanvasOp],
    fonts: &FontDb,
    images: &ImageStore,
) {
    let bw = backing.0.ceil().max(1.0) as u32;
    let bh = backing.1.ceil().max(1.0) as u32;
    let Some(mut back) = Pixmap::new(bw, bh) else {
        return;
    };
    replay_canvas_into_backing(&mut back, ops, fonts, images);

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

/// Run the canvas op state machine into an already-allocated backing pixmap (in
/// backing coords), WITHOUT the final composite-to-rect. Factored out of
/// `replay_canvas` so `drawImage` of a `<canvas>` source (E20-M3) can replay a
/// snapshot's ops into a temp backing and read its raw pixels.
fn replay_canvas_into_backing(
    back: &mut Pixmap,
    ops: &[CanvasOp],
    fonts: &FontDb,
    images: &ImageStore,
) {
    let bw = back.width();
    let bh = back.height();
    let mut st = CanvasState::default_state();
    let mut stack: Vec<CanvasState> = Vec::new();
    // The current path builder + current point (for arc's "line to start").
    let mut pb = PathBuilder::new();
    let mut cur: Option<(f32, f32)> = None;

    for op in ops {
        match op {
            CanvasOp::SetFillStyle(c) => st.fill = CanvasPaintSrc::Color(*c),
            CanvasOp::SetStrokeStyle(c) => st.stroke = CanvasPaintSrc::Color(*c),
            CanvasOp::SetFillStyleGradient(g) => st.fill = CanvasPaintSrc::Gradient(g.clone()),
            CanvasOp::SetStrokeStyleGradient(g) => st.stroke = CanvasPaintSrc::Gradient(g.clone()),
            CanvasOp::SetLineWidth(w) => st.line_width = *w,
            CanvasOp::SetGlobalAlpha(a) => st.global_alpha = a.clamp(0.0, 1.0),
            CanvasOp::SetLineCap(c) => st.cap = canvas_cap(*c),
            CanvasOp::SetLineJoin(j) => st.join = canvas_join(*j),
            CanvasOp::SetLineDash(d) => st.dash = d.clone(),
            CanvasOp::Save => stack.push(st.clone()),
            CanvasOp::Restore => {
                if let Some(s) = stack.pop() {
                    st = s;
                }
            }
            CanvasOp::Transform(a, b, c, d, e, f) => {
                st.transform = st
                    .transform
                    .pre_concat(Transform::from_row(*a, *b, *c, *d, *e, *f));
            }
            CanvasOp::SetTransform(a, b, c, d, e, f) => {
                st.transform = Transform::from_row(*a, *b, *c, *d, *e, *f);
            }
            CanvasOp::FillRect(x, y, w, h) => fill_canvas_rect(back, *x, *y, *w, *h, &st),
            CanvasOp::StrokeRect(x, y, w, h) => stroke_canvas_rect(back, *x, *y, *w, *h, &st),
            CanvasOp::ClearRect(x, y, w, h) => clear_canvas_rect(back, *x, *y, *w, *h, &st),
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
            CanvasOp::QuadTo(cx, cy, x, y) => {
                if cur.is_none() {
                    pb.move_to(*cx, *cy); // no current point → start at the control
                }
                pb.quad_to(*cx, *cy, *x, *y);
                cur = Some((*x, *y));
            }
            CanvasOp::BezierTo(c1x, c1y, c2x, c2y, x, y) => {
                if cur.is_none() {
                    pb.move_to(*c1x, *c1y);
                }
                pb.cubic_to(*c1x, *c1y, *c2x, *c2y, *x, *y);
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
                    if let Some(paint) = canvas_paint(&st.fill, st.global_alpha, bw, bh) {
                        back.fill_path(
                            &path,
                            &paint,
                            FillRule::Winding,
                            st.transform,
                            st.clip.as_ref(),
                        );
                    }
                }
            }
            CanvasOp::Stroke => {
                if let Some(path) = pb.clone().finish() {
                    if let Some(paint) = canvas_paint(&st.stroke, st.global_alpha, bw, bh) {
                        let s = canvas_stroke(&st);
                        back.stroke_path(&path, &paint, &s, st.transform, st.clip.as_ref());
                    }
                }
            }
            CanvasOp::Clip => {
                // Intersect the clip region with the current path (winding rule),
                // transformed by the CTM. save/restore scope it via the cloned state.
                if let Some(path) = pb.clone().finish() {
                    let mask = match st.clip.take() {
                        Some(mut m) => {
                            m.intersect_path(&path, FillRule::Winding, true, st.transform);
                            m
                        }
                        None => {
                            let Some(mut m) = Mask::new(bw, bh) else {
                                continue;
                            };
                            m.fill_path(&path, FillRule::Winding, true, st.transform);
                            m
                        }
                    };
                    st.clip = Some(mask);
                }
            }
            // --- E20-M3 ---
            CanvasOp::SetFont {
                size_px,
                family,
                weight,
                italic,
            } => {
                st.font_size = *size_px;
                st.font_family = family.clone();
                st.font_weight = *weight;
                st.font_italic = *italic;
            }
            CanvasOp::SetTextAlign(a) => st.text_align = *a,
            CanvasOp::SetTextBaseline(b) => st.text_baseline = *b,
            CanvasOp::FillText {
                text,
                x,
                y,
                max_width,
            } => {
                draw_canvas_text(back, &st, text, *x, *y, *max_width, &st.fill, fonts, false);
            }
            CanvasOp::StrokeText {
                text,
                x,
                y,
                max_width,
            } => {
                draw_canvas_text(back, &st, text, *x, *y, *max_width, &st.stroke, fonts, true);
            }
            CanvasOp::DrawImage {
                source,
                src_rect,
                dst,
            } => {
                draw_canvas_image(back, &st, source, *src_rect, *dst, images, fonts);
            }
        }
    }
}

fn canvas_cap(c: CanvasLineCap) -> LineCap {
    match c {
        CanvasLineCap::Butt => LineCap::Butt,
        CanvasLineCap::Round => LineCap::Round,
        CanvasLineCap::Square => LineCap::Square,
    }
}

fn canvas_join(j: CanvasLineJoin) -> LineJoin {
    match j {
        CanvasLineJoin::Miter => LineJoin::Miter,
        CanvasLineJoin::Round => LineJoin::Round,
        CanvasLineJoin::Bevel => LineJoin::Bevel,
    }
}

/// Build the `Stroke` for the current state (width/cap/join/dash).
fn canvas_stroke(st: &CanvasState) -> Stroke {
    Stroke {
        width: st.line_width.max(0.0),
        line_cap: st.cap,
        line_join: st.join,
        dash: if st.dash.is_empty() {
            None
        } else {
            StrokeDash::new(st.dash.clone(), 0.0)
        },
        ..Default::default()
    }
}

/// Build a `Paint` for a canvas fill/stroke source, alpha-multiplied by
/// `global_alpha`. A solid color sets `set_color_rgba8` with the scaled alpha; a
/// gradient builds the matching shader. `None` (skip the draw) for a gradient
/// with no stops. M1 byte-identity: alpha 1 + solid color → `set_color_rgba8`
/// with the unmodified alpha, exactly like M1.
fn canvas_paint(src: &CanvasPaintSrc, global_alpha: f32, w: u32, h: u32) -> Option<Paint<'static>> {
    match src {
        CanvasPaintSrc::Color(c) => {
            let a = mul_alpha(c.a, global_alpha);
            let mut paint = Paint {
                anti_alias: true,
                ..Default::default()
            };
            paint.set_color_rgba8(c.r, c.g, c.b, a);
            Some(paint)
        }
        CanvasPaintSrc::Gradient(g) => {
            let shader = canvas_gradient_shader(g, global_alpha, w, h)?;
            Some(Paint {
                anti_alias: true,
                shader,
                ..Default::default()
            })
        }
    }
}

/// Multiply an 8-bit alpha by a `[0,1]` factor, rounding.
fn mul_alpha(a: u8, factor: f32) -> u8 {
    (a as f32 * factor.clamp(0.0, 1.0))
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Build a tiny-skia gradient shader from a recorded `CanvasGradient`, with each
/// stop's alpha pre-multiplied by `global_alpha`. Stops are clamped to `[0,1]`
/// and sorted non-decreasing (CSS rule). Empty stops → `None` (skip the draw).
/// Radial MVP: `SkRadial::new(start=(x0,y0), end=(x1,y1), radius=r1)` — `r0` is
/// ignored. The shader transform is identity; the caller's CTM composes on top.
fn canvas_gradient_shader(
    g: &CanvasGradient,
    global_alpha: f32,
    _w: u32,
    _h: u32,
) -> Option<Shader<'static>> {
    if g.stops.is_empty() {
        return None;
    }
    // Clamp positions to [0,1] and enforce non-decreasing (CSS clamp rule).
    let mut last = 0.0_f32;
    let stops: Vec<SkStop> = g
        .stops
        .iter()
        .map(|(off, c)| {
            let p = off.clamp(0.0, 1.0).max(last);
            last = p;
            let a = mul_alpha(c.a, global_alpha);
            SkStop::new(p, Color::from_rgba8(c.r, c.g, c.b, a))
        })
        .collect();
    match g.kind {
        CanvasGradientKind::Linear { x0, y0, x1, y1 } => SkGradient::new(
            Point::from_xy(x0, y0),
            Point::from_xy(x1, y1),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ),
        CanvasGradientKind::Radial {
            x0,
            y0,
            r0: _,
            x1,
            y1,
            r1,
        } => SkRadial::new(
            Point::from_xy(x0, y0),
            Point::from_xy(x1, y1),
            r1,
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ),
    }
}

/// Fill an axis-aligned rect on the backing in the current fill source, through
/// the CTM + clip (source-over).
fn fill_canvas_rect(back: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, st: &CanvasState) {
    let Some(r) = SkRect::from_xywh(x.min(x + w), y.min(y + h), w.abs(), h.abs()) else {
        return;
    };
    let Some(paint) = canvas_paint(&st.fill, st.global_alpha, back.width(), back.height()) else {
        return;
    };
    back.fill_rect(r, &paint, st.transform, st.clip.as_ref());
}

/// Stroke an axis-aligned rect's outline on the backing through the CTM + clip.
fn stroke_canvas_rect(back: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, st: &CanvasState) {
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    let Some(path) = pb.finish() else { return };
    let Some(paint) = canvas_paint(&st.stroke, st.global_alpha, back.width(), back.height()) else {
        return;
    };
    let s = canvas_stroke(st);
    back.stroke_path(&path, &paint, &s, st.transform, st.clip.as_ref());
}

/// Erase an axis-aligned rect to transparent on the backing (BlendMode::Clear).
/// `clear` ignores the fill source and global alpha, but still honours the CTM +
/// clip.
fn clear_canvas_rect(back: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, st: &CanvasState) {
    let Some(r) = SkRect::from_xywh(x.min(x + w), y.min(y + h), w.abs(), h.abs()) else {
        return;
    };
    let paint = Paint {
        blend_mode: tiny_skia::BlendMode::Clear,
        ..Default::default()
    };
    back.fill_rect(r, &paint, st.transform, st.clip.as_ref());
}

/// E20-M3: draw a text run on the backing as a filled (or stroked) glyph-outline
/// path, honouring the CTM, clip, global alpha, and fill/stroke source uniformly
/// (same code path as the rect/path ops). Builds a `FontQuery` from the canvas
/// font state, shapes the text, walks the pen by `x_advance` collecting each
/// glyph's outline (via `FontDb::glyph_outline`) into one path, then applies the
/// textAlign x-shift, the textBaseline y-shift, and any `maxWidth` squeeze.
#[allow(clippy::too_many_arguments)]
fn draw_canvas_text(
    back: &mut Pixmap,
    st: &CanvasState,
    text: &str,
    x: f32,
    y: f32,
    max_width: Option<f32>,
    src: &CanvasPaintSrc,
    fonts: &FontDb,
    stroke: bool,
) {
    if text.is_empty() {
        return;
    }
    let q = FontQuery {
        family: &st.font_family,
        style: if st.font_italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        weight: FontWeight(st.font_weight),
        size: st.font_size,
        letter_spacing: 0.0,
        word_spacing: 0.0,
    };
    let glyphs = fonts.shape(text, &q);
    let advance: f32 = glyphs.iter().map(|g| g.x_advance).sum();
    if advance <= 0.0 {
        return;
    }
    // textAlign x-shift (start/left = no shift; right/end = -advance; center = -advance/2).
    let align_x = match st.text_align {
        CanvasTextAlign::Start | CanvasTextAlign::Left => x,
        CanvasTextAlign::End | CanvasTextAlign::Right => x - advance,
        CanvasTextAlign::Center => x - advance / 2.0,
    };
    // textBaseline y-shift via line metrics (ascent/descent positive).
    let lm = fonts.line_metrics(&q);
    let baseline_y = match st.text_baseline {
        CanvasTextBaseline::Alphabetic => y,
        CanvasTextBaseline::Top => y + lm.ascent,
        CanvasTextBaseline::Bottom => y - lm.descent,
        CanvasTextBaseline::Middle => y + (lm.ascent - lm.descent) / 2.0,
    };

    // Build one path of all glyph outlines at their pen positions (origin at 0).
    let mut combined = PathBuilder::new();
    let mut pen = 0.0_f32;
    for g in glyphs.iter() {
        if let Some(path) = fonts.glyph_outline(g.face, g.glyph_id, &q) {
            // Translate the glyph outline (baseline at 0) to its pen x + y_offset.
            let t = Transform::from_translate(pen + g.x_offset, -g.y_offset);
            if let Some(path) = path.transform(t) {
                combined.push_path(&path);
            }
        }
        pen += g.x_advance;
    }
    let Some(path) = combined.finish() else {
        return;
    };

    // Position transform: the combined path is run-local (x in [0, advance],
    // origin at the run start). Scale first (squeeze), then translate the run
    // start to align_x — so x=0 maps to align_x and x=advance maps to
    // align_x + min(advance, mw). Scaling about origin == scaling about the run
    // start, which is exactly the maxWidth behaviour.
    let mut t = Transform::from_translate(align_x, baseline_y);
    if let Some(mw) = max_width {
        if advance > mw && mw > 0.0 {
            let s = mw / advance;
            t = t.pre_concat(Transform::from_scale(s, 1.0));
        }
    }
    // Compose with the CTM (CTM applies on the outside).
    let full = st.transform.pre_concat(t);

    if stroke {
        let Some(paint) = canvas_paint(src, st.global_alpha, back.width(), back.height()) else {
            return;
        };
        let s = canvas_stroke(st);
        back.stroke_path(&path, &paint, &s, full, st.clip.as_ref());
    } else {
        let Some(paint) = canvas_paint(src, st.global_alpha, back.width(), back.height()) else {
            return;
        };
        back.fill_path(&path, &paint, FillRule::Winding, full, st.clip.as_ref());
    }
}

/// E20-M3: draw an image (decoded `<img>` or a `<canvas>` snapshot) into the
/// backing, honouring the CTM, global alpha, and clip. The image is first
/// materialized into a source `Pixmap` (the decoded RGBA, or a fresh backing
/// replayed from the snapshot's ops), then composited through `st.transform`:
/// the source crop `(sx,sy,sw,sh)` maps onto the dest rect `(dx,dy,dw,dh)` (dw/dh
/// default to the intrinsic size). Routed through a temp layer so alpha + CTM +
/// clip apply uniformly.
fn draw_canvas_image(
    back: &mut Pixmap,
    st: &CanvasState,
    source: &CanvasImageSrc,
    src_rect: Option<(f32, f32, f32, f32)>,
    dst: (f32, f32, Option<f32>, Option<f32>),
    images: &ImageStore,
    fonts: &FontDb,
) {
    // Materialize the source into a Pixmap of (iw, ih) intrinsic pixels.
    let src_pix = match source {
        CanvasImageSrc::Url(url) => {
            let Some(img) = images.peek(url) else { return };
            if img.width == 0 || img.height == 0 {
                return;
            }
            let Some(mut p) = Pixmap::new(img.width, img.height) else {
                return;
            };
            // DecodedImage rgba is straight (non-premultiplied) RGBA8; tiny-skia
            // pixmap data is premultiplied RGBA8. Premultiply on copy.
            let data = p.data_mut();
            for (i, px) in img.rgba.chunks_exact(4).enumerate() {
                let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
                let pr = (r as u16 * a as u16 / 255) as u8;
                let pg = (g as u16 * a as u16 / 255) as u8;
                let pb = (b as u16 * a as u16 / 255) as u8;
                let o = i * 4;
                data[o] = pr;
                data[o + 1] = pg;
                data[o + 2] = pb;
                data[o + 3] = a;
            }
            p
        }
        CanvasImageSrc::CanvasSnapshot { backing, ops } => {
            let iw = backing.0.ceil().max(1.0) as u32;
            let ih = backing.1.ceil().max(1.0) as u32;
            let Some(mut p) = Pixmap::new(iw, ih) else {
                return;
            };
            replay_canvas_into_backing(&mut p, ops, fonts, images);
            p
        }
        // A bare Canvas(id) should have been flattened to a snapshot by emit;
        // if one reaches here, no-op (documented).
        CanvasImageSrc::Canvas(_) => return,
    };

    let iw = src_pix.width() as f32;
    let ih = src_pix.height() as f32;
    // Source crop (default = whole image), clamped to the image bounds.
    let (cx, cy, cw, ch) = src_rect.unwrap_or((0.0, 0.0, iw, ih));
    let (cx, cy) = (cx.clamp(0.0, iw), cy.clamp(0.0, ih));
    let cw = cw.clamp(0.0, iw - cx);
    let ch = ch.clamp(0.0, ih - cy);
    if cw <= 0.0 || ch <= 0.0 {
        return;
    }
    let (dx, dy, dw, dh) = dst;
    let dw = dw.unwrap_or(cw);
    let dh = dh.unwrap_or(ch);
    if dw <= 0.0 || dh <= 0.0 {
        return;
    }

    // Map the crop region of the source onto the dest rect: a transform that
    // takes source-pixel coords → backing coords. Place the source pixmap at
    // (dx,dy) scaled by dw/cw, dh/ch, with the crop origin (cx,cy) translated to 0.
    let sx = dw / cw;
    let sy = dh / ch;
    let place = Transform::from_row(sx, 0.0, 0.0, sy, dx - cx * sx, dy - cy * sy);
    let full = st.transform.pre_concat(place);
    if !full.is_finite() {
        return;
    }
    let paint = PixmapPaint {
        opacity: st.global_alpha,
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    // Clip the draw to the dest rect (so a source crop doesn't bleed the rest of
    // the image outside the dst box) intersected with the active canvas clip.
    // The dest-rect mask is built under the CTM so it follows transforms.
    let clip = dest_rect_clip(back.width(), back.height(), dx, dy, dw, dh, st);
    back.draw_pixmap(0, 0, src_pix.as_ref(), &paint, full, clip.as_ref());
}

/// Build the clip mask for a `drawImage` dest rect `(dx,dy,dw,dh)` under the
/// CTM, intersected with the canvas's active clip. Returns `None` only if mask
/// allocation fails (the draw then proceeds unclipped, matching prior behavior).
fn dest_rect_clip(
    w: u32,
    h: u32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    st: &CanvasState,
) -> Option<Mask> {
    let r = SkRect::from_xywh(dx, dy, dw, dh)?;
    let path = PathBuilder::from_rect(r);
    let mut mask = match &st.clip {
        Some(m) => m.clone(),
        None => {
            let mut m = Mask::new(w, h)?;
            m.fill_path(&path, FillRule::Winding, true, st.transform);
            return Some(m);
        }
    };
    mask.intersect_path(&path, FillRule::Winding, true, st.transform);
    Some(mask)
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
            BlendMode::Normal,
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
        blit_image(
            &mut near,
            &dest,
            "g.png",
            &full,
            false,
            BlendMode::Normal,
            &images,
        );
        for x in 0..8 {
            let p = near.pixel(x, 0).unwrap();
            let pure = (p.red() == 255 && p.blue() == 0) || (p.red() == 0 && p.blue() == 255);
            assert!(pure, "nearest pixel {x} should be pure red/blue: {p:?}");
        }

        // Bilinear: at least one pixel near the boundary blends (both r and b > 0).
        let mut smooth = Pixmap::new(8, 1).unwrap();
        smooth.fill(Color::WHITE);
        blit_image(
            &mut smooth,
            &dest,
            "g.png",
            &full,
            true,
            BlendMode::Normal,
            &images,
        );
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
            blend: BlendMode::Normal,
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
            blend: BlendMode::Normal,
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

    // --- E21-M1: filter rasterization ---

    /// Rasterize a single opaque colored fill wrapped in a filter layer.
    fn render_filter(color: Rgba, filter: Vec<FilterFn>, w: u32, h: u32) -> Pixmap {
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: w as f32 / 2.0,
            height: h as f32,
        };
        let cmds = vec![
            PaintCmd::PushLayer {
                opacity: 1.0,
                filter,
                blend: BlendMode::Normal,
                mask: None,
            },
            PaintCmd::FillRect {
                rect,
                color,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PopLayer,
        ];
        rasterize(&cmds, w, h, &fonts, &empty_store())
    }

    fn red() -> Rgba {
        Rgba {
            r: 200,
            g: 40,
            b: 40,
            a: 255,
        }
    }

    #[test]
    fn filter_grayscale_equalizes_channels() {
        let pm = render_filter(red(), vec![FilterFn::Grayscale(1.0)], 20, 10);
        let p = pm.pixel(2, 5).unwrap();
        assert_eq!(p.red(), p.green(), "r==g after grayscale(1)");
        assert_eq!(p.green(), p.blue(), "g==b after grayscale(1)");
    }

    #[test]
    fn filter_brightness_halves_channels() {
        let plain = render_filter(red(), vec![], 20, 10);
        let dim = render_filter(red(), vec![FilterFn::Brightness(0.5)], 20, 10);
        let p0 = plain.pixel(2, 5).unwrap();
        let p1 = dim.pixel(2, 5).unwrap();
        // brightness(0.5) ≈ half each channel (±1 for rounding).
        assert!(
            (p1.red() as i32 - p0.red() as i32 / 2).abs() <= 2,
            "{p0:?} {p1:?}"
        );
        assert!((p1.green() as i32 - p0.green() as i32 / 2).abs() <= 2);
    }

    #[test]
    fn filter_invert_inverts() {
        let pm = render_filter(red(), vec![FilterFn::Invert(1.0)], 20, 10);
        let p = pm.pixel(2, 5).unwrap();
        // invert of (200,40,40) → (55,215,215).
        assert!((p.red() as i32 - 55).abs() <= 2, "{}", p.red());
        assert!((p.green() as i32 - 215).abs() <= 2, "{}", p.green());
        assert!((p.blue() as i32 - 215).abs() <= 2, "{}", p.blue());
    }

    #[test]
    fn filter_opacity_halves_alpha() {
        // opacity(0.5) on the layer → the fill composites over white at half alpha,
        // so the red channel sits between the fill (200) and white (255).
        let plain = render_filter(red(), vec![], 20, 10);
        let faded = render_filter(red(), vec![FilterFn::Opacity(0.5)], 20, 10);
        let p0 = plain.pixel(2, 5).unwrap();
        let p1 = faded.pixel(2, 5).unwrap();
        assert!(
            p1.red() > p0.red() && p1.red() < 255,
            "faded red {} between {} and 255",
            p1.red(),
            p0.red()
        );
    }

    #[test]
    fn filter_blur_spreads_color() {
        // A blurred fill bleeds color into the (originally white) right half.
        let blurred = render_filter(red(), vec![FilterFn::Blur(6.0)], 40, 10);
        let plain = render_filter(red(), vec![], 40, 10);
        // Just past the fill edge (x=20): plain is white, blurred is tinted.
        let bp = blurred.pixel(21, 5).unwrap();
        let pp = plain.pixel(21, 5).unwrap();
        assert!(pp.red() > 245 && pp.green() > 245, "plain edge is white");
        assert!(
            bp.red() < 250 || bp.green() < 250,
            "blur should spread color past the edge: {bp:?}"
        );
    }

    #[test]
    fn filter_drop_shadow_draws_offset_shadow() {
        let shadow_color = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        // Opaque red block (left half) with a blue shadow offset by (6,0), no blur.
        let pm = render_filter(
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            vec![FilterFn::DropShadow {
                dx: 6.0,
                dy: 0.0,
                blur: 0.0,
                color: shadow_color,
            }],
            40,
            10,
        );
        // The block (x<20) is still red on top.
        let blk = pm.pixel(2, 5).unwrap();
        assert_eq!((blk.red(), blk.green(), blk.blue()), (255, 0, 0));
        // Just past the block's right edge the shifted shadow shows blue.
        let sh = pm.pixel(24, 5).unwrap();
        assert!(
            sh.blue() > 200 && sh.red() < 60,
            "shadow blue at offset: {sh:?}"
        );
    }

    #[test]
    fn filter_empty_matches_opacity_path() {
        // An empty filter at opacity 0.5 must be byte-identical to the old
        // opacity-only path (apply_filters is a no-op on empty).
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let cmds = vec![
            PaintCmd::PushLayer {
                opacity: 0.5,
                filter: vec![],
                blend: BlendMode::Normal,
                mask: None,
            },
            PaintCmd::FillRect {
                rect,
                color: red(),
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PopLayer,
        ];
        let pm = rasterize(&cmds, 20, 10, &fonts, &empty_store());
        // The fill at half opacity over white: red between 200 and 255.
        let p = pm.pixel(2, 5).unwrap();
        assert!(p.red() > 220 && p.red() < 255, "{p:?}");
    }

    // --- E21-M2: blend-mode rasterization ---

    /// Fill the whole canvas with `back`, then a `blend` layer of `fore`, and
    /// return the resulting top-left pixel.
    fn render_blend_layer(back: Rgba, fore: Rgba, blend: BlendMode) -> (u8, u8, u8) {
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let cmds = vec![
            PaintCmd::FillRect {
                rect,
                color: back,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PushLayer {
                opacity: 1.0,
                filter: vec![],
                blend,
                mask: None,
            },
            PaintCmd::FillRect {
                rect,
                color: fore,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PopLayer,
        ];
        let pm = rasterize(&cmds, 10, 10, &fonts, &empty_store());
        let p = pm.pixel(2, 2).unwrap();
        (p.red(), p.green(), p.blue())
    }

    #[test]
    fn mix_blend_multiply_darkens() {
        // multiply: result = a*b/255 per channel. backdrop (128,128,128) ×
        // foreground (128,128,128) → ~64.
        let a = Rgba {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        let (r, g, b) = render_blend_layer(a, a, BlendMode::Multiply);
        let want = 128 * 128 / 255;
        assert!((r as i32 - want).abs() <= 2, "r {r} ~= {want}");
        assert!((g as i32 - want).abs() <= 2, "g {g} ~= {want}");
        assert!((b as i32 - want).abs() <= 2, "b {b} ~= {want}");
    }

    #[test]
    fn mix_blend_screen_lightens() {
        // screen: result = 255 - (255-a)(255-b)/255. (128,128,128) over itself
        // → ~192, i.e. lighter than either input.
        let a = Rgba {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        let (r, _, _) = render_blend_layer(a, a, BlendMode::Screen);
        let want = 255 - (255 - 128) * (255 - 128) / 255;
        assert!((r as i32 - want).abs() <= 2, "r {r} ~= {want}");
        assert!(r > 128, "screen must lighten: {r}");
    }

    #[test]
    fn mix_blend_normal_is_source_over() {
        // Byte-identity sentinel: a Normal layer == an opaque source-over fill,
        // so the foreground fully replaces the backdrop.
        let back = Rgba {
            r: 200,
            g: 40,
            b: 40,
            a: 255,
        };
        let fore = Rgba {
            r: 40,
            g: 200,
            b: 40,
            a: 255,
        };
        let (r, g, b) = render_blend_layer(back, fore, BlendMode::Normal);
        assert_eq!((r, g, b), (40, 200, 40));
    }

    #[test]
    fn background_blend_mode_multiply_between_layers() {
        // An isolated bg group: a (128,128,128) bottom layer, then a multiply
        // (128,128,128) top layer → ~64 inside the group, composited over white.
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let grey = Rgba {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        let cmds = vec![
            PaintCmd::PushBgGroup,
            // bottom layer (source-over, the group's base).
            PaintCmd::FillRect {
                rect,
                color: grey,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            // top layer multiplies with the bottom.
            PaintCmd::FillRect {
                rect,
                color: grey,
                radius: [0.0; 4],
                blend: BlendMode::Multiply,
            },
            PaintCmd::PopBgGroup,
        ];
        let pm = rasterize(&cmds, 10, 10, &fonts, &empty_store());
        let p = pm.pixel(2, 2).unwrap();
        let want = 128 * 128 / 255;
        assert!(
            (p.red() as i32 - want).abs() <= 2,
            "multiplied bg {} ~= {want}",
            p.red()
        );
    }

    // --- E32-M3: isolation confines descendant blending ---

    #[test]
    fn isolation_confines_blend_to_group() {
        // A grey backdrop with a child `mix-blend-mode: multiply` red layer.
        //
        // NOT isolated: the child blends against the grey backdrop directly →
        // multiply darkens (red channel stays 255, but green/blue go to 0 × grey
        // = 0; the visible effect is the grey being multiplied, so the pixel is
        // NOT the pure source red).
        //
        // ISOLATED: an extra Normal parent layer wraps the child, so the child
        // multiplies against the group's fresh transparent pixmap (a no-op), then
        // the group composites source-over onto the grey → the child's pure red
        // lands on top, unblended. The two results MUST differ.
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let grey = Rgba {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        let red = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let multiply_child = vec![
            PaintCmd::PushLayer {
                opacity: 1.0,
                filter: vec![],
                blend: BlendMode::Multiply,
                mask: None,
            },
            PaintCmd::FillRect {
                rect,
                color: red,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PopLayer,
        ];

        // Non-isolated: backdrop then the multiply child directly on the page.
        let mut not_iso = vec![PaintCmd::FillRect {
            rect,
            color: grey,
            radius: [0.0; 4],
            blend: BlendMode::Normal,
        }];
        not_iso.extend(multiply_child.clone());
        let pm_not = rasterize(&not_iso, 10, 10, &fonts, &empty_store());
        let p_not = pm_not.pixel(2, 2).unwrap();

        // Isolated: backdrop, then a Normal group bracketing the same child.
        let mut iso = vec![PaintCmd::FillRect {
            rect,
            color: grey,
            radius: [0.0; 4],
            blend: BlendMode::Normal,
        }];
        iso.push(PaintCmd::PushLayer {
            opacity: 1.0,
            filter: vec![],
            blend: BlendMode::Normal,
            mask: None,
        });
        iso.extend(multiply_child);
        iso.push(PaintCmd::PopLayer);
        let pm_iso = rasterize(&iso, 10, 10, &fonts, &empty_store());
        let p_iso = pm_iso.pixel(2, 2).unwrap();

        // The isolated group blends against transparent, so the child's pure red
        // survives; the non-isolated case multiplies against grey and differs.
        assert_eq!(
            (p_iso.red(), p_iso.green(), p_iso.blue()),
            (255, 0, 0),
            "isolated child must keep its source red"
        );
        assert_ne!(
            (p_not.red(), p_not.green(), p_not.blue()),
            (p_iso.red(), p_iso.green(), p_iso.blue()),
            "isolation must change the blended result"
        );
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
                blend: BlendMode::Normal,
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

    // --- E20-M2: canvas state / transform / gradient / clip / alpha replay ---

    use starfish_dom::{CanvasColor, CanvasGradient, CanvasGradientKind, CanvasOp};

    /// Rasterize a single `PaintCmd::Canvas` covering the whole `s×s` pixmap
    /// (backing == box, so sx=sy=1, canvas coords == device coords).
    fn render_canvas(ops: Vec<CanvasOp>, s: u32) -> Pixmap {
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::Canvas {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: s as f32,
                height: s as f32,
            },
            backing: (s as f32, s as f32),
            ops,
        }];
        rasterize(&cmds, s, s, &fonts, &empty_store())
    }

    #[test]
    fn canvas_linear_gradient_fill_is_a_ramp() {
        // Left red, right blue across a 40-wide fill. The left edge is reddish, the
        // right edge is bluish — a visible ramp.
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = CanvasColor {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let g = CanvasGradient {
            kind: CanvasGradientKind::Linear {
                x0: 0.0,
                y0: 0.0,
                x1: 40.0,
                y1: 0.0,
            },
            stops: vec![(0.0, red), (1.0, blue)],
        };
        let pm = render_canvas(
            vec![
                CanvasOp::SetFillStyleGradient(g),
                CanvasOp::FillRect(0.0, 0.0, 40.0, 40.0),
            ],
            40,
        );
        let left = pm.pixel(2, 20).unwrap();
        let right = pm.pixel(37, 20).unwrap();
        assert!(left.red() > left.blue(), "left edge reddish: {left:?}");
        assert!(right.blue() > right.red(), "right edge bluish: {right:?}");
    }

    #[test]
    fn canvas_translate_moves_fill() {
        // Without translate a 10×10 fill at (0,0) covers the top-left; with
        // translate(20,20) it lands at (20,20).
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let pm = render_canvas(
            vec![
                CanvasOp::SetFillStyle(red),
                CanvasOp::Transform(1.0, 0.0, 0.0, 1.0, 20.0, 20.0),
                CanvasOp::FillRect(0.0, 0.0, 10.0, 10.0),
            ],
            40,
        );
        // (5,5) is untouched (white); (25,25) is red.
        assert_eq!(pm.pixel(5, 5).unwrap().red(), 255);
        assert_eq!(pm.pixel(5, 5).unwrap().green(), 255);
        let moved = pm.pixel(25, 25).unwrap();
        assert_eq!((moved.red(), moved.green(), moved.blue()), (255, 0, 0));
    }

    #[test]
    fn canvas_global_alpha_halves_fill() {
        // Red fill at alpha 0.5 over the (transparent backing over) white page →
        // a pink ~ (255,128,128).
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let pm = render_canvas(
            vec![
                CanvasOp::SetFillStyle(red),
                CanvasOp::SetGlobalAlpha(0.5),
                CanvasOp::FillRect(0.0, 0.0, 20.0, 20.0),
            ],
            20,
        );
        let p = pm.pixel(10, 10).unwrap();
        // The backing pixel has alpha ~128; composited over white it lightens.
        assert_eq!(p.red(), 255);
        assert!(p.green() > 100 && p.green() < 160, "halved green: {p:?}");
        assert!(p.blue() > 100 && p.blue() < 160, "halved blue: {p:?}");
    }

    #[test]
    fn canvas_clip_limits_later_fill() {
        // Clip to a 10×10 rect, then fill 40×40 red. Only inside the clip is red.
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let pm = render_canvas(
            vec![
                CanvasOp::BeginPath,
                CanvasOp::Rect(0.0, 0.0, 10.0, 10.0),
                CanvasOp::Clip,
                CanvasOp::SetFillStyle(red),
                CanvasOp::FillRect(0.0, 0.0, 40.0, 40.0),
            ],
            40,
        );
        let inside = pm.pixel(5, 5).unwrap();
        assert_eq!((inside.red(), inside.green(), inside.blue()), (255, 0, 0));
        let outside = pm.pixel(25, 25).unwrap();
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255),
            "outside the clip stays white"
        );
    }

    #[test]
    fn canvas_save_restore_isolates_transform() {
        // save → translate → fill (lands at 25,25); restore → fill at (0,0) lands
        // at the origin (the translate is undone).
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let pm = render_canvas(
            vec![
                CanvasOp::SetFillStyle(red),
                CanvasOp::Save,
                CanvasOp::Transform(1.0, 0.0, 0.0, 1.0, 20.0, 20.0),
                CanvasOp::FillRect(0.0, 0.0, 5.0, 5.0),
                CanvasOp::Restore,
                CanvasOp::FillRect(0.0, 0.0, 5.0, 5.0),
            ],
            40,
        );
        // translated fill at (22,22) is red; post-restore fill at (2,2) is red.
        assert_eq!(pm.pixel(22, 22).unwrap().red(), 255);
        assert_eq!(pm.pixel(22, 22).unwrap().green(), 0);
        let origin = pm.pixel(2, 2).unwrap();
        assert_eq!((origin.red(), origin.green(), origin.blue()), (255, 0, 0));
    }

    #[test]
    fn canvas_m1_only_ops_replay_unchanged() {
        // BYTE-IDENTITY: an M1-only op stream (fillRect + arc fill, no M2 ops)
        // replays exactly as the M1 default state (identity transform, alpha 1, no
        // clip, solid color). A fillRect red lands where M1 put it.
        let red = CanvasColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let ops = vec![
            CanvasOp::SetFillStyle(red),
            CanvasOp::FillRect(5.0, 5.0, 20.0, 20.0),
            CanvasOp::BeginPath,
            CanvasOp::Arc(30.0, 30.0, 8.0, 0.0, std::f32::consts::TAU, false),
            CanvasOp::Fill,
        ];
        let pm = render_canvas(ops, 40);
        // Inside the fillRect.
        assert_eq!(
            {
                let p = pm.pixel(10, 10).unwrap();
                (p.red(), p.green(), p.blue())
            },
            (255, 0, 0)
        );
        // Inside the arc fill.
        let arc = pm.pixel(30, 30).unwrap();
        assert_eq!((arc.red(), arc.green(), arc.blue()), (255, 0, 0));
        // An undrawn corner shows the white page.
        let bg = pm.pixel(38, 2).unwrap();
        assert_eq!((bg.red(), bg.green(), bg.blue()), (255, 255, 255));
    }

    // --- E20-M3: text + image ---

    use starfish_dom::{CanvasImageSrc, CanvasTextBaseline};

    /// Count non-white (i.e. drawn) pixels in a region of the pixmap.
    fn count_drawn(pm: &Pixmap, x0: u32, y0: u32, x1: u32, y1: u32) -> u32 {
        let mut n = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = pm.pixel(x, y).unwrap();
                if !(p.red() > 245 && p.green() > 245 && p.blue() > 245) {
                    n += 1;
                }
            }
        }
        n
    }

    fn black() -> CanvasColor {
        CanvasColor {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }
    }

    #[test]
    fn canvas_fill_text_draws_pixels() {
        // A black "H" at a large size leaves drawn (non-white) pixels near the
        // baseline position.
        let pm = render_canvas(
            vec![
                CanvasOp::SetFillStyle(black()),
                CanvasOp::SetFont {
                    size_px: 30.0,
                    family: vec!["sans-serif".to_string()],
                    weight: 400,
                    italic: false,
                },
                CanvasOp::FillText {
                    text: "H".to_string(),
                    x: 5.0,
                    y: 40.0,
                    max_width: None,
                },
            ],
            60,
        );
        // Alphabetic baseline at y=40: glyph body sits above it (roughly y 15..40).
        assert!(
            count_drawn(&pm, 0, 10, 40, 42) > 20,
            "expected drawn glyph pixels above the baseline"
        );
    }

    #[test]
    fn canvas_text_baseline_shifts_y() {
        // Same text/x; `top` baseline draws the glyph BELOW y, `alphabetic` draws
        // it ABOVE y. Compare drawn-pixel counts in the band just below y.
        let mk = |bl: CanvasTextBaseline| {
            render_canvas(
                vec![
                    CanvasOp::SetFillStyle(black()),
                    CanvasOp::SetFont {
                        size_px: 30.0,
                        family: vec!["sans-serif".to_string()],
                        weight: 400,
                        italic: false,
                    },
                    CanvasOp::SetTextBaseline(bl),
                    CanvasOp::FillText {
                        text: "H".to_string(),
                        x: 5.0,
                        y: 20.0,
                        max_width: None,
                    },
                ],
                60,
            )
        };
        let top = mk(CanvasTextBaseline::Top);
        let alpha = mk(CanvasTextBaseline::Alphabetic);
        // Below y=20: `top` has the glyph there (many drawn pixels), `alphabetic`
        // has it above (few drawn pixels below).
        let below_top = count_drawn(&top, 0, 22, 40, 50);
        let below_alpha = count_drawn(&alpha, 0, 22, 40, 50);
        assert!(
            below_top > below_alpha + 10,
            "top baseline draws below y, alphabetic above: top={below_top} alpha={below_alpha}"
        );
    }

    #[test]
    fn canvas_fill_text_respects_ctm_and_clip() {
        // Clip to a small rect that excludes the glyph, then fillText: nothing of
        // the glyph appears outside the clip.
        let pm = render_canvas(
            vec![
                CanvasOp::BeginPath,
                CanvasOp::Rect(0.0, 0.0, 4.0, 4.0),
                CanvasOp::Clip,
                CanvasOp::SetFillStyle(black()),
                CanvasOp::SetFont {
                    size_px: 30.0,
                    family: vec!["sans-serif".to_string()],
                    weight: 400,
                    italic: false,
                },
                CanvasOp::FillText {
                    text: "H".to_string(),
                    x: 5.0,
                    y: 40.0,
                    max_width: None,
                },
            ],
            60,
        );
        // The glyph (around x 5.., y 15..40) is entirely outside the 4×4 clip →
        // no drawn pixels there.
        assert_eq!(
            count_drawn(&pm, 5, 15, 40, 42),
            0,
            "clipped-out glyph leaves no pixels"
        );
    }

    #[test]
    fn canvas_draw_image_composites_known_color() {
        // A 2×2 all-green decoded image drawn at (5,5) size 10×10 → green pixels.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-cimg-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 2);
        for y in 0..2 {
            for x in 0..2 {
                img.put_pixel(x, y, image::Rgba([0, 200, 0, 255]));
            }
        }
        img.save(dir.join("g.png")).unwrap();
        let mut images = store_for(&dir);
        images.get("g.png").expect("decoded 2x2");

        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::Canvas {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
            backing: (40.0, 40.0),
            ops: vec![CanvasOp::DrawImage {
                source: CanvasImageSrc::Url("g.png".to_string()),
                src_rect: None,
                dst: (5.0, 5.0, Some(10.0), Some(10.0)),
            }],
        }];
        let pm = rasterize(&cmds, 40, 40, &fonts, &images);
        let inside = pm.pixel(10, 10).unwrap();
        assert!(
            inside.green() > 150 && inside.red() < 80 && inside.blue() < 80,
            "image pixel should be green inside the dst rect: {inside:?}"
        );
        // Outside the dst rect stays white.
        let outside = pm.pixel(30, 30).unwrap();
        assert!(outside.red() > 240 && outside.green() > 240 && outside.blue() > 240);
    }

    #[test]
    fn canvas_draw_image_canvas_snapshot_composites() {
        // A CanvasSnapshot (a red fillRect over its whole 10×10 backing) drawn into
        // the parent canvas at (2,2) size 10×10 → red there.
        let snapshot = CanvasImageSrc::CanvasSnapshot {
            backing: (10.0, 10.0),
            ops: vec![
                CanvasOp::SetFillStyle(CanvasColor {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                CanvasOp::FillRect(0.0, 0.0, 10.0, 10.0),
            ],
        };
        let pm = render_canvas(
            vec![CanvasOp::DrawImage {
                source: snapshot,
                src_rect: None,
                dst: (2.0, 2.0, Some(10.0), Some(10.0)),
            }],
            40,
        );
        let inside = pm.pixel(6, 6).unwrap();
        assert_eq!((inside.red(), inside.green(), inside.blue()), (255, 0, 0));
        let outside = pm.pixel(30, 30).unwrap();
        assert!(outside.red() > 240 && outside.green() > 240 && outside.blue() > 240);
    }

    // --- E21-M3: mask-image + backdrop-filter rasterization ---

    use starfish_style::{BgRepeat, BgSize, GradientStop, LengthPct, LinearGradient, MaskSpec};

    fn stop(color: Rgba, pos: f32) -> GradientStop {
        GradientStop {
            color,
            pos: Some(pos),
        }
    }

    /// Fill `(0,0,w,h)` with `color`, wrapped in a layer carrying `mask`.
    fn render_masked(color: Rgba, mask: MaskBox, w: u32, h: u32) -> Pixmap {
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: w as f32,
            height: h as f32,
        };
        let cmds = vec![
            PaintCmd::PushLayer {
                opacity: 1.0,
                filter: vec![],
                blend: BlendMode::Normal,
                mask: Some(mask),
            },
            PaintCmd::FillRect {
                rect,
                color,
                radius: [0.0; 4],
                blend: BlendMode::Normal,
            },
            PaintCmd::PopLayer,
        ];
        rasterize(&cmds, w, h, &fonts, &empty_store())
    }

    fn mask_box(image: MaskImage, mode: MaskMode, w: f32, h: f32) -> MaskBox {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: w,
            height: h,
        };
        MaskBox {
            spec: MaskSpec {
                image,
                mode,
                size: BgSize::Auto,
                position: (LengthPct::Px(0.0), LengthPct::Px(0.0)),
                repeat: BgRepeat::Repeat,
                origin: MaskGeometryBox::BorderBox,
                clip: MaskGeometryBox::BorderBox,
            },
            rect,
            padding_box: rect,
            content_box: rect,
            radius: [0.0; 4],
        }
    }

    fn pure_red() -> Rgba {
        Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        }
    }

    #[test]
    fn alpha_gradient_mask_fades_fill() {
        // A horizontal alpha gradient (opaque black → transparent) fades a red fill
        // from full at the left to nearly gone at the right.
        let opaque = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let clear = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        // angle_deg 90 = "to right" (gradient line points right).
        let g = LinearGradient {
            angle_deg: 90.0,
            stops: vec![stop(opaque, 0.0), stop(clear, 1.0)],
        };
        let pm = render_masked(
            pure_red(),
            mask_box(MaskImage::Gradient(g), MaskMode::Alpha, 40.0, 10.0),
            40,
            10,
        );
        let left = pm.pixel(1, 5).unwrap();
        let right = pm.pixel(38, 5).unwrap();
        // Left: red preserved over white → strong red. Right: masked away → white.
        assert!(left.red() > 200, "left should stay red: {left:?}");
        assert!(
            right.red() > 240 && right.green() > 240,
            "right masked to white: {right:?}"
        );
        assert!(left.green() < right.green(), "alpha increases left→right");
    }

    #[test]
    fn luminance_mask_fades_fill() {
        // A luminance gradient (white → black) over a fully opaque source: white
        // (lum 255) keeps the fill, black (lum 0) masks it away.
        let white = Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let black = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let g = LinearGradient {
            angle_deg: 90.0,
            stops: vec![stop(white, 0.0), stop(black, 1.0)],
        };
        let pm = render_masked(
            pure_red(),
            mask_box(MaskImage::Gradient(g), MaskMode::Luminance, 40.0, 10.0),
            40,
            10,
        );
        let left = pm.pixel(1, 5).unwrap();
        let right = pm.pixel(38, 5).unwrap();
        assert!(left.red() > 200, "white-lum keeps red: {left:?}");
        assert!(
            right.red() > 240 && right.green() > 240,
            "black-lum masks to white: {right:?}"
        );
    }

    #[test]
    fn no_mask_layer_byte_identical() {
        // A layer with mask:None must be byte-identical to no-mask compositing.
        let fonts = FontDb::load().unwrap();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let mk = |mask: Option<MaskBox>| {
            let cmds = vec![
                PaintCmd::PushLayer {
                    opacity: 1.0,
                    filter: vec![],
                    blend: BlendMode::Normal,
                    mask,
                },
                PaintCmd::FillRect {
                    rect,
                    color: pure_red(),
                    radius: [0.0; 4],
                    blend: BlendMode::Normal,
                },
                PaintCmd::PopLayer,
            ];
            rasterize(&cmds, 10, 10, &fonts, &empty_store())
        };
        assert_eq!(mk(None).data(), mk(None).data());
    }

    #[test]
    fn backdrop_filter_blurs_backdrop() {
        // A sharp red/blue split backdrop; a backdrop-filter blur over the seam
        // smears the colors so a pixel near the seam is no longer pure red/blue.
        let fonts = FontDb::load().unwrap();
        let blue = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let half = |x: f32, color: Rgba| PaintCmd::FillRect {
            rect: Rect {
                x,
                y: 0.0,
                width: 20.0,
                height: 40.0,
            },
            color,
            radius: [0.0; 4],
            blend: BlendMode::Normal,
        };
        let region = Rect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 40.0,
        };
        let cmds = vec![
            half(0.0, pure_red()),
            half(20.0, blue),
            PaintCmd::ApplyBackdropFilter {
                rect: region,
                filter: vec![FilterFn::Blur(8.0)],
            },
        ];
        let pm = rasterize(&cmds, 40, 40, &fonts, &empty_store());
        // Near the seam, blur mixes red+blue → some blue bleeds left of x=20.
        let near_seam = pm.pixel(18, 20).unwrap();
        assert!(
            near_seam.blue() > 10,
            "blur should bleed blue across the seam: {near_seam:?}"
        );
    }

    #[test]
    fn empty_backdrop_filter_no_op() {
        // An empty filter leaves the backdrop byte-identical.
        let fonts = FontDb::load().unwrap();
        let bg = PaintCmd::FillRect {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            color: pure_red(),
            radius: [0.0; 4],
            blend: BlendMode::Normal,
        };
        let region = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let with = rasterize(
            &[
                bg.clone(),
                PaintCmd::ApplyBackdropFilter {
                    rect: region,
                    filter: vec![],
                },
            ],
            10,
            10,
            &fonts,
            &empty_store(),
        );
        let without = rasterize(&[bg], 10, 10, &fonts, &empty_store());
        assert_eq!(with.data(), without.data());
    }
}
