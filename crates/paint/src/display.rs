//! The flat display list (M5 §3): a pre-order walk of the box tree turning each
//! box into background/border fill-rects and each text run into a glyph run, in
//! correct paint order (parent before child; bg → border → text).

use starfish_dom::{CanvasImageSrc, CanvasOp, Document, NodeId};
use starfish_layout::{
    control_label, form_control_kind, input_display, parse_view_box, range_fraction, range_values,
    selected_option_text, textarea_value, BoxKind, FontQuery, FormControl, LayoutBox, Rect,
    ViewBox,
};
use starfish_style::{
    BackgroundLayer, BgImage, BgSize, BgSizeAxis, BlendMode, BorderStyle, BoxShadow, ClipShape,
    ComputedStyle, ConicGradient, FilterFn, Float, FontStyle, FontWeight, ImageRendering, Length,
    LengthPct,
    LinearGradient, ObjectFit, Outline, Overflow, Position, RadialGradient, Rgba, StyledTree,
    TextDecorationLine, TextOrientation, TransformFn,
};
use tiny_skia::Transform;

use crate::font::FontDb;
use crate::image_store::ImageStore;

// Re-export the mask types so the rasterizer can refer to them via `crate::display`.
pub use starfish_style::{MaskGeometryBox, MaskImage, MaskMode, MaskSpec};

/// A resolved mask box (E21-M3): the computed `mask` spec plus the box geometry
/// the mask source is rendered against (the border box + its corner radii). The
/// source's coverage multiplies the offscreen layer's alpha on pop. `padding_box`
/// and `content_box` (E32-M2) let the rasterizer resolve `mask-origin`/`mask-clip`.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskBox {
    pub spec: MaskSpec,
    pub rect: Rect, // border box
    pub padding_box: Rect,
    pub content_box: Rect,
    pub radius: [f32; 4],
}

/// A device-space (page-space) paint command. Coordinates are f32 page pixels;
/// the rasterizer rounds. Colors are straight (non-premultiplied) `Rgba`.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintCmd {
    /// A filled rectangle (a background or one border edge). `radius` is per
    /// corner (TL,TR,BR,BL); all-zero = sharp corners (the fast path).
    FillRect {
        rect: Rect,
        color: Rgba,
        radius: [f32; 4],
        /// Blend mode against the backdrop (E21-M2). `Normal` (the default for
        /// every emit site except a `background-blend-mode` group) → source-over.
        blend: BlendMode,
    },
    /// A stroked border edge for a non-solid sharp border (`dashed`/`dotted`/
    /// `double`, E13-M4). `from`/`to` is the edge's center line; `width` is the
    /// border width; `style` selects the dash/double pattern in raster.
    StrokeLine {
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: Rgba,
        style: BorderStyle,
    },
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
    /// Blit a decoded image into `dest`. `src` is the raw `<img>` src; the
    /// rasterizer looks the pixels up in the `ImageStore` (E2-M4 §7). `src_crop`
    /// is the source sub-rect (in SOURCE pixels) to sample — object-fit maps it
    /// into `dest` (E15-M1). `smooth` selects bilinear (vs nearest) sampling per
    /// `image-rendering`. The default `object-fit: fill` + `image-rendering: auto`
    /// gives `src_crop` = the full image rect + `smooth: false` (byte-identical
    /// to the original nearest-neighbour stretch).
    ImageBlit {
        dest: Rect,
        src: String,
        src_crop: Rect,
        smooth: bool,
        /// Blend mode against the backdrop (E21-M2). `Normal` everywhere except a
        /// `background-blend-mode` group → source-over.
        blend: BlendMode,
    },
    /// A linear-gradient-filled rect (E2-M5 §1.4), optionally rounded.
    GradientRect {
        rect: Rect,
        gradient: LinearGradient,
        radius: [f32; 4],
        /// Blend mode against the backdrop (E21-M2). `Normal` everywhere except a
        /// `background-blend-mode` group → source-over.
        blend: BlendMode,
    },
    /// A radial-gradient-filled rect (E16-M3), optionally rounded.
    RadialRect {
        rect: Rect,
        gradient: RadialGradient,
        radius: [f32; 4],
        /// Blend mode against the backdrop (E21-M2). `Normal` everywhere except a
        /// `background-blend-mode` group → source-over.
        blend: BlendMode,
    },
    /// A conic-gradient-filled rect (E16-M3), optionally rounded.
    ConicRect {
        rect: Rect,
        gradient: ConicGradient,
        radius: [f32; 4],
        /// Blend mode against the backdrop (E21-M2). `Normal` everywhere except a
        /// `background-blend-mode` group → source-over.
        blend: BlendMode,
    },
    /// A text-shadow glyph layer (E16-M3): the same shaped run as a `GlyphRun`,
    /// painted at the shadow offset in the shadow color, optionally blurred. Emitted
    /// just before the matching `GlyphRun`.
    GlyphShadow {
        origin: (f32, f32),
        text: String,
        font_size: f32,
        weight: FontWeight,
        style: FontStyle,
        family: Vec<String>,
        color: Rgba,
        ascent: f32,
        letter_spacing: f32,
        word_spacing: f32,
        /// Gaussian blur radius in px (0 = sharp).
        blur: f32,
    },
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
    /// Begin an offscreen layer wrapping the box + its subtree (§4.2). The layer
    /// is composited at `opacity` on pop; `filter` (E21-M1) is applied to the
    /// layer pixels before compositing (empty = none); `blend` (E21-M2,
    /// `mix-blend-mode`) selects the composite mode against the backdrop (`Normal`
    /// = source-over).
    PushLayer {
        opacity: f32,
        filter: Vec<FilterFn>,
        blend: BlendMode,
        /// `mask-image` (E21-M3): when `Some`, the layer's alpha is multiplied by
        /// the mask source's coverage (after `filter`) before compositing. `None`
        /// = no mask (byte-identical to the pre-M3 layer).
        mask: Option<MaskBox>,
    },
    /// Composite the current layer at its opacity, after applying its filter (§4.2).
    PopLayer,
    /// Apply a `backdrop-filter` (E21-M3) to the current backdrop region `rect`
    /// in place: snapshot the destination's `rect`, filter it, and draw it back.
    /// Emitted BEFORE the box's own `PushLayer`, so it filters the parent backdrop
    /// rather than the box's own fresh layer. Empty `filter` = no-op.
    ApplyBackdropFilter { rect: Rect, filter: Vec<FilterFn> },
    /// Begin an isolated background sub-layer for `background-blend-mode` (E21-M2):
    /// the bg color + each image layer are drawn into a transparent offscreen, the
    /// later layers blending with the earlier ones, then the group is composited
    /// source-over onto the backdrop (isolation).
    PushBgGroup,
    /// Composite the current bg group source-over onto the backdrop (E21-M2).
    PopBgGroup,
    /// Begin an offscreen transform layer wrapping the box + its subtree. The
    /// subtree is painted at its normal absolute position into the layer; the
    /// layer is composited back via `draw_pixmap` with `matrix` (E5-M3 §4).
    /// `matrix` is a,b,c,d,e,f (→ `Transform::from_row`).
    PushTransform { matrix: [f32; 6] },
    /// Composite the current transform layer through its matrix.
    PopTransform,
    /// Begin a clip region for `overflow: hidden|clip` (E13-M4): children/floats/
    /// positioned descendants painted until the matching `PopClip` are clipped to
    /// `rect` (the padding box), with per-corner `radius`. The box's own bg/border
    /// are emitted before the push, so they are not clipped.
    PushClip { rect: Rect, radius: [f32; 4] },
    /// Begin a `clip-path` region (E32-M1): the box (bg/border + content +
    /// descendants) is clipped to `shape` resolved against `border_box`. Closed
    /// by `PopClip`.
    PushClipPath { shape: ClipShape, border_box: Rect },
    /// Composite the clip layer back through its clip mask (E13-M4).
    PopClip,
    /// A single SVG shape, flattened from an `<svg>` subtree at build time
    /// (E9-M1 §5.2). `transform` (a,b,c,d,e,f) is the effective user→canvas
    /// transform (viewBox · ancestor `<g>` · own `transform`, E9-M3 §1);
    /// geometry is in user coords. `fill`/`stroke` are already alpha-folded
    /// (shape opacity × paint opacity); `None` ⇒ no paint. (E9-M3 §4)
    SvgShape {
        geom: SvgGeom,
        transform: [f32; 6],
        fill: Option<SvgPaint>,
        /// Fill rule for the geometry (E9-M2 §5).
        fill_rule: SvgFillRule,
        stroke: Option<SvgPaint>,
        /// Stroke width in USER units (scaled by `transform`).
        stroke_width: f32,
        /// Stroke line cap (E9-M2 §5).
        stroke_cap: SvgLineCap,
        /// Stroke line join (E9-M2 §5).
        stroke_join: SvgLineJoin,
        /// The shape's user-space bounding box (objectBoundingBox mapping, §4.4).
        bbox: Rect,
    },
    /// A `<canvas>` 2D bitmap (E20-M1): `ops` are replayed into a transparent
    /// `backing` (CSS-px) pixmap, then that pixmap is scaled/composited into
    /// `rect` (the canvas content box) source-over.
    Canvas {
        rect: Rect,
        /// Backing pixmap size in canvas-coordinate px (width attr, height attr).
        backing: (f32, f32),
        ops: Vec<starfish_dom::CanvasOp>,
    },
}

/// A resolved SVG paint: a solid color or a gradient (E9-M3 §4.1).
#[derive(Debug, Clone, PartialEq)]
pub enum SvgPaint {
    Color(Rgba),
    Gradient(SvgGradient),
}

/// A gradient fully resolved at build time except the objectBoundingBox→user
/// mapping, which raster applies once it knows the shape's bbox (E9-M3 §4.4).
#[derive(Debug, Clone, PartialEq)]
pub struct SvgGradient {
    pub kind: GradKind,
    /// Reuses `{color, pos}` (resolve_stops-compatible, E9-M3 §4.2).
    pub stops: Vec<starfish_style::GradientStop>,
    pub units: GradUnits,
}

/// The gradient geometry: linear endpoints or a radial center+radius (§4.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradKind {
    Linear { x1: f32, y1: f32, x2: f32, y2: f32 },
    Radial { cx: f32, cy: f32, r: f32 },
}

/// `gradientUnits` (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

/// `id` → resolved gradient (E9-M3 §4.2).
type GradientRegistry = std::collections::HashMap<String, SvgGradient>;

/// The geometry of one SVG basic shape, in user-space coordinates (E9-M1 §5.2).
/// NOTE: not `Copy` — `Path` carries a `Vec` (E9-M2 §3.1).
#[derive(Debug, Clone, PartialEq)]
pub enum SvgGeom {
    /// Rectangle (`rx`/`ry` corner radii; 0 ⇒ sharp).
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rx: f32,
        ry: f32,
    },
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
    PaintCmd::FillRect {
        rect,
        color,
        radius: [0.0; 4],
        blend: BlendMode::Normal,
    }
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
    let Some(s) = b.style(styled) else {
        return Role::InFlow;
    };
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

    // backdrop-filter (E21-M3): filter the parent backdrop UNDER this box, so it
    // must be emitted BEFORE the box's own PushLayer (which opens a fresh, empty
    // layer). Empty backdrop-filter → no command (fast path).
    if let Some((rect, filter)) = backdrop_of(b, styled) {
        out.push(PaintCmd::ApplyBackdropFilter { rect, filter });
    }

    // Opacity < 1 wraps the box AND its whole subtree in an offscreen layer so
    // overlapping descendants composite as a group (E2-M5 §4.2). opacity == 1.0
    // → no layer (fast path, unchanged output). A `mask` (E21-M3) likewise forces
    // a layer.
    let layer = layer_effect(b, styled);
    if let Some((o, ref filter, blend, ref mask)) = layer {
        out.push(PaintCmd::PushLayer {
            opacity: o,
            filter: filter.clone(),
            blend,
            mask: mask.clone(),
        });
    }

    // clip-path (E32-M1) clips the WHOLE box (bg/border + content), so it pushes
    // BEFORE emit_self and pops last.
    let cpath = clip_path_of(b, styled);
    if let Some((shape, bbox)) = &cpath {
        out.push(PaintCmd::PushClipPath {
            shape: shape.clone(),
            border_box: *bbox,
        });
    }
    emit_self(b, styled, fonts, images, doc, out);
    // overflow: hidden|clip clips this box's descendants (in-flow + out-of-flow)
    // to its padding box. The box's own bg/border (emit_self above) are NOT
    // clipped. Visible → no clip (fast path, identical output).
    let clip = clip_of(b, styled);
    if let Some((rect, radius)) = clip {
        out.push(PaintCmd::PushClip { rect, radius });
    }
    for child in b.children() {
        collect_inflow(
            child,
            styled,
            fonts,
            images,
            doc,
            out,
            &mut floats,
            &mut positioned,
        );
    }
    for f in floats {
        paint_subtree(f, styled, fonts, images, doc, out);
    }
    for p in positioned {
        paint_subtree(p, styled, fonts, images, doc, out);
    }
    if clip.is_some() {
        out.push(PaintCmd::PopClip);
    }
    if cpath.is_some() {
        out.push(PaintCmd::PopClip);
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
            // backdrop-filter (E21-M3): filter the parent backdrop UNDER this box,
            // so it must be emitted BEFORE the box's own PushLayer. Empty → no cmd.
            if let Some((rect, filter)) = backdrop_of(b, styled) {
                out.push(PaintCmd::ApplyBackdropFilter { rect, filter });
            }
            // opacity < 1 wraps this in-flow box + its in-flow descendants in an
            // offscreen layer (E2-M5 §4.2). Out-of-flow descendants re-ordered
            // into the float/positioned buckets paint outside the bracket — an
            // accepted M5 edge (they are rare under opacity boxes). A `mask`
            // (E21-M3) likewise forces a layer.
            let layer = layer_effect(b, styled);
            if let Some((o, ref filter, blend, ref mask)) = layer {
                out.push(PaintCmd::PushLayer {
                    opacity: o,
                    filter: filter.clone(),
                    blend,
                    mask: mask.clone(),
                });
            }
            // clip-path (E32-M1) clips the whole box (bg/border + content).
            let cpath = clip_path_of(b, styled);
            if let Some((shape, bbox)) = &cpath {
                out.push(PaintCmd::PushClipPath {
                    shape: shape.clone(),
                    border_box: *bbox,
                });
            }
            emit_self(b, styled, fonts, images, doc, out);
            // overflow clip wraps this box's in-flow descendants (E13-M4); the
            // box's own bg/border (emit_self) stay unclipped. Out-of-flow
            // descendants re-ordered into the buckets paint outside this clip —
            // the same accepted edge as the opacity bracket above.
            let clip = clip_of(b, styled);
            if let Some((rect, radius)) = clip {
                out.push(PaintCmd::PushClip { rect, radius });
            }
            for child in b.children() {
                collect_inflow(child, styled, fonts, images, doc, out, floats, positioned);
            }
            if clip.is_some() {
                out.push(PaintCmd::PopClip);
            }
            if cpath.is_some() {
                out.push(PaintCmd::PopClip);
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

/// The (opacity, filter, blend, mask) for a box that needs an offscreen layer —
/// when `opacity < 1.0` OR a non-empty `filter` OR `mix-blend-mode != Normal` OR
/// a `mask`, else `None` (the fast path, byte-identical to no layer). (E2-M5
/// §4.2, E21-M1/M2/M3)
fn layer_effect(
    b: &LayoutBox,
    styled: &StyledTree,
) -> Option<(f32, Vec<FilterFn>, BlendMode, Option<MaskBox>)> {
    let s = b.style(styled)?;
    if s.opacity < 1.0
        || !s.filter.is_empty()
        || s.mix_blend_mode != BlendMode::Normal
        || s.mask.is_some()
    {
        let mask = s.mask.clone().map(|spec| MaskBox {
            spec,
            rect: b.dimensions().border_box(),
            padding_box: b.dimensions().padding_box(),
            content_box: b.dimensions().content_box(),
            radius: s.border_radius,
        });
        Some((s.opacity, s.filter.clone(), s.mix_blend_mode, mask))
    } else {
        None
    }
}

/// The backdrop-filter region for a box (E21-M3): `Some((border_box, filter))`
/// when `backdrop-filter` is non-empty, else `None` (no backdrop snapshot).
fn backdrop_of(b: &LayoutBox, styled: &StyledTree) -> Option<(Rect, Vec<FilterFn>)> {
    let s = b.style(styled)?;
    if s.backdrop_filter.is_empty() {
        None
    } else {
        Some((b.dimensions().border_box(), s.backdrop_filter.clone()))
    }
}

/// The clip region (padding box + inset corner radii) for a box with
/// `overflow: hidden|clip`, else `None` for `visible` (the fast path, E13-M4).
fn clip_of(b: &LayoutBox, styled: &StyledTree) -> Option<(Rect, [f32; 4])> {
    let s = b.style(styled)?;
    if s.overflow == Overflow::Visible {
        return None;
    }
    let d = b.dimensions();
    Some((d.padding_box(), inset_radius(s.border_radius, &d.border)))
}

/// The box's `clip-path` shape + border box, if set (E32-M1).
fn clip_path_of(b: &LayoutBox, styled: &StyledTree) -> Option<(ClipShape, Rect)> {
    let s = b.style(styled)?;
    let shape = s.clip_path.clone()?;
    Some((shape, b.dimensions().border_box()))
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
        BoxKind::Image => emit_image(b, styled, fonts, images, doc, out),
        BoxKind::Svg => emit_svg(b, styled, fonts, doc, out),
        BoxKind::Media => emit_media(b, styled, images, doc, out),
        BoxKind::Canvas => emit_canvas(b, doc, out),
        BoxKind::FormControl => emit_form_control(b, styled, fonts, images, doc, out),
        _ => emit_box(b, styled, images, out),
    }
}

// --- E14-M2: native form-control colors ---
/// The UA box border / unchecked outline color (#767676).
const FC_BORDER: Rgba = Rgba {
    r: 0x76,
    g: 0x76,
    b: 0x76,
    a: 255,
};
/// The control background (white).
const FC_BG: Rgba = Rgba {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 255,
};
/// The check mark / radio dot / dropdown-arrow color (#333333).
const FC_MARK: Rgba = Rgba {
    r: 0x33,
    g: 0x33,
    b: 0x33,
    a: 255,
};

/// Emit a native form control. E14-M1 text controls (input/textarea/button) draw
/// a UA box + clipped text via `emit_text_control`; E14-M2 choice controls draw
/// their own shapes: checkbox/radio at a fixed 13×13, `<select>` as a UA field
/// with the selected option's text + a dropdown arrow.
fn emit_form_control(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    let id = b.style.node();
    let Some(kind) = form_control_kind(doc, id) else {
        emit_box(b, styled, images, out);
        return;
    };
    match kind {
        FormControl::Checkbox { checked } => emit_checkbox(b, checked, out),
        FormControl::Radio { checked } => emit_radio(b, checked, out),
        FormControl::Select => emit_select(b, styled, fonts, images, doc, out),
        FormControl::Color => emit_color(b, doc, out),
        FormControl::Range => emit_range(b, doc, out),
        _ => emit_text_control(b, styled, fonts, images, doc, kind, out),
    }
}

/// Build an `SvgShape` for a single basic geometry with an identity transform
/// (geom already in canvas px) + optional solid fill/stroke (E14-M2).
fn emit_shape(
    geom: SvgGeom,
    fill: Option<Rgba>,
    stroke: Option<Rgba>,
    stroke_width: f32,
    out: &mut Vec<PaintCmd>,
) {
    let bbox = geom_bbox(&geom);
    out.push(PaintCmd::SvgShape {
        geom,
        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        fill: fill.map(SvgPaint::Color),
        fill_rule: SvgFillRule::NonZero,
        stroke: stroke.map(SvgPaint::Color),
        stroke_width,
        stroke_cap: SvgLineCap::Round,
        stroke_join: SvgLineJoin::Round,
        bbox,
    });
}

/// Emit `<input type=checkbox>` (E14-M2): a white box with a #767676 outline;
/// when checked, a #333333 tick polyline inside it.
fn emit_checkbox(b: &LayoutBox, checked: bool, out: &mut Vec<PaintCmd>) {
    let cb = b.dimensions().content;
    // Box, inset 0.5px so the 1px stroke sits inside the 13×13 content rect.
    emit_shape(
        SvgGeom::Rect {
            x: cb.x + 0.5,
            y: cb.y + 0.5,
            w: cb.width - 1.0,
            h: cb.height - 1.0,
            rx: 0.0,
            ry: 0.0,
        },
        Some(FC_BG),
        Some(FC_BORDER),
        1.0,
        out,
    );
    if checked {
        let pts = [
            (cb.x + 0.20 * cb.width, cb.y + 0.55 * cb.height),
            (cb.x + 0.42 * cb.width, cb.y + 0.78 * cb.height),
            (cb.x + 0.80 * cb.width, cb.y + 0.25 * cb.height),
        ];
        emit_shape(
            SvgGeom::Path(crate::svg_path::points_to_ops(&pts, false)),
            None,
            Some(FC_MARK),
            2.0,
            out,
        );
    }
}

/// Emit `<input type=radio>` (E14-M2): a white circle with a #767676 outline;
/// when checked, a filled #333333 centre dot.
fn emit_radio(b: &LayoutBox, checked: bool, out: &mut Vec<PaintCmd>) {
    let cb = b.dimensions().content;
    let cx = cb.x + cb.width / 2.0;
    let cy = cb.y + cb.height / 2.0;
    let min = cb.width.min(cb.height);
    let r = min / 2.0 - 0.5;
    emit_shape(
        SvgGeom::Ellipse {
            cx,
            cy,
            rx: r,
            ry: r,
        },
        Some(FC_BG),
        Some(FC_BORDER),
        1.0,
        out,
    );
    if checked {
        emit_shape(
            SvgGeom::Ellipse {
                cx,
                cy,
                rx: 0.25 * min,
                ry: 0.25 * min,
            },
            Some(FC_MARK),
            None,
            0.0,
            out,
        );
    }
}

/// Emit `<input type=color>` (E14-M3): a UA-bordered field whose interior is a
/// solid swatch filled with the parsed `value` colour (defaulting to black).
fn emit_color(b: &LayoutBox, doc: &Document, out: &mut Vec<PaintCmd>) {
    let cb = b.dimensions().content;
    let id = b.style.node();
    let swatch = doc
        .get_attribute(id, "value")
        .and_then(starfish_css::parse_color)
        .unwrap_or(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        });
    // Outer field: bordered like the other UA controls.
    emit_shape(
        SvgGeom::Rect {
            x: cb.x + 0.5,
            y: cb.y + 0.5,
            w: cb.width - 1.0,
            h: cb.height - 1.0,
            rx: 0.0,
            ry: 0.0,
        },
        Some(FC_BG),
        Some(FC_BORDER),
        1.0,
        out,
    );
    // Inner swatch: the value colour, inset 2px, no stroke.
    emit_shape(
        SvgGeom::Rect {
            x: cb.x + 2.0,
            y: cb.y + 2.0,
            w: (cb.width - 4.0).max(0.0),
            h: (cb.height - 4.0).max(0.0),
            rx: 0.0,
            ry: 0.0,
        },
        Some(swatch),
        None,
        0.0,
        out,
    );
}

/// Emit `<input type=range>` (E14-M3): a thin #767676 track with a white #767676-
/// outlined circular thumb positioned at the value's fraction between min/max.
fn emit_range(b: &LayoutBox, doc: &Document, out: &mut Vec<PaintCmd>) {
    let cb = b.dimensions().content;
    let id = b.style.node();
    let (value, min, max) = range_values(doc, id);
    let frac = range_fraction(value, min, max);
    let cy = cb.y + cb.height / 2.0;
    let r = cb.height / 2.0 - 1.0;
    let tx0 = cb.x + r;
    let tx1 = cb.x + cb.width - r;
    // Track: a thin rectangle spanning the thumb's travel.
    emit_shape(
        SvgGeom::Rect {
            x: tx0,
            y: cy - 1.5,
            w: (tx1 - tx0).max(0.0),
            h: 3.0,
            rx: 0.0,
            ry: 0.0,
        },
        Some(FC_BORDER),
        None,
        0.0,
        out,
    );
    // Thumb: a circle centred at the value fraction along the track.
    let cx = tx0 + frac * (tx1 - tx0);
    emit_shape(
        SvgGeom::Ellipse {
            cx,
            cy,
            rx: r,
            ry: r,
        },
        Some(FC_BG),
        Some(FC_BORDER),
        1.0,
        out,
    );
}

/// Emit `<select>` (E14-M2): the UA field box (`emit_box`), the selected option's
/// text (left, vertically centered, clipped to the text slot), then a #333333
/// down-triangle in the arrow slot on the right.
fn emit_select(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    emit_box(b, styled, images, out);

    let initial = ComputedStyle::initial();
    let style = b.style(styled).unwrap_or(&initial);
    let id = b.style.node();
    let cb = b.dimensions().content;
    let arrow_w = style.font_size;
    let text_w = (cb.width - arrow_w).max(0.0);

    let text = selected_option_text(doc, id);
    if !text.is_empty() {
        let q = FontQuery {
            family: &style.font_family,
            style: style.font_style,
            weight: style.font_weight,
            size: style.font_size,
            letter_spacing: style.letter_spacing,
            word_spacing: style.word_spacing,
        };
        let lm = fonts.line_metrics(&q);
        let ty = cb.y + (cb.height - (lm.ascent + lm.descent)) / 2.0;
        let clip = Rect {
            x: cb.x,
            y: cb.y,
            width: text_w,
            height: cb.height,
        };
        out.push(PaintCmd::PushClip {
            rect: clip,
            radius: [0.0; 4],
        });
        out.push(PaintCmd::GlyphRun {
            origin: (cb.x, ty),
            text,
            font_size: style.font_size,
            weight: style.font_weight,
            style: style.font_style,
            family: style.font_family.clone(),
            color: style.color,
            ascent: lm.ascent,
            letter_spacing: style.letter_spacing,
            word_spacing: style.word_spacing,
        });
        out.push(PaintCmd::PopClip);
    }

    // Down-triangle in the arrow slot.
    let acx = cb.x + text_w + arrow_w / 2.0;
    let acy = cb.y + cb.height / 2.0;
    let pts = [
        (acx - 0.30 * arrow_w, acy - 0.18 * arrow_w),
        (acx + 0.30 * arrow_w, acy - 0.18 * arrow_w),
        (acx, acy + 0.18 * arrow_w),
    ];
    emit_shape(
        SvgGeom::Path(crate::svg_path::points_to_ops(&pts, true)),
        Some(FC_MARK),
        None,
        0.0,
        out,
    );
}

/// Emit a native text form control (E14-M1): its UA bg+border (via `emit_box`),
/// then the displayed text clipped to the content box. `<input>` text masks to
/// the left + vertically centered; `<button>` centers; `<textarea>` is top-left.
fn emit_text_control(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    kind: FormControl,
    out: &mut Vec<PaintCmd>,
) {
    // Background + border come for free from the box's ComputedStyle.
    emit_box(b, styled, images, out);

    let initial = ComputedStyle::initial();
    let style = b.style(styled).unwrap_or(&initial);
    let id = b.style.node();

    let grey = Rgba {
        r: 0x75,
        g: 0x75,
        b: 0x75,
        a: 255,
    };
    let (text, color) = match kind {
        FormControl::TextInput { password } => {
            let (t, is_placeholder) = input_display(doc, id, password);
            (t, if is_placeholder { grey } else { style.color })
        }
        FormControl::TextArea => (textarea_value(doc, id), style.color),
        FormControl::Button => (control_label(doc, id), style.color),
        // Choice controls are dispatched to their own emitters upstream.
        _ => unreachable!("non-text control in emit_text_control"),
    };
    if text.is_empty() {
        return;
    }

    let q = FontQuery {
        family: &style.font_family,
        style: style.font_style,
        weight: style.font_weight,
        size: style.font_size,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
    };
    let lm = fonts.line_metrics(&q);
    let cb = b.dimensions().content;
    let measured = fonts.advance_width(&text, &q);

    let (tx, ty) = match kind {
        FormControl::Button => (
            cb.x + (cb.width - measured) / 2.0,
            cb.y + (cb.height - (lm.ascent + lm.descent)) / 2.0,
        ),
        FormControl::TextInput { .. } => {
            (cb.x, cb.y + (cb.height - (lm.ascent + lm.descent)) / 2.0)
        }
        FormControl::TextArea => (cb.x, cb.y),
        _ => unreachable!("non-text control in emit_text_control"),
    };

    out.push(PaintCmd::PushClip {
        rect: cb,
        radius: [0.0; 4],
    });
    out.push(PaintCmd::GlyphRun {
        origin: (tx, ty),
        text,
        font_size: style.font_size,
        weight: style.font_weight,
        style: style.font_style,
        family: style.font_family.clone(),
        color,
        ascent: lm.ascent,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
    });
    out.push(PaintCmd::PopClip);
}

/// Emit shadow + background + border for an element box. Routes between the
/// sharp fast path (no rounding → existing 4-edge borders) and the rounded
/// uniform-border approximation (E2-M5 §5.2).
fn emit_box(b: &LayoutBox, styled: &StyledTree, images: &ImageStore, out: &mut Vec<PaintCmd>) {
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
            emit_background_at(pb, irad, style, images, out);
        } else {
            emit_background_at(bb, radius, style, images, out);
        }
    } else {
        emit_background_at(bb, radius, style, images, out);
        emit_borders(b, styled, out);
    }

    // column rules (E18-M2): a vertical stroke centered in each inter-column gap.
    // Skipped entirely for non-multicol boxes and invisible rules (byte-identical).
    if (style.column_count.is_some() || style.column_width.is_some())
        && style.column_rule_width > 0.0
        && style.column_rule_style != BorderStyle::None
        && style.column_rule_color.a != 0
    {
        emit_column_rules(b, style, out);
    }

    // outline (E16-M3): drawn AFTER bg + border, OUTSIDE the border box. Skipped
    // for the common no-outline case (byte-identical).
    let o = style.outline;
    if o.style != BorderStyle::None && o.width > 0.0 && o.color.a != 0 {
        emit_outline(bb, o, out);
    }
}

/// Emit the column rules of a multi-column box (E18-M2): a vertical
/// `StrokeLine` centered in each of the `used_count - 1` inter-column gaps,
/// spanning the content-box height. Mirrors `multicol::resolve_columns` (kept
/// local to avoid a layout→paint dependency).
fn emit_column_rules(b: &LayoutBox, style: &ComputedStyle, out: &mut Vec<PaintCmd>) {
    let content = b.dimensions().content;
    // `normal` column-gap → 1em (font-size).
    let gap = match &style.column_gap {
        Length::Px(p) if *p == 0.0 => style.font_size,
        Length::Px(p) => *p,
        Length::Percent(p) => p / 100.0 * content.width,
        Length::Calc { px, percent } => px + percent / 100.0 * content.width,
        // Math-function tree (E24-M1): resolved against the content width.
        Length::Math(m) => m.resolve(content.width),
        Length::Auto => 0.0,
    };
    let col_width_px = style.column_width.as_ref().and_then(|l| match l {
        Length::Px(p) => Some(*p),
        _ => None,
    });
    let (used_count, col_w) = resolve_columns(content.width, gap, style.column_count, col_width_px);
    for i in 1..used_count {
        let gap_center_x = content.x + i as f32 * col_w + (i as f32 - 0.5) * gap;
        out.push(PaintCmd::StrokeLine {
            from: (gap_center_x, content.y),
            to: (gap_center_x, content.y + content.height),
            width: style.column_rule_width,
            color: style.column_rule_color,
            style: style.column_rule_style,
        });
    }
}

/// Local copy of the multi-column count/width resolution (E18-M2). Identical
/// formula to `layout::multicol::resolve_columns`; duplicated to keep paint
/// independent of the layout crate.
fn resolve_columns(u: f32, gap: f32, count: Option<u32>, width: Option<f32>) -> (u32, f32) {
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
        (None, None) => 1,
    };
    let col_w = ((u - (used - 1) as f32 * gap) / used as f32).max(0.0);
    (used, col_w)
}

/// Emit an outline (E16-M3): a frame whose inner edge is the border box inflated
/// by `o.offset`, with the stroke growing OUTWARD by `o.width`. `Solid` → four
/// `FillRect`s forming the frame; `dashed`/`dotted`/`double` → four `StrokeLine`s
/// centered on the frame's edges (reusing the border-stroke geometry). Sharp
/// corners only (border-radius ignored, MVP).
fn emit_outline(border_box: Rect, o: Outline, out: &mut Vec<PaintCmd>) {
    // Inner edge of the outline = border box inflated by the offset.
    let inner = Rect {
        x: border_box.x - o.offset,
        y: border_box.y - o.offset,
        width: border_box.width + 2.0 * o.offset,
        height: border_box.height + 2.0 * o.offset,
    };
    let w = o.width;
    // Outer edge = inner inflated by the width (stroke sits outside `inner`).
    let outer = Rect {
        x: inner.x - w,
        y: inner.y - w,
        width: inner.width + 2.0 * w,
        height: inner.height + 2.0 * w,
    };
    if outer.width <= 0.0 || outer.height <= 0.0 {
        return;
    }
    match o.style {
        BorderStyle::Solid | BorderStyle::None => {
            // Four fill rects forming the frame between `outer` and `inner`.
            let (l, t, r, bot) = (
                outer.x,
                outer.y,
                outer.x + outer.width,
                outer.y + outer.height,
            );
            // top + bottom span the full outer width; left + right fill the gap.
            out.push(fill(
                Rect {
                    x: l,
                    y: t,
                    width: outer.width,
                    height: w,
                },
                o.color,
            ));
            out.push(fill(
                Rect {
                    x: l,
                    y: bot - w,
                    width: outer.width,
                    height: w,
                },
                o.color,
            ));
            let mid_h = (outer.height - 2.0 * w).max(0.0);
            out.push(fill(
                Rect {
                    x: l,
                    y: t + w,
                    width: w,
                    height: mid_h,
                },
                o.color,
            ));
            out.push(fill(
                Rect {
                    x: r - w,
                    y: t + w,
                    width: w,
                    height: mid_h,
                },
                o.color,
            ));
        }
        bs @ (BorderStyle::Dashed | BorderStyle::Dotted | BorderStyle::Double) => {
            // Center lines of the stroke ring (inner edge + w/2 outward), spanning
            // the outer extent on each axis — same pattern as emit_borders_stroke.
            let (l, t, r, bot) = (
                outer.x,
                outer.y,
                outer.x + outer.width,
                outer.y + outer.height,
            );
            let mut edge = |from: (f32, f32), to: (f32, f32)| {
                out.push(PaintCmd::StrokeLine {
                    from,
                    to,
                    width: w,
                    color: o.color,
                    style: bs,
                });
            };
            edge((l, t + w / 2.0), (r, t + w / 2.0)); // top
            edge((l, bot - w / 2.0), (r, bot - w / 2.0)); // bottom
            edge((l + w / 2.0, t), (l + w / 2.0, bot)); // left
            edge((r - w / 2.0, t), (r - w / 2.0, bot)); // right
        }
    }
}

/// Emit the background for `rect` with `radius` (E16-M2): the solid color at the
/// bottom, then the image layers back-to-front (last layer = bottom). For a page
/// with no image layers and a transparent/opaque color this is byte-identical to
/// the pre-M2 painter: an opaque color → one `FillRect`, a transparent color →
/// nothing. A single gradient layer + transparent color → one `GradientRect`,
/// also byte-identical (size/position are ignored on gradient layers, §M2).
fn emit_background_at(
    rect: Rect,
    radius: [f32; 4],
    style: &ComputedStyle,
    images: &ImageStore,
    out: &mut Vec<PaintCmd>,
) {
    // E21-M2: `background-blend-mode` with at least one non-Normal mode blends the
    // layers within an isolated sub-layer. Empty / all-Normal → the fast path
    // (no group, every cmd blend:Normal → byte-identical to pre-M2).
    let blends = &style.background_blend_mode;
    let blended = blends.iter().any(|m| *m != BlendMode::Normal);
    if blended {
        out.push(PaintCmd::PushBgGroup);
    }

    // 1. The solid color (bottom of the stack; always source-over).
    if style.background_color.a != 0 {
        out.push(PaintCmd::FillRect {
            rect,
            color: style.background_color,
            radius,
            blend: BlendMode::Normal,
        });
    }
    // 2. Image layers, back-to-front (source index 0 paints last / on top). The
    // blend for source index `i` is `blends[i % len]` (or Normal outside a group).
    let n = style.background_layers.len();
    for (rev_i, layer) in style.background_layers.iter().rev().enumerate() {
        let i = n - 1 - rev_i; // source index
        let blend = if blended {
            pick_blend(blends, i)
        } else {
            BlendMode::Normal
        };
        match &layer.image {
            BgImage::Gradient(g) => {
                out.push(PaintCmd::GradientRect {
                    rect,
                    gradient: g.clone(),
                    radius,
                    blend,
                });
            }
            BgImage::Radial(g) => {
                out.push(PaintCmd::RadialRect {
                    rect,
                    gradient: g.clone(),
                    radius,
                    blend,
                });
            }
            BgImage::Conic(g) => {
                out.push(PaintCmd::ConicRect {
                    rect,
                    gradient: g.clone(),
                    radius,
                    blend,
                });
            }
            BgImage::Url(src) => emit_bg_image(rect, radius, src, layer, blend, images, out),
        }
    }

    if blended {
        out.push(PaintCmd::PopBgGroup);
    }
}

/// The blend mode for background layer `i` (E21-M2): `blends[i % len]`, or
/// `Normal` for an empty list.
fn pick_blend(blends: &[BlendMode], i: usize) -> BlendMode {
    if blends.is_empty() {
        BlendMode::Normal
    } else {
        blends[i % blends.len()]
    }
}

/// Painter cap: never emit more than this many tiles per axis for a repeating
/// background, so a 1px image in a huge box can't blow up the display list.
const MAX_BG_TILES_PER_AXIS: usize = 4096;

/// Emit one `url(...)` background layer (E16-M2): resolve the tile size from
/// `background-size`, the origin from `background-position`, then blit the tile
/// once (no-repeat) or across the box (repeat), clipped to `rect`. A missing /
/// zero-size image emits nothing.
#[allow(clippy::too_many_arguments)]
fn emit_bg_image(
    rect: Rect,
    radius: [f32; 4],
    src: &str,
    layer: &BackgroundLayer,
    blend: BlendMode,
    images: &ImageStore,
    out: &mut Vec<PaintCmd>,
) {
    let Some(img) = images.peek(src) else { return };
    let (iw, ih) = (img.width as f32, img.height as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let (tw, th) = bg_tile_size(layer.size, iw, ih, rect.width, rect.height);
    if tw <= 0.0 || th <= 0.0 {
        return;
    }
    let ox = rect.x + align(layer.position.0, rect.width - tw);
    let oy = rect.y + align(layer.position.1, rect.height - th);
    let (rep_x, rep_y) = match layer.repeat {
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
    out.push(PaintCmd::PushClip { rect, radius });
    let ys = tile_starts(oy, th, rect.y, rect.height, rep_y);
    let xs = tile_starts(ox, tw, rect.x, rect.width, rep_x);
    for ty in ys {
        for &tx in &xs {
            out.push(PaintCmd::ImageBlit {
                dest: Rect {
                    x: tx,
                    y: ty,
                    width: tw,
                    height: th,
                },
                src: src.to_string(),
                src_crop,
                smooth: false,
                blend,
            });
        }
    }
    out.push(PaintCmd::PopClip);
}

/// Resolve `background-size` to a tile size in px. `Auto` = intrinsic; `Cover` /
/// `Contain` reuse the object-fit max/min scale; `Explicit` resolves each axis
/// (an `auto` axis derives from the other by the intrinsic aspect, both auto =
/// intrinsic).
pub(crate) fn bg_tile_size(size: BgSize, iw: f32, ih: f32, bw: f32, bh: f32) -> (f32, f32) {
    match size {
        BgSize::Auto => (iw, ih),
        BgSize::Cover => {
            let s = (bw / iw).max(bh / ih);
            (iw * s, ih * s)
        }
        BgSize::Contain => {
            let s = (bw / iw).min(bh / ih);
            (iw * s, ih * s)
        }
        BgSize::Explicit(ax, ay) => {
            let rx = resolve_axis(ax, bw);
            let ry = resolve_axis(ay, bh);
            match (rx, ry) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w / iw * ih),
                (None, Some(h)) => (h / ih * iw, h),
                (None, None) => (iw, ih),
            }
        }
    }
}

/// One explicit `background-size` axis → px, or `None` for `auto`.
fn resolve_axis(a: BgSizeAxis, basis: f32) -> Option<f32> {
    match a {
        BgSizeAxis::Auto => None,
        BgSizeAxis::Px(v) => Some(v),
        BgSizeAxis::Percent(p) => Some(p / 100.0 * basis),
    }
}

/// Tile origin positions on one axis. `!repeat` → just `[origin]`. Otherwise step
/// back from `origin` to the first tile start ≤ `box_start`, then forward to the
/// box end, capped at `MAX_BG_TILES_PER_AXIS` (always ≥ 1 tile).
pub(crate) fn tile_starts(
    origin: f32,
    tile: f32,
    box_start: f32,
    box_len: f32,
    repeat: bool,
) -> Vec<f32> {
    if !repeat || tile <= 0.0 {
        return vec![origin];
    }
    // First tile start ≤ box_start.
    let mut first = origin;
    if first > box_start {
        let steps = ((first - box_start) / tile).ceil();
        first -= steps * tile;
    }
    let box_end = box_start + box_len;
    let mut out = Vec::new();
    let mut t = first;
    while t < box_end && out.len() < MAX_BG_TILES_PER_AXIS {
        out.push(t);
        t += tile;
    }
    if out.is_empty() {
        out.push(origin);
    }
    out
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
/// image (no attrs) paints nothing. `object-fit`/`object-position` map the
/// decoded pixels into the content box; `image-rendering` selects the sampler
/// (E15-M1).
fn emit_image(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    let Some(src) = b.text() else { return };
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return; // collapsed broken image → nothing
    }
    // E15-M3: an `<img src=*.svg>` renders via the inline-SVG painter into the
    // img content box (vector, crisp). Checked before the raster peek (an SVG
    // ref never has a raster cache entry).
    if let Some(parsed) = images.peek_svg(src) {
        emit_svg_into(&parsed.doc, styled, fonts, parsed.svg_id, dest, out);
        return;
    }
    if let Some(img) = images.peek(src) {
        emit_image_blit(b, styled, src, img, dest, out);
        return;
    }
    // Broken image with a non-zero box → 1px grey placeholder border.
    let grey = Rgba {
        r: 0x80,
        g: 0x80,
        b: 0x80,
        a: 255,
    };
    let edges = [
        Rect {
            x: dest.x,
            y: dest.y,
            width: dest.width,
            height: 1.0,
        },
        Rect {
            x: dest.x,
            y: dest.y + dest.height - 1.0,
            width: dest.width,
            height: 1.0,
        },
        Rect {
            x: dest.x,
            y: dest.y,
            width: 1.0,
            height: dest.height,
        },
        Rect {
            x: dest.x + dest.width - 1.0,
            y: dest.y,
            width: 1.0,
            height: dest.height,
        },
    ];
    for rect in edges {
        out.push(fill(rect, grey));
    }
    // E15-M3: a broken image with non-empty `alt` text shows that text, clipped
    // to the box (the no-alt path above is unchanged — no glyph run emitted).
    if let Some(alt) = doc
        .get_attribute(b.style.node(), "alt")
        .filter(|a| !a.is_empty())
    {
        let initial = ComputedStyle::initial();
        let s = b.style(styled).unwrap_or(&initial);
        let q = FontQuery {
            family: &s.font_family,
            style: s.font_style,
            weight: s.font_weight,
            size: s.font_size,
            letter_spacing: s.letter_spacing,
            word_spacing: s.word_spacing,
        };
        let lm = fonts.line_metrics(&q);
        out.push(PaintCmd::PushClip {
            rect: dest,
            radius: [0.0; 4],
        });
        out.push(PaintCmd::GlyphRun {
            origin: (dest.x + 2.0, dest.y + 2.0),
            text: alt.to_string(),
            font_size: s.font_size,
            weight: s.font_weight,
            style: s.font_style,
            family: s.font_family.clone(),
            color: s.color,
            ascent: lm.ascent,
            letter_spacing: s.letter_spacing,
            word_spacing: s.word_spacing,
        });
        out.push(PaintCmd::PopClip);
    }
}

/// Emit the `ImageBlit` for a decoded raster image mapped into `dest` per
/// `object-fit`/`object-position`/`image-rendering` (E15-M1). Shared by `<img>`
/// and a `<video poster>` (E15-M3). The default `object-fit: fill` +
/// `image-rendering: auto` stays byte-identical to the nearest-neighbour stretch.
fn emit_image_blit(
    b: &LayoutBox,
    styled: &StyledTree,
    src: &str,
    img: &crate::image_store::DecodedImage,
    dest: Rect,
    out: &mut Vec<PaintCmd>,
) {
    let initial = ComputedStyle::initial();
    let s = b.style(styled).unwrap_or(&initial);
    let (iw, ih) = (img.width as f32, img.height as f32);
    // FAST PATH: object-fit:fill → full crop into the full box. This keeps
    // the blit byte-identical to the original nearest-neighbour stretch.
    let (drect, src_crop) = if s.object_fit == ObjectFit::Fill {
        (
            dest,
            Rect {
                x: 0.0,
                y: 0.0,
                width: iw,
                height: ih,
            },
        )
    } else {
        fit_image(dest, iw, ih, s.object_fit, s.object_position)
    };
    // Auto/Pixelated/CrispEdges → nearest (false); only Smooth → bilinear.
    // Keeping `auto` = nearest preserves byte-identity of every existing page.
    let smooth = matches!(s.image_rendering, ImageRendering::Smooth);
    out.push(PaintCmd::ImageBlit {
        dest: drect,
        src: src.to_string(),
        src_crop,
        smooth,
        blend: BlendMode::Normal,
    });
}

/// Emit a `<video>`/`<audio>` (E15-M3): a `<video poster>` blits the poster like
/// an `<img>`; a posterless `<video>`/`<audio>` paints a placeholder box (video:
/// a dark box + a white play triangle; audio: a dark box).
fn emit_media(
    b: &LayoutBox,
    styled: &StyledTree,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return;
    }
    // `text` carries the `<video poster>` url (audio → None). A decoded poster
    // blits like an `<img>`; otherwise the placeholder box.
    if let Some(poster) = b.text() {
        if let Some(img) = images.peek(poster) {
            emit_image_blit(b, styled, poster, img, dest, out);
            return;
        }
    }
    let is_video = doc.tag_name(b.style.node()) == Some("video");
    emit_media_placeholder(dest, is_video, out);
}

/// Emit a `<canvas>` (E20-M1): replay the recorded 2D ops into a backing pixmap
/// and composite it into the content box. No recorded ops (or an undrawn canvas
/// — `getContext` never called) → nothing emitted (a transparent box).
fn emit_canvas(b: &LayoutBox, doc: &Document, out: &mut Vec<PaintCmd>) {
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return;
    }
    let id = b.style.node();
    let ops = match doc.canvas_ops(id) {
        Some(ops) if !ops.is_empty() => ops,
        _ => return,
    };
    // The backing bitmap is sized by the canvas width/height attrs (default
    // 300×150), independent of the CSS-laid-out box size (the spec's intrinsic
    // size); raster scales it into `dest`.
    let bw = attr_opt_f(doc, id, "width").unwrap_or(300.0);
    let bh = attr_opt_f(doc, id, "height").unwrap_or(150.0);
    if bw <= 0.0 || bh <= 0.0 {
        return;
    }
    // E20-M3: rewrite any `DrawImage` whose source is another `<canvas>` into a
    // self-contained `CanvasSnapshot` (its backing size + op stream) so the
    // rasterizer never has to chase NodeIds. Bounded recursion depth so a cycle
    // (canvas A draws canvas B draws canvas A) can't loop forever.
    let ops = flatten_canvas_ops(ops, doc, 4);
    out.push(PaintCmd::Canvas {
        rect: dest,
        backing: (bw, bh),
        ops,
    });
}

/// Recursively rewrite `DrawImage{ source: Canvas(id) }` ops into
/// `DrawImage{ source: CanvasSnapshot { backing, ops } }`. `depth` caps the
/// recursion; at depth 0 a `Canvas(id)` source is left as-is (the rasterizer
/// no-ops on a bare `Canvas` source) so cycles terminate. Returns an owned op
/// vec (a plain `to_vec()` clone when no `DrawImage`/canvas-source ops exist —
/// byte-identical to M1/M2).
fn flatten_canvas_ops(ops: &[CanvasOp], doc: &Document, depth: u32) -> Vec<CanvasOp> {
    ops.iter()
        .map(|op| match op {
            CanvasOp::DrawImage {
                source: CanvasImageSrc::Canvas(src_id),
                src_rect,
                dst,
            } if depth > 0 => {
                let bw = attr_opt_f(doc, *src_id, "width").unwrap_or(300.0);
                let bh = attr_opt_f(doc, *src_id, "height").unwrap_or(150.0);
                let inner = doc
                    .canvas_ops(*src_id)
                    .map(|o| flatten_canvas_ops(o, doc, depth - 1))
                    .unwrap_or_default();
                CanvasOp::DrawImage {
                    source: CanvasImageSrc::CanvasSnapshot {
                        backing: (bw, bh),
                        ops: inner,
                    },
                    src_rect: *src_rect,
                    dst: *dst,
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// Paint a media placeholder (E15-M3): a dark fill over `dest`, plus — for video
/// — a centered white play triangle sized to ~0.2× the smaller box dimension.
fn emit_media_placeholder(dest: Rect, is_video: bool, out: &mut Vec<PaintCmd>) {
    let dark = Rgba {
        r: 0x33,
        g: 0x33,
        b: 0x33,
        a: 255,
    };
    out.push(fill(dest, dark));
    if is_video {
        let white = Rgba {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let cx = dest.x + dest.width / 2.0;
        let cy = dest.y + dest.height / 2.0;
        let r = 0.2 * dest.width.min(dest.height);
        // A right-pointing triangle centered on (cx, cy), roughly inscribed in a
        // circle of radius `r`.
        let pts = [(cx - 0.5 * r, cy - r), (cx - 0.5 * r, cy + r), (cx + r, cy)];
        emit_shape(
            SvgGeom::Path(crate::svg_path::points_to_ops(&pts, true)),
            Some(white),
            None,
            0.0,
            out,
        );
    }
}

/// Resolve an `object-position` axis component to a px OFFSET within `free` px
/// of free space (E15-M1). `Percent(p)` → `p/100 * free` (so 50% centers, 0%
/// hugs the start, 100% hugs the end). `Px(v)` → `v` directly (may be negative
/// or exceed `free`; the source/dest math then clips). `free` can be negative
/// (cover/none, where the image is larger than the box) — the same formula
/// gives the correct negative offset for percent.
pub(crate) fn align(lp: LengthPct, free: f32) -> f32 {
    match lp {
        LengthPct::Percent(p) => p / 100.0 * free,
        LengthPct::Px(v) => v,
    }
}

/// Map a decoded `iw`×`ih` image into the content box `cb` per `object-fit` +
/// `object-position`, returning `(dest_rect, src_crop)` where `src_crop` is in
/// SOURCE pixels (E15-M1). `Fill` is handled on the fast path by the caller but
/// is implemented here too for completeness.
fn fit_image(
    cb: Rect,
    iw: f32,
    ih: f32,
    fit: ObjectFit,
    pos: (LengthPct, LengthPct),
) -> (Rect, Rect) {
    let full = Rect {
        x: 0.0,
        y: 0.0,
        width: iw,
        height: ih,
    };
    match fit {
        ObjectFit::Fill => (cb, full),
        ObjectFit::Contain => {
            let s = (cb.width / iw).min(cb.height / ih);
            let (dw, dh) = (iw * s, ih * s);
            let dx = cb.x + align(pos.0, cb.width - dw);
            let dy = cb.y + align(pos.1, cb.height - dh);
            (
                Rect {
                    x: dx,
                    y: dy,
                    width: dw,
                    height: dh,
                },
                full,
            )
        }
        ObjectFit::Cover => {
            let s = (cb.width / iw).max(cb.height / ih);
            let sw = (cb.width / s).min(iw);
            let sh = (cb.height / s).min(ih);
            let sx = align(pos.0, iw - sw);
            let sy = align(pos.1, ih - sh);
            (
                cb,
                Rect {
                    x: sx,
                    y: sy,
                    width: sw,
                    height: sh,
                },
            )
        }
        ObjectFit::None => fit_none(cb, iw, ih, pos),
        ObjectFit::ScaleDown => {
            // The smaller of `none` and `contain` (CSS: pick whichever yields a
            // smaller rendered size). When the image fits, `none` (1:1) is
            // smaller; otherwise `contain` shrinks it.
            if iw <= cb.width && ih <= cb.height {
                fit_none(cb, iw, ih, pos)
            } else {
                fit_image(cb, iw, ih, ObjectFit::Contain, pos)
            }
        }
    }
}

/// `object-fit: none` (E15-M1): the image at its intrinsic size, positioned by
/// `object-position`, clipped to the box on each axis. Returns `(dest, src_crop)`
/// at a 1:1 scale (a pixel-accurate sub-rect of the source mapped to the matching
/// dest sub-rect). A fully off-box image yields a zero-area dest.
fn fit_none(cb: Rect, iw: f32, ih: f32, pos: (LengthPct, LengthPct)) -> (Rect, Rect) {
    // Top-left of the intrinsic-size image inside the box.
    let off_x = align(pos.0, cb.width - iw);
    let off_y = align(pos.1, cb.height - ih);
    let (dx, sx, dw) = intersect_axis(cb.x, cb.width, off_x, iw);
    let (dy, sy, dh) = intersect_axis(cb.y, cb.height, off_y, ih);
    (
        Rect {
            x: dx,
            y: dy,
            width: dw,
            height: dh,
        },
        Rect {
            x: sx,
            y: sy,
            width: dw,
            height: dh,
        },
    )
}

/// Intersect a 1:1-placed image span with the box span on one axis. The image's
/// box-relative start is `off` and its length is `len`; the box starts at
/// `box_start` with length `box_len`. Returns `(dest_start, src_start, span)`:
/// the device-space dest position, the source-pixel offset into the image, and
/// the visible length (0 if disjoint).
fn intersect_axis(box_start: f32, box_len: f32, off: f32, len: f32) -> (f32, f32, f32) {
    // Visible range in box-relative coords: [max(0,off), min(box_len, off+len)).
    let vis_start = off.max(0.0);
    let vis_end = (off + len).min(box_len);
    let span = (vis_end - vis_start).max(0.0);
    let dest_start = box_start + vis_start;
    // Source offset: how far into the image the visible part begins.
    let src_start = (vis_start - off).max(0.0);
    (dest_start, src_start, span)
}

// --- E9-M1: inline SVG shape flattening ---

/// Flatten an `<svg>` box's DOM subtree into self-contained `SvgShape`/text
/// commands (E9-M1 §5.3, extended E9-M3). Computes the viewBox→box transform,
/// collects the gradient registry once, then recursively walks the svg DOM
/// threading the accumulated transform + inherited paint context (§1.1).
fn emit_svg(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    emit_svg_into(
        doc,
        styled,
        fonts,
        b.style.node(),
        b.dimensions().content,
        out,
    );
}

/// Flatten the `<svg>` element `svg_id` of `doc` into `out`, mapping its viewBox
/// onto the device-space `dest` rect (E9-M1 §5.3). Shared by inline `<svg>`
/// (`emit_svg`) and `<img src=*.svg>` (E15-M3, where `doc`/`svg_id` come from a
/// separately-parsed SVG file and `dest` is the `<img>` content box). `styled`
/// is used only for `currentColor`; a foreign SVG `NodeId` simply misses → black.
fn emit_svg_into(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    svg_id: NodeId,
    dest: Rect,
    out: &mut Vec<PaintCmd>,
) {
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return;
    }
    let vb = parse_view_box(doc.get_attribute(svg_id, "viewBox"));
    let root_t = svg_transform(dest, vb);
    let grads = collect_gradients(doc, svg_id);
    let ctx = SvgCtx::root();
    for child in doc.children(svg_id) {
        walk_svg(doc, styled, fonts, child, root_t, &ctx, &grads, out);
    }
}

/// Inherited presentation context threaded down the svg walk (E9-M3 §2.1). Only
/// the cheap, common SVG-inherited paints; everything else is read per-element.
#[derive(Clone)]
struct SvgCtx {
    fill: Option<String>,
    stroke: Option<String>,
    stroke_width: Option<String>,
}

impl SvgCtx {
    fn root() -> Self {
        SvgCtx {
            fill: None,
            stroke: None,
            stroke_width: None,
        }
    }

    /// A child context where each paint is this element's own attr/inline-style
    /// value if present, else the parent's (so `<g fill=red>` cascades, §2.2).
    fn inherit(&self, doc: &Document, id: NodeId) -> SvgCtx {
        let style = doc.get_attribute(id, "style");
        let own = |name: &str| -> Option<String> {
            svg_style_prop(style, name).or_else(|| doc.get_attribute(id, name).map(str::to_string))
        };
        SvgCtx {
            fill: own("fill").or_else(|| self.fill.clone()),
            stroke: own("stroke").or_else(|| self.stroke.clone()),
            stroke_width: own("stroke-width").or_else(|| self.stroke_width.clone()),
        }
    }
}

/// Recursively walk one svg DOM node, painting shapes/text with the effective
/// transform (E9-M3 §2.2). `<g>`/`<svg>`/`<a>` recurse with the composed
/// transform + inherited context; `<defs>`/gradients/metadata paint nothing.
#[allow(clippy::too_many_arguments)]
fn walk_svg(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    id: NodeId,
    parent_t: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    out: &mut Vec<PaintCmd>,
) {
    let Some(tag) = doc.tag_name(id) else { return }; // skip text/comment nodes
    let eff = effective_transform(parent_t, doc.get_attribute(id, "transform"));
    match tag {
        "g" | "svg" | "a" => {
            let child_ctx = ctx.inherit(doc, id);
            for c in doc.children(id) {
                walk_svg(doc, styled, fonts, c, eff, &child_ctx, grads, out);
            }
        }
        "defs" | "linearGradient" | "radialGradient" | "stop" | "title" | "desc" | "metadata" => {}
        "text" => emit_svg_text(doc, styled, fonts, id, eff, ctx, grads, out),
        _ => {
            if let Some(cmd) = build_shape(doc, styled, id, eff, ctx, grads) {
                out.push(cmd);
            }
        }
    }
}

/// The effective matrix `parent · parse_transform_attr(attr)` (E9-M3 §1.3).
fn effective_transform(parent: [f32; 6], attr: Option<&str>) -> [f32; 6] {
    let p = to_transform(parent);
    let m = match attr {
        Some(s) => p.pre_concat(to_transform(parse_transform_attr(s))),
        None => p,
    };
    [m.sx, m.ky, m.kx, m.sy, m.tx, m.ty]
}

/// `[a,b,c,d,e,f]` → tiny-skia `Transform`.
fn to_transform(m: [f32; 6]) -> Transform {
    Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])
}

/// Parse an SVG `transform` attribute (a function list) into a 3×2 matrix
/// a,b,c,d,e,f (E9-M3 §1.2). LENIENT: a malformed function is skipped; an
/// empty/absent string → identity. List applied left-to-right (`f1·f2·…`).
fn parse_transform_attr(s: &str) -> [f32; 6] {
    let mut acc = Transform::identity();
    for (name, args) in transform_fns(s) {
        if let Some(t) = transform_fn_matrix(&name, &args) {
            acc = acc.pre_concat(t);
        }
    }
    [acc.sx, acc.ky, acc.kx, acc.sy, acc.tx, acc.ty]
}

/// One SVG transform function name + args → a tiny-skia `Transform` (§1.2).
/// Angles are degrees. Unknown/malformed → `None` (skipped by the caller).
fn transform_fn_matrix(name: &str, a: &[f32]) -> Option<Transform> {
    Some(match name {
        "translate" => Transform::from_translate(*a.first()?, a.get(1).copied().unwrap_or(0.0)),
        "scale" => {
            let sx = *a.first()?;
            Transform::from_scale(sx, a.get(1).copied().unwrap_or(sx))
        }
        "rotate" => {
            let r = Transform::from_rotate(*a.first()?); // degrees
            match (a.get(1), a.get(2)) {
                (Some(&cx), Some(&cy)) => Transform::from_translate(cx, cy)
                    .pre_concat(r)
                    .pre_concat(Transform::from_translate(-cx, -cy)),
                _ => r,
            }
        }
        "skewX" => Transform::from_row(1.0, 0.0, a.first()?.to_radians().tan(), 1.0, 0.0, 0.0),
        "skewY" => Transform::from_row(1.0, a.first()?.to_radians().tan(), 0.0, 1.0, 0.0, 0.0),
        "matrix" => {
            if a.len() != 6 {
                return None;
            }
            Transform::from_row(a[0], a[1], a[2], a[3], a[4], a[5])
        }
        _ => return None,
    })
}

/// Scan a `transform` attribute into `(name, args)` pairs. Lenient: stops at the
/// first malformed token; numbers split on `,`/whitespace (§1.2).
fn transform_fns(s: &str) -> Vec<(String, Vec<f32>)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // skip leading separators.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphabetic()) {
            i += 1;
        }
        if i == name_start {
            break; // no identifier → malformed; stop.
        }
        let name = s[name_start..i].to_string();
        // skip whitespace before '('.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'(' {
            break;
        }
        i += 1; // consume '('
        let args_start = i;
        while i < bytes.len() && bytes[i] != b')' {
            i += 1;
        }
        if i >= bytes.len() {
            break; // unterminated → malformed; stop.
        }
        let args = parse_number_list(&s[args_start..i]);
        i += 1; // consume ')'
        out.push((name, args));
    }
    out
}

/// Split a string into f32 numbers on `,`/whitespace (skips unparseable tokens).
fn parse_number_list(s: &str) -> Vec<f32> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .collect()
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

/// Resolved paints for a shape (E9-M1 §5.4, extended E9-M2 §5, E9-M3 §4.3).
struct Paints {
    fill: Option<SvgPaint>,
    stroke: Option<SvgPaint>,
    stroke_width: f32,
    fill_rule: SvgFillRule,
    cap: SvgLineCap,
    join: SvgLineJoin,
}

/// Build an `SvgShape` for one element child, or `None` for an unknown tag
/// (`defs`/…) or a degenerate shape. `transform` is the effective matrix (§1.3).
fn build_shape(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    transform: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
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

    let p = resolve_paints(doc, styled, id, ctx, grads);
    // A line never fills; a shape with neither fill nor stroke paints nothing.
    let fill = if matches!(geom, SvgGeom::Line { .. }) {
        None
    } else {
        p.fill
    };
    if fill.is_none() && p.stroke.is_none() {
        return None;
    }
    let bbox = geom_bbox(&geom);
    Some(PaintCmd::SvgShape {
        geom,
        transform,
        fill,
        fill_rule: p.fill_rule,
        stroke: p.stroke,
        stroke_width: p.stroke_width,
        stroke_cap: p.cap,
        stroke_join: p.join,
        bbox,
    })
}

/// The user-space bounding box of a shape geometry (objectBoundingBox, §4.1).
fn geom_bbox(geom: &SvgGeom) -> Rect {
    match geom {
        &SvgGeom::Rect { x, y, w, h, .. } => Rect {
            x,
            y,
            width: w,
            height: h,
        },
        &SvgGeom::Ellipse { cx, cy, rx, ry } => Rect {
            x: cx - rx,
            y: cy - ry,
            width: 2.0 * rx,
            height: 2.0 * ry,
        },
        &SvgGeom::Line { x1, y1, x2, y2 } => Rect {
            x: x1.min(x2),
            y: y1.min(y2),
            width: (x1 - x2).abs(),
            height: (y1 - y2).abs(),
        },
        SvgGeom::Path(ops) => {
            let mut min = (f32::INFINITY, f32::INFINITY);
            let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
            let mut acc = |x: f32, y: f32| {
                min.0 = min.0.min(x);
                min.1 = min.1.min(y);
                max.0 = max.0.max(x);
                max.1 = max.1.max(y);
            };
            for op in ops {
                match *op {
                    crate::svg_path::PathOp::MoveTo(x, y)
                    | crate::svg_path::PathOp::LineTo(x, y) => acc(x, y),
                    crate::svg_path::PathOp::QuadTo(cx, cy, x, y) => {
                        acc(cx, cy);
                        acc(x, y);
                    }
                    crate::svg_path::PathOp::CubicTo(a, b, c, d, x, y) => {
                        acc(a, b);
                        acc(c, d);
                        acc(x, y);
                    }
                    crate::svg_path::PathOp::Close => {}
                }
            }
            if min.0 > max.0 {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                }
            } else {
                Rect {
                    x: min.0,
                    y: min.1,
                    width: max.0 - min.0,
                    height: max.1 - min.1,
                }
            }
        }
    }
}

/// Resolve fill/stroke/stroke-width with opacity folded into alpha (E9-M1 §5.4,
/// E9-M3 §4.3). Lookup order per property: inline `style`, then the presentation
/// attribute, then the inherited `<g>` context, then the SVG initial. `url(#id)`
/// resolves to a gradient via the registry; solid colors as before.
fn resolve_paints(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    ctx: &SvgCtx,
    grads: &GradientRegistry,
) -> Paints {
    let style = doc.get_attribute(id, "style");
    let own = |name: &str| -> Option<String> {
        svg_style_prop(style, name).or_else(|| doc.get_attribute(id, name).map(str::to_string))
    };
    let current = styled.get(id).map(|s| s.color).unwrap_or(BLACK);

    let fill_v = own("fill").or_else(|| ctx.fill.clone());
    let stroke_v = own("stroke").or_else(|| ctx.stroke.clone());
    let fill = parse_svg_paint(fill_v.as_deref(), Some(BLACK), current, grads);
    let stroke = parse_svg_paint(stroke_v.as_deref(), None, current, grads);
    let sw = own("stroke-width")
        .or_else(|| ctx.stroke_width.clone())
        .and_then(|s| parse_len(&s))
        .unwrap_or(1.0)
        .max(0.0);

    let op = own("opacity").and_then(parse_opacity).unwrap_or(1.0);
    let fo = own("fill-opacity").and_then(parse_opacity).unwrap_or(1.0);
    let so = own("stroke-opacity").and_then(parse_opacity).unwrap_or(1.0);

    let fill_rule = match own("fill-rule").as_deref().map(str::trim) {
        Some("evenodd") => SvgFillRule::EvenOdd,
        _ => SvgFillRule::NonZero,
    };
    let cap = match own("stroke-linecap").as_deref().map(str::trim) {
        Some("round") => SvgLineCap::Round,
        Some("square") => SvgLineCap::Square,
        _ => SvgLineCap::Butt,
    };
    let join = match own("stroke-linejoin").as_deref().map(str::trim) {
        Some("round") => SvgLineJoin::Round,
        Some("bevel") => SvgLineJoin::Bevel,
        _ => SvgLineJoin::Miter,
    };

    Paints {
        fill: fill.map(|p| paint_with_alpha(p, op * fo)),
        stroke: stroke.map(|p| paint_with_alpha(p, op * so)),
        stroke_width: sw,
        fill_rule,
        cap,
        join,
    }
}

/// Parse a `fill`/`stroke` value into an `SvgPaint` (E9-M3 §4.3): a `url(#id)`
/// resolves to the registered gradient (missing → `None`); else a solid color.
fn parse_svg_paint(
    value: Option<&str>,
    default: Option<Rgba>,
    current: Rgba,
    grads: &GradientRegistry,
) -> Option<SvgPaint> {
    // Absent value → the SVG initial (e.g. fill=black) via `parse_paint`.
    let Some(v) = value.map(str::trim) else {
        return parse_paint(None, default, current).map(SvgPaint::Color);
    };
    if let Some(id) = parse_url_ref(v) {
        return grads.get(id).cloned().map(SvgPaint::Gradient);
    }
    parse_paint(Some(v), default, current).map(SvgPaint::Color)
}

/// `url(#id)` → `id`, else `None` (trivial prefix/suffix strip, §4.3).
fn parse_url_ref(v: &str) -> Option<&str> {
    let rest = v.strip_prefix("url(")?.strip_suffix(')')?.trim();
    let id = rest.strip_prefix('#')?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Scale a paint's alpha by `a` (0..=1): a solid color's alpha, or every
/// gradient stop's color alpha (so fill/stroke-opacity works on gradients, §4.3).
fn paint_with_alpha(p: SvgPaint, a: f32) -> SvgPaint {
    match p {
        SvgPaint::Color(c) => SvgPaint::Color(with_alpha(c, a)),
        SvgPaint::Gradient(mut g) => {
            for s in &mut g.stops {
                s.color = with_alpha(s.color, a);
            }
            SvgPaint::Gradient(g)
        }
    }
}

const BLACK: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};

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

// --- E9-M3 §4.2: gradient registry ---

/// Walk the whole svg subtree collecting `id` → `SvgGradient` (gradients may
/// live in `<defs>` or anywhere; nested `<defs>` handled by the recursion).
fn collect_gradients(doc: &Document, svg_id: NodeId) -> GradientRegistry {
    let mut reg = GradientRegistry::new();
    collect_gradients_rec(doc, svg_id, &mut reg);
    reg
}

fn collect_gradients_rec(doc: &Document, id: NodeId, reg: &mut GradientRegistry) {
    for child in doc.children(id) {
        if let Some(tag) = doc.tag_name(child) {
            match tag {
                "linearGradient" | "radialGradient" => {
                    if let Some(gid) = doc.get_attribute(child, "id") {
                        if let Some(g) = parse_gradient(doc, child, tag) {
                            reg.insert(gid.to_string(), g);
                        }
                    }
                }
                _ => {}
            }
            collect_gradients_rec(doc, child, reg);
        }
    }
}

/// Parse one `<linearGradient>`/`<radialGradient>` element into an `SvgGradient`
/// (§4.2). `None` if it has no usable stops.
fn parse_gradient(doc: &Document, id: NodeId, tag: &str) -> Option<SvgGradient> {
    let units = match doc.get_attribute(id, "gradientUnits").map(str::trim) {
        Some("userSpaceOnUse") => GradUnits::UserSpaceOnUse,
        _ => GradUnits::ObjectBoundingBox,
    };
    let obj = units == GradUnits::ObjectBoundingBox;
    let stops = parse_stops(doc, id);
    if stops.is_empty() {
        return None;
    }
    let kind = if tag == "linearGradient" {
        GradKind::Linear {
            x1: grad_coord(doc, id, "x1", 0.0, obj),
            y1: grad_coord(doc, id, "y1", 0.0, obj),
            x2: grad_coord(doc, id, "x2", 1.0, obj),
            y2: grad_coord(doc, id, "y2", 0.0, obj),
        }
    } else {
        GradKind::Radial {
            cx: grad_coord(doc, id, "cx", 0.5, obj),
            cy: grad_coord(doc, id, "cy", 0.5, obj),
            r: grad_coord(doc, id, "r", 0.5, obj),
        }
    };
    Some(SvgGradient { kind, stops, units })
}

/// A gradient geometry coordinate: `%` → fraction; under objectBoundingBox a
/// plain number is already a 0..1 fraction (§4.2).
fn grad_coord(doc: &Document, id: NodeId, name: &str, default: f32, _obj: bool) -> f32 {
    match doc.get_attribute(id, name) {
        None => default,
        Some(s) => {
            let s = s.trim();
            if let Some(p) = s.strip_suffix('%') {
                p.trim()
                    .parse::<f32>()
                    .ok()
                    .map(|v| v / 100.0)
                    .unwrap_or(default)
            } else {
                parse_len(s).unwrap_or(default)
            }
        }
    }
}

/// Parse the `<stop>` children of a gradient → `{color, pos}` in document order.
fn parse_stops(doc: &Document, id: NodeId) -> Vec<starfish_style::GradientStop> {
    let mut out = Vec::new();
    for child in doc.children(id) {
        if doc.tag_name(child) != Some("stop") {
            continue;
        }
        let style = doc.get_attribute(child, "style");
        let prop = |name: &str| -> Option<String> {
            svg_style_prop(style, name)
                .or_else(|| doc.get_attribute(child, name).map(str::to_string))
        };
        let color = prop("stop-color")
            .and_then(|c| starfish_css::parse_color(c.trim()))
            .unwrap_or(BLACK);
        let so = prop("stop-opacity").and_then(parse_opacity).unwrap_or(1.0);
        let pos = prop("offset").and_then(|o| parse_offset(&o));
        out.push(starfish_style::GradientStop {
            color: with_alpha(color, so),
            pos,
        });
    }
    out
}

/// Parse a `<stop offset>` value (`"50%"` → 0.5, `"0.5"` → 0.5; clamp 0..1).
fn parse_offset(s: &str) -> Option<f32> {
    let s = s.trim();
    let v = if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        s.parse::<f32>().ok()?
    };
    if v.is_finite() {
        Some(v.clamp(0.0, 1.0))
    } else {
        None
    }
}

// --- E9-M3 §3: <text> / <tspan> ---

/// A resolved SVG font (owned), built from a `<text>`/`<tspan>`'s attrs (§3.2).
struct SvgFont {
    family: Vec<String>,
    size: f32,
    weight: FontWeight,
    style: FontStyle,
}

impl SvgFont {
    fn query(&self) -> FontQuery<'_> {
        FontQuery {
            family: &self.family,
            style: self.style,
            weight: self.weight,
            size: self.size,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
    }
}

/// One placed text segment (a run starting at `x`, sharing the text's baseline).
struct TextSeg {
    x: f32,
    text: String,
}

/// Emit an SVG `<text>` as a `GlyphRun` per segment, bracketed by
/// `PushTransform`/`PopTransform` with the effective matrix (E9-M3 §3.2).
#[allow(clippy::too_many_arguments)]
fn emit_svg_text(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    id: NodeId,
    eff: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    out: &mut Vec<PaintCmd>,
) {
    let font = svg_font(doc, styled, id, ctx, None);
    // fill: a gradient on text falls back to the first stop's color (§3.5).
    let fill_v = svg_style_prop(doc.get_attribute(id, "style"), "fill")
        .or_else(|| doc.get_attribute(id, "fill").map(str::to_string))
        .or_else(|| ctx.fill.clone());
    let current = styled.get(id).map(|s| s.color).unwrap_or(BLACK);
    let color = match parse_svg_paint(fill_v.as_deref(), Some(BLACK), current, grads) {
        Some(SvgPaint::Color(c)) => c,
        Some(SvgPaint::Gradient(g)) => g.stops.first().map(|s| s.color).unwrap_or(BLACK),
        None => return, // fill:none → no glyphs
    };
    if color.a == 0 {
        return;
    }

    let x0 = attr_f(doc, id, "x");
    let y = attr_f(doc, id, "y");
    let lm = fonts.line_metrics(&font.query());

    let mut runs = Vec::new();
    let mut pen_x = x0;
    collect_text_runs(doc, id, &font, fonts, &mut pen_x, &mut runs);

    // text-anchor shifts every segment's x by 0 / -w/2 / -w (whole-run, §3.4).
    let anchor = svg_style_prop(doc.get_attribute(id, "style"), "text-anchor")
        .or_else(|| doc.get_attribute(id, "text-anchor").map(str::to_string));
    let shift = match anchor.as_deref().map(str::trim) {
        Some("middle") => -(pen_x - x0) / 2.0,
        Some("end") => -(pen_x - x0),
        _ => 0.0,
    };

    if runs.is_empty() {
        return;
    }
    out.push(PaintCmd::PushTransform { matrix: eff });
    for seg in runs {
        if seg.text.is_empty() {
            continue;
        }
        out.push(PaintCmd::GlyphRun {
            origin: (seg.x + shift, y - lm.ascent),
            text: seg.text,
            font_size: font.size,
            weight: font.weight,
            style: font.style,
            family: font.family.clone(),
            color,
            ascent: lm.ascent,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        });
    }
    out.push(PaintCmd::PopTransform);
}

/// Walk a `<text>`/`<tspan>`'s children, appending segments. Bare text advances
/// the pen inline; a `<tspan x=…>` repositions the pen (§3.3).
fn collect_text_runs(
    doc: &Document,
    id: NodeId,
    font: &SvgFont,
    fonts: &FontDb,
    pen_x: &mut f32,
    out: &mut Vec<TextSeg>,
) {
    for child in doc.children(id) {
        match doc.kind(child) {
            starfish_dom::NodeKind::Text(s) => {
                if s.is_empty() {
                    continue;
                }
                let seg_x = *pen_x;
                *pen_x += fonts.advance_width(s, &font.query());
                out.push(TextSeg {
                    x: seg_x,
                    text: s.clone(),
                });
            }
            starfish_dom::NodeKind::Element(_) if doc.tag_name(child) == Some("tspan") => {
                // A tspan x/y override repositions the pen (basic; §3.3).
                if let Some(nx) = attr_opt_f(doc, child, "x") {
                    *pen_x = nx;
                }
                collect_text_runs(doc, child, font, fonts, pen_x, out);
            }
            _ => {}
        }
    }
}

/// Resolve the font for an SVG text element from attrs/inline-style, falling
/// back to the element's CSS style then sane defaults (§3.2). `inherit` carries
/// the parent `<text>`'s font for a `<tspan>` (unused at the top level → None).
fn svg_font(
    doc: &Document,
    styled: &StyledTree,
    id: NodeId,
    _ctx: &SvgCtx,
    inherit: Option<&SvgFont>,
) -> SvgFont {
    let style = doc.get_attribute(id, "style");
    let prop = |name: &str| -> Option<String> {
        svg_style_prop(style, name).or_else(|| doc.get_attribute(id, name).map(str::to_string))
    };
    let css = styled.get(id);

    let size = prop("font-size")
        .and_then(|s| parse_len(&s))
        .or_else(|| inherit.map(|f| f.size))
        .or_else(|| css.map(|s| s.font_size))
        .unwrap_or(16.0);

    let family = match prop("font-family") {
        Some(f) => {
            let list: Vec<String> = f
                .split(',')
                .map(|p| p.trim().trim_matches(['"', '\'']).to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if list.is_empty() {
                default_family(css, inherit)
            } else {
                list
            }
        }
        None => default_family(css, inherit),
    };

    let weight = match prop("font-weight").as_deref().map(str::trim) {
        Some("bold") => FontWeight(700),
        Some("normal") => FontWeight(400),
        Some(n) => n.parse::<u16>().map(FontWeight).unwrap_or(FontWeight(400)),
        None => inherit
            .map(|f| f.weight)
            .or_else(|| css.map(|s| s.font_weight))
            .unwrap_or(FontWeight(400)),
    };
    let style_v = match prop("font-style").as_deref().map(str::trim) {
        Some("italic") => FontStyle::Italic,
        Some("oblique") => FontStyle::Oblique,
        Some("normal") => FontStyle::Normal,
        _ => inherit
            .map(|f| f.style)
            .or_else(|| css.map(|s| s.font_style))
            .unwrap_or(FontStyle::Normal),
    };

    SvgFont {
        family,
        size,
        weight,
        style: style_v,
    }
}

/// The font-family fallback: the parent text's, else the element's CSS list,
/// else sans-serif.
fn default_family(css: Option<&ComputedStyle>, inherit: Option<&SvgFont>) -> Vec<String> {
    if let Some(f) = inherit {
        return f.family.clone();
    }
    match css {
        Some(s) if !s.font_family.is_empty() => s.font_family.clone(),
        _ => vec!["sans-serif".to_string()],
    }
}

/// Sharp (non-rounded) border dispatcher: `Solid` keeps the exact original
/// edge-fill path (`emit_borders_solid`); `Dashed`/`Dotted`/`Double` emit a
/// `StrokeLine` per edge; `None` emits nothing (E13-M4).
fn emit_borders(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    match style.border_style {
        BorderStyle::None => {}
        BorderStyle::Solid => emit_borders_solid(b, styled, out),
        BorderStyle::Dashed | BorderStyle::Dotted | BorderStyle::Double => {
            emit_borders_stroke(b, styled, out)
        }
    }
}

/// Emit non-solid sharp borders (`dashed`/`dotted`/`double`) as a `StrokeLine`
/// per edge, centered along the edge and spanning the border-box (E13-M4).
fn emit_borders_stroke(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    let bc = style.border_color;
    if bc.a == 0 {
        return;
    }
    let bs = style.border_style;
    let d = b.dimensions();
    let bb = d.border_box();
    let mut edge = |from: (f32, f32), to: (f32, f32), width: f32| {
        if width > 0.0 {
            out.push(PaintCmd::StrokeLine {
                from,
                to,
                width,
                color: bc,
                style: bs,
            });
        }
    };
    // Center lines span the full border-box edge; width = that edge's border.
    let (l, t, r, bot) = (bb.x, bb.y, bb.x + bb.width, bb.y + bb.height);
    if d.border.top > 0.0 {
        let cy = t + d.border.top / 2.0;
        edge((l, cy), (r, cy), d.border.top);
    }
    if d.border.bottom > 0.0 {
        let cy = bot - d.border.bottom / 2.0;
        edge((l, cy), (r, cy), d.border.bottom);
    }
    if d.border.left > 0.0 {
        let cx = l + d.border.left / 2.0;
        edge((cx, t), (cx, bot), d.border.left);
    }
    if d.border.right > 0.0 {
        let cx = r - d.border.right / 2.0;
        edge((cx, t), (cx, bot), d.border.right);
    }
}

/// Solid sharp borders: four edge fill-rects. Moved verbatim from the original
/// `emit_borders` so solid output stays byte-identical (E13-M4).
fn emit_borders_solid(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    let bc = style.border_color;
    if bc.a == 0 {
        return;
    }
    let d = b.dimensions();
    let bb = d.border_box();

    if d.border.top > 0.0 {
        out.push(fill(
            Rect {
                x: bb.x,
                y: bb.y,
                width: bb.width,
                height: d.border.top,
            },
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

/// Emit a vertical-writing-mode text run (E18-M3). The layout slot is tall and
/// narrow (`content.width` = line-height, `content.height` = inline advance).
///
/// - `sideways` / `mixed` (mixed≈sideways): rotate the whole run +90° clockwise
///   about the slot's top-left, then emit a normal horizontal `GlyphRun` at that
///   origin — the rotation lays the glyphs down the tall slot.
/// - `upright`: no rotation; one single-char `GlyphRun` per char, sharing the
///   slot's x (centered on the line column) and advancing down Y by ≈ font_size.
///   Decoration is skipped for upright (documented).
fn emit_text_vertical(
    b: &LayoutBox,
    style: &ComputedStyle,
    text: &str,
    ascent: f32,
    out: &mut Vec<PaintCmd>,
) {
    let c = b.dimensions().content;
    match style.text_orientation {
        TextOrientation::Upright => {
            // Stack one glyph per char down the column. Center each on the column
            // x (slot is `c.width` wide); advance Y by ≈ font_size.
            let step = style.font_size;
            let mut y = c.y;
            for ch in text.chars() {
                let mut s = String::new();
                s.push(ch);
                out.push(PaintCmd::GlyphRun {
                    origin: (c.x, y),
                    text: s,
                    font_size: style.font_size,
                    weight: style.font_weight,
                    style: style.font_style,
                    family: style.font_family.clone(),
                    color: style.color,
                    ascent,
                    letter_spacing: style.letter_spacing,
                    word_spacing: style.word_spacing,
                });
                y += step;
            }
        }
        // Mixed ≈ Sideways: a single CW90-rotated horizontal run.
        TextOrientation::Mixed | TextOrientation::Sideways => {
            // Pivot the +90° clockwise rotation about the slot's top-left `(ox,oy)`:
            // M = T(o)·RotateCW90·T(-o). The row matrix for CW90 is
            // [0, 1, -1, 0, tx, ty] (x'=-y, y'=x, then re-translate to the pivot).
            let (ox, oy) = (c.x, c.y);
            let m = Transform::from_translate(ox, oy)
                .pre_concat(Transform::from_row(0.0, 1.0, -1.0, 0.0, 0.0, 0.0))
                .pre_concat(Transform::from_translate(-ox, -oy));
            out.push(PaintCmd::PushTransform {
                matrix: [m.sx, m.ky, m.kx, m.sy, m.tx, m.ty],
            });
            out.push(PaintCmd::GlyphRun {
                origin: (c.x, c.y),
                text: text.to_string(),
                font_size: style.font_size,
                weight: style.font_weight,
                style: style.font_style,
                family: style.font_family.clone(),
                color: style.color,
                ascent,
                letter_spacing: style.letter_spacing,
                word_spacing: style.word_spacing,
            });
            out.push(PaintCmd::PopTransform);
        }
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
    // text-shadow (E16-M3): paint the same shaped run, offset + in the shadow
    // color, just BEHIND the glyph run. A None shadow / fully transparent color
    // emits nothing (keeps shadowless pages byte-identical).
    if let Some(s) = style.text_shadow {
        if s.color.a != 0 {
            out.push(PaintCmd::GlyphShadow {
                origin: (c.x + s.offset_x, c.y + s.offset_y),
                text: text.to_string(),
                font_size: style.font_size,
                weight: style.font_weight,
                style: style.font_style,
                family: style.font_family.clone(),
                color: s.color,
                ascent: lm.ascent,
                letter_spacing: style.letter_spacing,
                word_spacing: style.word_spacing,
                blur: s.blur.max(0.0),
            });
        }
    }
    // E18-M3 vertical writing modes: the layout slot is a tall narrow box
    // (w = line-height, h = inline advance). Emit a rotated/stacked glyph run.
    // Default (horizontal-tb) keeps the verbatim path below (byte-identical).
    if style.writing_mode.is_vertical() {
        emit_text_vertical(b, style, text, lm.ascent, out);
        return;
    }

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
            Rect {
                x: c.x,
                y,
                width: c.width,
                height: thickness,
            },
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
        let first_bg = cmds.iter().position(
            |c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0),
        );
        let first_border = cmds.iter().position(
            |c| matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0),
        );
        let first_glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
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
        let found = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.g == 255 && color.r == 0 && rect.width == 100.0
            )
        });
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
            PaintCmd::GlyphRun {
                color,
                font_size,
                text,
                ..
            } => Some((*color, *font_size, text.clone())),
            _ => None,
        });
        let (color, fs, text) = glyph.expect("glyph run");
        assert_eq!(
            color,
            Rgba {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            }
        );
        assert_eq!(fs, 20.0);
        assert_eq!(text, "hi");
    }

    // --- E2-M1: text-decoration, list markers, inline-block ---

    /// The glyph run + its content rect for the first text run matching `t`.
    fn glyph_with_origin(cmds: &[PaintCmd], t: &str) -> (f32, f32, f32) {
        cmds.iter()
            .find_map(|c| match c {
                PaintCmd::GlyphRun { origin, text, .. } if text == t => {
                    Some((origin.0, origin.1, 0.0))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no glyph run {t:?}"))
    }

    #[test]
    fn underline_emits_fillrect_below_baseline() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;text-decoration:underline}",
        );
        // exactly one fill rect (the underline) at baseline+1.
        let fills: Vec<&PaintCmd> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .collect();
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
        assert_eq!(
            color,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
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
        let fills = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .count();
        assert_eq!(fills, 3, "underline+overline+line-through: {cmds:?}");
    }

    #[test]
    fn marker_emits_bullet_glyph() {
        let cmds = list(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0}",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")),
            "expected a bullet glyph run: {cmds:?}"
        );
    }

    #[test]
    fn decimal_marker_emits_number_glyph() {
        let cmds = list(
            "<html><body><ol><li>x</li></ol></body></html>",
            "body{margin:0}",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "1.")),
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
        let found = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.g == 255 && color.r == 0 && rect.width == 50.0
            )
        });
        assert!(
            found,
            "expected a green 50px-wide inline-block bg: {cmds:?}"
        );
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
        let red =
            first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("in-flow red bg");
        let green =
            first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("float green bg");
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
        let bg = cmds.iter().position(
            |c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0),
        );
        let border = cmds.iter().position(
            |c| matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0),
        );
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (bg, border, glyph) = (
            bg.expect("bg"),
            border.expect("border"),
            glyph.expect("glyph"),
        );
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
        assert!(cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")));
        // ...but only the "a" TextRun produces an underline rect, not the marker.
        // There's exactly one decoration FillRect (for "a").
        let fills = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .count();
        assert_eq!(
            fills, 1,
            "only the text run is decorated, not the marker: {cmds:?}"
        );
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
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
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
            PaintCmd::ImageBlit {
                dest,
                src,
                src_crop,
                smooth,
                ..
            } => Some((*dest, src.clone(), *src_crop, *smooth)),
            _ => None,
        });
        let (dest, src, src_crop, smooth) = blit.expect("an ImageBlit command");
        assert_eq!(dest.width, 4.0);
        assert_eq!(src, "px.png");
        // Default object-fit:fill + image-rendering:auto → full crop + nearest.
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0
            }
        );
        assert!(!smooth);
    }

    #[test]
    fn srcset_density_blit_uses_chosen_url() {
        // E15-M2: with DPR 1, `a.png 1x, b.png 2x` selects the 1x candidate, and
        // the ImageBlit must carry that SAME url (decode == blit via the shared
        // resolve_img_src). We decode the chosen file so a blit is emitted.
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-srcset-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([0, 255, 0, 255]));
        img.save(dir.join("a.png")).unwrap();
        img.save(dir.join("b.png")).unwrap();

        let html =
            "<html><body><img srcset='a.png 1x, b.png 2x' width='4' height='4'></body></html>";
        let doc = parse(html);
        let sheet = parse_stylesheet("body{margin:0}");
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        // Pre-pass decode the chosen (1x) candidate, mirroring render_document.
        images.get("a.png");
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        let cmds = build_display_list(&root, &styled, &fonts, &images, &doc);

        let src = cmds.iter().find_map(|c| match c {
            PaintCmd::ImageBlit { src, .. } => Some(src.clone()),
            _ => None,
        });
        assert_eq!(
            src.as_deref(),
            Some("a.png"),
            "blit src must be the chosen 1x url"
        );
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

    // --- E15-M3: <img src=*.svg>, <video>/<audio>, broken-img alt ---

    /// Build a display list for `html`+`css`, writing each `(name, bytes)` into
    /// the document dir and decoding each `src` into the store (mirroring the
    /// render_document pre-pass). Used by the E15-M3 SVG-img / media / alt tests.
    fn list_with_files(
        html: &str,
        css: &str,
        files: &[(&str, &[u8])],
        decode: &[&str],
    ) -> Vec<PaintCmd> {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-m3-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        for src in decode {
            images.get(src);
        }
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    #[test]
    fn svg_img_renders_via_svg_painter() {
        let svg = b"<svg viewBox='0 0 40 30'><circle cx='20' cy='15' r='10' fill='red'/></svg>";
        let cmds = list_with_files(
            "<html><body><img src='c.svg' width='80' height='60'></body></html>",
            "body{margin:0}",
            &[("c.svg", svg)],
            &["c.svg"],
        );
        // No raster blit — it renders as SVG shapes.
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // The red circle becomes an Ellipse with a red fill, scaled by the
        // viewBox(40×30)→box(80×60) transform (factor 2 on each axis).
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::SvgShape { geom: SvgGeom::Ellipse { rx, ry, .. }, fill: Some(SvgPaint::Color(col)), transform, .. }
                if *col == red() && *rx == 10.0 && *ry == 10.0 && transform[0] == 2.0 && transform[3] == 2.0
        ));
        assert!(found, "expected a scaled red Ellipse: {cmds:?}");
    }

    #[test]
    fn video_poster_emits_imageblit() {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-vp-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("px.png")).unwrap();

        let doc = parse(
            "<html><body><video poster='px.png' width='100' height='50'></video></body></html>",
        );
        let sheet = parse_stylesheet("body{margin:0}");
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        images.get("px.png"); // mirror the <video poster> pre-pass decode.
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        let cmds = build_display_list(&root, &styled, &fonts, &images, &doc);

        let blit = cmds.iter().find_map(|c| match c {
            PaintCmd::ImageBlit { dest, src, .. } => Some((*dest, src.clone())),
            _ => None,
        });
        let (dest, src) = blit.expect("a poster ImageBlit");
        assert_eq!(src, "px.png");
        assert_eq!((dest.width, dest.height), (100.0, 50.0));
    }

    #[test]
    fn posterless_video_emits_dark_box_and_triangle() {
        let cmds = list_with_files(
            "<html><body><video width='100' height='50'></video></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // A dark (0x33) fill over the 100×50 box.
        let dark = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.r == 0x33 && color.g == 0x33 && color.b == 0x33
                        && rect.width == 100.0 && rect.height == 50.0
            )
        });
        assert!(dark, "expected a dark video box: {cmds:?}");
        // A white play triangle (a filled Path).
        let tri = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Path(_), fill: Some(SvgPaint::Color(col)), .. }
                    if col.r == 0xff && col.g == 0xff && col.b == 0xff
            )
        });
        assert!(tri, "expected a white play triangle: {cmds:?}");
    }

    #[test]
    fn audio_emits_dark_box_no_triangle() {
        let cmds = list_with_files(
            "<html><body><audio></audio></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // A dark box (default 300×54) but NO play triangle.
        let dark = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, .. }
                    if color.r == 0x33 && color.g == 0x33 && color.b == 0x33
            )
        });
        assert!(dark, "expected a dark audio box: {cmds:?}");
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::SvgShape { .. })),
            "audio must not paint a triangle: {cmds:?}"
        );
    }

    #[test]
    fn broken_img_with_alt_emits_text() {
        let cmds = list_with_files(
            "<html><body><img src='nope.png' alt='cat' width='40' height='20'></body></html>",
            "body{margin:0}",
            &[],
            &["nope.png"],
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // Still the 4 grey placeholder edges.
        let grey = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 0x80 && color.g == 0x80 && color.b == 0x80))
            .count();
        assert_eq!(grey, 4, "expected 4 placeholder border rects: {cmds:?}");
        // The alt text in a clipped glyph run.
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "cat")));
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopClip)));
    }

    // --- E15-M1: object-fit / object-position / image-rendering emission ---

    /// Like `list_with_fixture` but writes a `iw`×`ih` image at `obj.png`, so
    /// object-fit geometry (which depends on the intrinsic vs box ratio) can be
    /// exercised.
    fn list_with_image(iw: u32, ih: u32, html: &str, css: &str) -> Vec<PaintCmd> {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-of-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(iw, ih);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(dir.join("obj.png")).unwrap();

        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        images.get("obj.png");
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    /// Pull the (dest, src_crop, smooth) of the single `ImageBlit` in `cmds`.
    fn blit_of(cmds: &[PaintCmd]) -> (Rect, Rect, bool) {
        cmds.iter()
            .find_map(|c| match c {
                PaintCmd::ImageBlit {
                    dest,
                    src_crop,
                    smooth,
                    ..
                } => Some((*dest, *src_crop, *smooth)),
                _ => None,
            })
            .expect("an ImageBlit command")
    }

    #[test]
    fn object_fit_fill_is_unchanged_default() {
        // Box 8×4, image 2×2: default fill stretches the full image into the box,
        // nearest. (Byte-identity fast path.)
        let cmds = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='8' height='4'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, smooth) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 4.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0
            }
        );
        assert!(!smooth);
    }

    #[test]
    fn object_fit_contain_letterboxes_centered() {
        // Box 8×4, image 2×2 (square). contain → scale = min(8/2,4/2)=2 → 4×4,
        // centered horizontally: dx = (8-4)/2 = 2, dy = 0. Full crop.
        let cmds = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='8' height='4' style='object-fit:contain'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 2.0,
                y: 0.0,
                width: 4.0,
                height: 4.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0
            }
        );
    }

    #[test]
    fn object_fit_cover_fills_box_and_crops_source() {
        // Box 8×4, image 2×2. cover → scale = max(8/2,4/2)=4 → sw=8/4=2 (full),
        // sh=4/4=1 (half). Dest = box; crop = vertical half centered: sy=(2-1)/2=0.5.
        let cmds = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='8' height='4' style='object-fit:cover'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 4.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.5,
                width: 2.0,
                height: 1.0
            }
        );
    }

    #[test]
    fn object_fit_none_is_intrinsic_clipped() {
        // Box 4×4, image 8×8. none → 1:1, centered: off = (4-8)/2 = -2 each axis.
        // Visible box-relative [0,4); src offset 2; span 4. Dest = box; crop 4×4 at (2,2).
        let cmds = list_with_image(
            8,
            8,
            "<html><body><img src='obj.png' width='4' height='4' style='object-fit:none'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 2.0,
                y: 2.0,
                width: 4.0,
                height: 4.0
            }
        );
    }

    #[test]
    fn object_fit_scale_down_picks_none_when_fits() {
        // Box 8×8, image 2×2 (fits) → scale-down behaves like none: 1:1, centered.
        // off = (8-2)/2 = 3; dest at (3,3) size 2×2; full crop.
        let cmds = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='8' height='8' style='object-fit:scale-down'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 3.0,
                y: 3.0,
                width: 2.0,
                height: 2.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0
            }
        );
    }

    #[test]
    fn object_fit_scale_down_picks_contain_when_larger() {
        // Box 4×4, image 8×8 (too big) → scale-down behaves like contain:
        // scale=min(4/8,4/8)=0.5 → 4×4 filling the box; full crop.
        let cmds = list_with_image(
            8,
            8,
            "<html><body><img src='obj.png' width='4' height='4' style='object-fit:scale-down'></body></html>",
            "body{margin:0}",
        );
        let (dest, src_crop, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0
            }
        );
        assert_eq!(
            src_crop,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 8.0
            }
        );
    }

    #[test]
    fn object_position_shifts_contain() {
        // Box 8×4, image 2×2, contain → 4×4. object-position: right top →
        // x at 100% of free (8-4=4) → dx=4; y at 0% of free (4-4=0) → dy=0.
        let cmds = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='8' height='4' \
             style='object-fit:contain;object-position:right top'></body></html>",
            "body{margin:0}",
        );
        let (dest, _, _) = blit_of(&cmds);
        assert_eq!(
            dest,
            Rect {
                x: 4.0,
                y: 0.0,
                width: 4.0,
                height: 4.0
            }
        );
    }

    #[test]
    fn image_rendering_selects_smooth() {
        let smooth = list_with_image(
            2,
            2,
            "<html><body><img src='obj.png' width='4' height='4' style='image-rendering:smooth'></body></html>",
            "body{margin:0}",
        );
        assert!(blit_of(&smooth).2, "smooth → bilinear");
        for kw in ["pixelated", "auto", "crisp-edges"] {
            let html = format!(
                "<html><body><img src='obj.png' width='4' height='4' style='image-rendering:{kw}'></body></html>"
            );
            let cmds = list_with_image(2, 2, &html, "body{margin:0}");
            assert!(!blit_of(&cmds).2, "{kw} → nearest");
        }
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
            .position(|c| matches!(c, PaintCmd::PushLayer { opacity, .. } if *opacity == 0.5))
            .expect("PushLayer{0.5}");
        let pop = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopLayer))
            .expect("PopLayer");
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        assert!(push < pop, "push {push} before pop {pop}");
        if let Some(g) = glyph {
            assert!(push < g && g < pop, "glyph {g} inside the layer bracket");
        }
        // No layer for opacity == 1.
        let plain = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:50px;height:50px}",
        );
        assert!(!plain
            .iter()
            .any(|c| matches!(c, PaintCmd::PushLayer { .. })));
    }

    // --- E21-M1: filter forces an offscreen layer ---

    #[test]
    fn filter_forces_push_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{filter:blur(2px);background:#ff0000;width:50px;height:50px}",
        );
        // opacity is 1.0 but a non-empty filter still brackets the subtree.
        let pushed = cmds.iter().any(
            |c| matches!(c, PaintCmd::PushLayer { opacity, filter, .. } if *opacity == 1.0 && !filter.is_empty()),
        );
        assert!(pushed, "filter must force a PushLayer: {cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopLayer)));
    }

    // --- E21-M2: blend modes ---

    #[test]
    fn mix_blend_mode_forces_push_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{mix-blend-mode:multiply;background:#ff0000;width:50px;height:50px}",
        );
        let pushed = cmds.iter().any(
            |c| matches!(c, PaintCmd::PushLayer { blend, .. } if *blend == BlendMode::Multiply),
        );
        assert!(pushed, "mix-blend-mode must force a PushLayer: {cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopLayer)));
    }

    #[test]
    fn mix_blend_mode_normal_no_push_layer() {
        // Byte-identity sentinel: the initial Normal must not bracket the subtree.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:50px;height:50px}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })),
            "normal mix-blend-mode must not push a layer: {cmds:?}"
        );
    }

    #[test]
    fn background_blend_mode_emits_bg_group() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background-image:linear-gradient(#f00,#00f),linear-gradient(#0f0,#000);background-blend-mode:multiply;width:50px;height:50px}",
        );
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::PushBgGroup)),
            "background-blend-mode must emit a PushBgGroup: {cmds:?}"
        );
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopBgGroup)));
    }

    #[test]
    fn background_blend_mode_normal_no_bg_group() {
        // Byte-identity: an empty/Normal background-blend-mode emits no group.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background-image:linear-gradient(#f00,#00f);width:50px;height:50px}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushBgGroup)),
            "no background-blend-mode must not emit a group: {cmds:?}"
        );
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
        let green = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.g == 255 && color.r == 0 && color.b == 0
                        && rect.x == 100.0 && rect.y == 0.0
                        && rect.width == 100.0 && rect.height == 50.0
            )
        });
        assert!(green, "green item at top-right cell (100,0): {cmds:?}");
        // item c (blue) sits in the bottom-left cell at (0,50).
        let blue = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.b == 255 && color.r == 0 && color.g == 0
                        && rect.x == 0.0 && rect.y == 50.0
                        && rect.width == 100.0 && rect.height == 50.0
            )
        });
        assert!(blue, "blue item at bottom-left cell (0,50): {cmds:?}");
        // item d (yellow) in the bottom-right cell at (100,50).
        let yellow = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.r == 255 && color.g == 255 && color.b == 0
                        && rect.x == 100.0 && rect.y == 50.0
            )
        });
        assert!(
            yellow,
            "yellow item at bottom-right cell (100,50): {cmds:?}"
        );
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
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::PushTransform { .. })));
        // transform:none likewise.
        let none = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:none;background:#ff0000;width:40px;height:40px}",
        );
        assert!(!none
            .iter()
            .any(|c| matches!(c, PaintCmd::PushTransform { .. })));
    }

    #[test]
    fn transform_outside_opacity_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:translate(10px);opacity:0.5;\
             background:#ff0000;width:40px;height:40px}",
        );
        let pt = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushTransform { .. }))
            .unwrap();
        let pl = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushLayer { .. }))
            .unwrap();
        let popl = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopLayer))
            .unwrap();
        let popt = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopTransform))
            .unwrap();
        // PushTransform, PushLayer, …, PopLayer, PopTransform
        assert!(pt < pl && pl < popl && popl < popt, "{cmds:?}");
    }

    #[test]
    fn translate_matrix_is_origin_independent() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:translate(20px,10px);width:40px;height:40px}",
        );
        let m = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::PushTransform { matrix } => Some(*matrix),
                _ => None,
            })
            .expect("a matrix");
        // pure translate is origin-independent → [1,0,0,1,20,10].
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(m[0], 1.0) && approx(m[1], 0.0) && approx(m[2], 0.0) && approx(m[3], 1.0));
        assert!(
            approx(m[4], 20.0) && approx(m[5], 10.0),
            "tx,ty = {},{}",
            m[4],
            m[5]
        );
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
        let any_serif = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::GlyphRun { family, .. } if family == &vec!["serif".to_string()]
            )
        });
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
            PaintCmd::GlyphRun {
                letter_spacing,
                word_spacing,
                ..
            } => Some((*letter_spacing, *word_spacing)),
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
        let found = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { radius, rect, .. }
                    if rect.width == 50.0 && radius == &[10.0; 4]
            )
        });
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
        assert_eq!(
            glyph,
            Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })
        );
    }

    #[test]
    fn li_custom_bullet_before() {
        // li::before red bullet renders before the list item's text.
        let cmds = list(
            "<html><body><ul style='list-style:none'><li>One</li></ul></body></html>",
            "body{margin:0} li::before { content: \"\u{2022} \"; color: #ff0000 }",
        );
        let texts: Vec<_> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::GlyphRun { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let bullet = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text.contains('\u{2022}') => Some(*color),
            _ => None,
        });
        assert_eq!(
            bullet,
            Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }),
            "glyphs={texts:?}"
        );
    }

    #[test]
    fn after_text_appends() {
        let cmds = list(
            "<html><body><p><a>link</a></p></body></html>",
            "body{margin:0} a::after { content: \" \u{2197}\"; color: #0000ff }",
        );
        let texts: Vec<_> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::GlyphRun { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let mark = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text.contains('\u{2197}') => Some(*color),
            _ => None,
        });
        assert_eq!(
            mark,
            Some(Rgba {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            }),
            "glyphs={texts:?}"
        );
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
        let red = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.r == 255 && color.g == 0 && color.b == 0
                       && rect.x == 0.0 && rect.y == 0.0
            )
        });
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
        let grey = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.r == 0x88 && color.g == 0x88 && color.b == 0x88 && rect.width > 0.0
            )
        });
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
        assert!(cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "wide")));
    }

    // --- E9-M1: inline SVG shape display-list emission ---

    /// Collect the SvgShape commands from a display list.
    fn svg_shapes(cmds: &[PaintCmd]) -> Vec<&PaintCmd> {
        cmds.iter()
            .filter(|c| matches!(c, PaintCmd::SvgShape { .. }))
            .collect()
    }

    fn red() -> Rgba {
        Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        }
    }
    fn blue() -> Rgba {
        Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        }
    }

    /// The solid color of an `Option<SvgPaint>` (gradient → its first stop, or
    /// `None`). Lets E9-M1/M2 tests assert solid-color fills/strokes (E9-M3 §4.1).
    fn paint_color(p: &Option<SvgPaint>) -> Option<Rgba> {
        match p {
            Some(SvgPaint::Color(c)) => Some(*c),
            Some(SvgPaint::Gradient(g)) => g.stops.first().map(|s| s.color),
            None => None,
        }
    }

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
            PaintCmd::SvgShape {
                geom,
                transform,
                fill,
                stroke,
                ..
            } => {
                assert_eq!(
                    *geom,
                    SvgGeom::Rect {
                        x: 10.0,
                        y: 10.0,
                        w: 80.0,
                        h: 80.0,
                        rx: 0.0,
                        ry: 0.0
                    }
                );
                assert_eq!(paint_color(fill), Some(red()));
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
            PaintCmd::SvgShape { geom: SvgGeom::Ellipse { cx, cy, rx, ry }, fill: Some(SvgPaint::Color(c)), .. }
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
            PaintCmd::SvgShape {
                geom, transform, ..
            } => {
                // geometry stays in user coords; the transform scales.
                assert_eq!(
                    *geom,
                    SvgGeom::Rect {
                        x: 1.0,
                        y: 1.0,
                        w: 8.0,
                        h: 8.0,
                        rx: 0.0,
                        ry: 0.0
                    }
                );
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
        let fills: Vec<Rgba> = svg_shapes(&cmds)
            .iter()
            .filter_map(|c| match c {
                PaintCmd::SvgShape { fill, .. } => paint_color(fill),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 3);
        assert_eq!(
            fills[0],
            Rgba {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }
        );
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
            PaintCmd::SvgShape { fill: Some(SvgPaint::Color(c)), .. } if *c == BLACK));
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
            PaintCmd::SvgShape {
                fill: Some(SvgPaint::Color(c)),
                ..
            } => {
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
            PaintCmd::SvgShape { fill: Some(SvgPaint::Color(c)), .. } if *c == blue()));
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
            PaintCmd::SvgShape {
                geom,
                fill,
                fill_rule,
                ..
            } => {
                assert_eq!(
                    *geom,
                    SvgGeom::Path(vec![
                        PathOp::MoveTo(10.0, 10.0),
                        PathOp::LineTo(90.0, 10.0),
                        PathOp::LineTo(90.0, 90.0),
                        PathOp::Close,
                    ])
                );
                assert_eq!(paint_color(fill), Some(red()));
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
        assert!(matches!(
            shapes[0],
            PaintCmd::SvgShape {
                fill_rule: SvgFillRule::EvenOdd,
                ..
            }
        ));
    }

    #[test]
    fn svg_polygon_closes_polyline_does_not() {
        let poly = list(
            "<html><body><svg width='100' height='100'>\
             <polygon points='50,5 90,90 10,90' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&poly)[0] {
            PaintCmd::SvgShape {
                geom: SvgGeom::Path(ops),
                ..
            } => {
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
            PaintCmd::SvgShape {
                geom: SvgGeom::Path(ops),
                ..
            } => {
                assert!(
                    !ops.iter().any(|o| matches!(o, PathOp::Close)),
                    "polyline has no Close"
                );
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
        assert!(matches!(
            svg_shapes(&cmds)[0],
            PaintCmd::SvgShape {
                stroke_cap: SvgLineCap::Round,
                stroke_join: SvgLineJoin::Bevel,
                ..
            }
        ));
    }

    // --- E9-M3: transform attribute / <g> / gradients / <text> ---

    fn approx6(a: [f32; 6], b: [f32; 6], tol: f32) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn parse_transform_attr_translate() {
        assert!(approx6(
            parse_transform_attr("translate(50,0)"),
            [1.0, 0.0, 0.0, 1.0, 50.0, 0.0],
            1e-4
        ));
        // single-arg translate → ty defaults to 0.
        assert!(approx6(
            parse_transform_attr("translate(7)"),
            [1.0, 0.0, 0.0, 1.0, 7.0, 0.0],
            1e-4
        ));
    }

    #[test]
    fn parse_transform_attr_rotate() {
        // rotate(90) → [0,1,-1,0,0,0].
        assert!(approx6(
            parse_transform_attr("rotate(90)"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
            1e-4
        ));
    }

    #[test]
    fn parse_transform_attr_rotate_about_center() {
        let m = to_transform(parse_transform_attr("rotate(90,10,10)"));
        let map = |x: f32, y: f32| {
            let mut p = [tiny_skia::Point::from_xy(x, y)];
            m.map_points(&mut p);
            (p[0].x, p[0].y)
        };
        // (10,10) is the fixed point; (20,10) → (10,20).
        let (fx, fy) = map(10.0, 10.0);
        assert!(
            (fx - 10.0).abs() < 1e-3 && (fy - 10.0).abs() < 1e-3,
            "fixed=({fx},{fy})"
        );
        let (ax, ay) = map(20.0, 10.0);
        assert!(
            (ax - 10.0).abs() < 1e-3 && (ay - 20.0).abs() < 1e-3,
            "mapped=({ax},{ay})"
        );
    }

    #[test]
    fn parse_transform_attr_scale_then_translate() {
        // "scale(2) translate(5,5)" composes f1·f2 → point (0,0) maps to (10,10).
        let m = to_transform(parse_transform_attr("scale(2) translate(5,5)"));
        let mut p = [tiny_skia::Point::from_xy(0.0, 0.0)];
        m.map_points(&mut p);
        assert!(
            (p[0].x - 10.0).abs() < 1e-3 && (p[0].y - 10.0).abs() < 1e-3,
            "got ({},{})",
            p[0].x,
            p[0].y
        );
    }

    #[test]
    fn parse_transform_attr_matrix_and_lenient() {
        assert!(approx6(
            parse_transform_attr("matrix(1,0,0,1,30,40)"),
            [1.0, 0.0, 0.0, 1.0, 30.0, 40.0],
            1e-4
        ));
        // empty / absent → identity.
        assert!(approx6(
            parse_transform_attr(""),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            1e-4
        ));
        // a leading good function then garbage → the good one still applies.
        assert!(approx6(
            parse_transform_attr("translate(5,5) bogus"),
            [1.0, 0.0, 0.0, 1.0, 5.0, 5.0],
            1e-4
        ));
    }

    #[test]
    fn svg_g_transform_composes_into_shape() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <g transform='translate(50,0)'>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></g></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        match shapes[0] {
            PaintCmd::SvgShape { transform, .. } => {
                // viewBox identity · translate(50,0).
                assert!(
                    approx6(*transform, [1.0, 0.0, 0.0, 1.0, 50.0, 0.0], 1e-4),
                    "{transform:?}"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_nested_g_transform_composes() {
        let cmds = list(
            "<html><body><svg width='200' height='200'>\
             <g transform='translate(50,0)'><g transform='translate(0,30)'>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></g></g></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape { transform, .. } => {
                assert!(
                    approx6(*transform, [1.0, 0.0, 0.0, 1.0, 50.0, 30.0], 1e-4),
                    "{transform:?}"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_g_fill_inherited_by_child() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <g fill='red'><rect x='0' y='0' width='10' height='10'/></g></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        match shapes[0] {
            PaintCmd::SvgShape { fill, .. } => assert_eq!(paint_color(fill), Some(red())),
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_child_overrides_g_fill() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <g fill='red'><rect x='0' y='0' width='10' height='10' fill='blue'/></g></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape { fill, .. } => assert_eq!(paint_color(fill), Some(blue())),
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_empty_g_is_transparent_passthrough() {
        // <g> with no transform/fill → child paints as if a direct svg child.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <g><rect x='5' y='5' width='10' height='10' fill='red'/></g></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape {
                transform, fill, ..
            } => {
                assert!(approx6(*transform, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 1e-4));
                assert_eq!(paint_color(fill), Some(red()));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_linear_gradient_registry_and_fill() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><linearGradient id='g'>\
             <stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/>\
             </linearGradient></defs>\
             <rect x='10' y='10' width='80' height='80' fill='url(#g)'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        match shapes[0] {
            PaintCmd::SvgShape {
                fill: Some(SvgPaint::Gradient(g)),
                bbox,
                ..
            } => {
                assert_eq!(g.stops.len(), 2);
                assert_eq!(g.stops[0].color, red());
                assert_eq!(g.stops[0].pos, Some(0.0));
                assert_eq!(g.stops[1].color, blue());
                assert_eq!(g.stops[1].pos, Some(1.0));
                assert!(matches!(g.kind, GradKind::Linear { .. }));
                assert_eq!(g.units, GradUnits::ObjectBoundingBox);
                // bbox = the rect's user rect.
                assert_eq!(bbox.x, 10.0);
                assert_eq!(bbox.width, 80.0);
            }
            _ => panic!("expected a gradient fill: {cmds:?}"),
        }
    }

    #[test]
    fn svg_gradient_user_space_on_use() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><linearGradient id='g' gradientUnits='userSpaceOnUse' x1='0' y1='0' x2='100' y2='0'>\
             <stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/>\
             </linearGradient></defs>\
             <rect x='10' y='10' width='80' height='80' fill='url(#g)'/></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape {
                fill: Some(SvgPaint::Gradient(g)),
                ..
            } => {
                assert_eq!(g.units, GradUnits::UserSpaceOnUse);
                assert!(
                    matches!(
                        g.kind,
                        GradKind::Linear {
                            x1: 0.0,
                            x2: 100.0,
                            ..
                        }
                    ),
                    "{:?}",
                    g.kind
                );
            }
            _ => panic!("expected a userSpaceOnUse gradient"),
        }
    }

    #[test]
    fn svg_radial_gradient_parsed() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><radialGradient id='r' cx='0.5' cy='0.5' r='0.5'>\
             <stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/>\
             </radialGradient></defs>\
             <rect x='0' y='0' width='100' height='100' fill='url(#r)'/></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape {
                fill: Some(SvgPaint::Gradient(g)),
                ..
            } => {
                assert!(
                    matches!(
                        g.kind,
                        GradKind::Radial {
                            cx: 0.5,
                            cy: 0.5,
                            r: 0.5
                        }
                    ),
                    "{:?}",
                    g.kind
                );
            }
            _ => panic!("expected a radial gradient"),
        }
    }

    #[test]
    fn svg_gradient_url_missing_emits_no_shape() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='0' y='0' width='10' height='10' fill='url(#nope)'/></svg></body></html>",
            "body{margin:0}",
        );
        // no fill (missing ref → None), no stroke → no command at all.
        assert!(
            svg_shapes(&cmds).is_empty(),
            "url(#missing) → no paint: {cmds:?}"
        );
    }

    #[test]
    fn svg_gradient_transform_composes_with_g() {
        // a gradient rect inside a transformed <g> keeps the gradient (the
        // effective transform carries it to canvas).
        let cmds = list(
            "<html><body><svg width='200' height='200'>\
             <defs><linearGradient id='g'>\
             <stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/>\
             </linearGradient></defs>\
             <g transform='rotate(30)'>\
             <rect x='0' y='0' width='50' height='50' fill='url(#g)'/></g></svg></body></html>",
            "body{margin:0}",
        );
        match svg_shapes(&cmds)[0] {
            PaintCmd::SvgShape {
                fill: Some(SvgPaint::Gradient(_)),
                transform,
                ..
            } => {
                // rotate(30) → non-identity off-diagonal.
                assert!(transform[1].abs() > 0.1, "rotated transform: {transform:?}");
            }
            _ => panic!("expected a gradient under a rotated g"),
        }
    }

    /// Collect the GlyphRun commands of a display list.
    fn glyph_runs(cmds: &[PaintCmd]) -> Vec<&PaintCmd> {
        cmds.iter()
            .filter(|c| matches!(c, PaintCmd::GlyphRun { .. }))
            .collect()
    }

    #[test]
    fn svg_text_emits_glyphrun_at_baseline() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <text x='10' y='30' fill='red' font-size='20'>Hi</text></svg></body></html>",
            "body{margin:0}",
        );
        // bracketed by PushTransform / PopTransform.
        let push = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushTransform { .. }));
        let pop = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopTransform));
        let gr = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        assert!(push.is_some() && pop.is_some() && gr.is_some(), "{cmds:?}");
        assert!(push.unwrap() < gr.unwrap() && gr.unwrap() < pop.unwrap());
        match glyph_runs(&cmds)[0] {
            PaintCmd::GlyphRun {
                origin,
                text,
                font_size,
                color,
                ascent,
                ..
            } => {
                assert_eq!(text, "Hi");
                assert_eq!(*color, red());
                assert_eq!(*font_size, 20.0);
                // origin.x = x, origin.y = y - ascent (baseline lands at y=30).
                assert!((origin.0 - 10.0).abs() < 1e-3, "x={}", origin.0);
                assert!(
                    (origin.1 - (30.0 - *ascent)).abs() < 1e-3,
                    "y={} ascent={}",
                    origin.1,
                    ascent
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_text_anchor_middle_shifts_left() {
        let start = list(
            "<html><body><svg width='200' height='100'>\
             <text x='100' y='30' font-size='20'>Hello</text></svg></body></html>",
            "body{margin:0}",
        );
        let middle = list(
            "<html><body><svg width='200' height='100'>\
             <text x='100' y='30' font-size='20' text-anchor='middle'>Hello</text></svg></body></html>",
            "body{margin:0}",
        );
        let sx = match glyph_runs(&start)[0] {
            PaintCmd::GlyphRun { origin, .. } => origin.0,
            _ => unreachable!(),
        };
        let mx = match glyph_runs(&middle)[0] {
            PaintCmd::GlyphRun { origin, .. } => origin.0,
            _ => unreachable!(),
        };
        // middle anchor shifts the run start left by half the measured width.
        assert!(mx < sx - 1.0, "middle {mx} should be left of start {sx}");
    }

    #[test]
    fn svg_tspan_inline_continuation() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <text x='10' y='20' font-size='20'>A<tspan>B</tspan></text></svg></body></html>",
            "body{margin:0}",
        );
        let runs = glyph_runs(&cmds);
        assert_eq!(runs.len(), 2, "two segments A and B: {cmds:?}");
        let (ax, atext) = match runs[0] {
            PaintCmd::GlyphRun { origin, text, .. } => (origin.0, text.clone()),
            _ => unreachable!(),
        };
        let (bx, btext) = match runs[1] {
            PaintCmd::GlyphRun { origin, text, .. } => (origin.0, text.clone()),
            _ => unreachable!(),
        };
        assert_eq!(atext, "A");
        assert_eq!(btext, "B");
        // B continues the pen from after A (so its x is strictly greater).
        assert!(bx > ax, "B at {bx} should follow A at {ax}");
        assert!((ax - 10.0).abs() < 1e-3, "A starts at x=10, got {ax}");
    }

    #[test]
    fn svg_tspan_absolute_x_repositions() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <text x='10' y='20' font-size='20'>A<tspan x='50'>B</tspan></text></svg></body></html>",
            "body{margin:0}",
        );
        let runs = glyph_runs(&cmds);
        let bx = match runs[1] {
            PaintCmd::GlyphRun { origin, .. } => origin.0,
            _ => unreachable!(),
        };
        assert!((bx - 50.0).abs() < 1e-3, "tspan x=50 repositions, got {bx}");
    }

    #[test]
    fn svg_text_gradient_falls_back_to_first_stop() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <defs><linearGradient id='g'>\
             <stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/>\
             </linearGradient></defs>\
             <text x='10' y='30' font-size='20' fill='url(#g)'>Hi</text></svg></body></html>",
            "body{margin:0}",
        );
        match glyph_runs(&cmds)[0] {
            PaintCmd::GlyphRun { color, .. } => {
                assert_eq!(*color, red(), "gradient text → first stop")
            }
            _ => unreachable!(),
        }
    }

    // --- E13-M4: border-style dashed/dotted/double + overflow clip ---

    #[test]
    fn dashed_border_emits_strokeline_not_fillrect() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;border:4px dashed #0000ff}",
        );
        // A dashed border emits StrokeLine{style:Dashed}, never a border FillRect.
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::StrokeLine { style: BorderStyle::Dashed, color, .. }
                    if color.b == 255 && color.r == 0
            )),
            "expected a dashed StrokeLine: {cmds:?}"
        );
        // No blue border fill rects (only the StrokeLine carries the blue edge).
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0
            )),
            "dashed border must not emit a FillRect: {cmds:?}"
        );
    }

    #[test]
    fn double_border_emits_strokeline_per_edge() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;border:6px double #0000ff}",
        );
        // One StrokeLine per edge (4 edges); raster splits each into two strokes.
        let n = cmds
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    PaintCmd::StrokeLine {
                        style: BorderStyle::Double,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            n, 4,
            "expected 4 double StrokeLines (one per edge): {cmds:?}"
        );
    }

    #[test]
    fn solid_border_still_emits_fillrect_no_strokeline() {
        // Regression guard: solid borders keep the FillRect path, no StrokeLine.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;border:4px solid #0000ff}",
        );
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0
            )),
            "solid border must still emit FillRect: {cmds:?}"
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::StrokeLine { .. })),
            "solid border must not emit StrokeLine: {cmds:?}"
        );
    }

    #[test]
    fn overflow_hidden_emits_pushclip_bracketing_children() {
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:hidden}",
        );
        let push = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushClip { .. }));
        let pop = cmds.iter().position(|c| matches!(c, PaintCmd::PopClip));
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (push, pop, glyph) = (
            push.expect("PushClip"),
            pop.expect("PopClip"),
            glyph.expect("glyph"),
        );
        assert!(
            push < glyph && glyph < pop,
            "child glyph inside clip bracket"
        );
        // The clip rect is the padding box (= border box here, no border/padding).
        if let PaintCmd::PushClip { rect, .. } = &cmds[push] {
            assert_eq!((rect.width, rect.height), (100.0, 50.0));
        }
    }

    #[test]
    fn overflow_visible_emits_no_pushclip() {
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:visible}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })),
            "overflow:visible must not clip: {cmds:?}"
        );
    }

    // --- E14-M1: native text form controls ---

    /// Find a GlyphRun with exactly `t` and return its color.
    fn glyph_color(cmds: &[PaintCmd], t: &str) -> Option<Rgba> {
        cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { text, color, .. } if text == t => Some(*color),
            _ => None,
        })
    }

    #[test]
    fn input_value_glyph_with_border_and_clip() {
        let cmds = list(
            "<html><body><input value='hi'></body></html>",
            "body{margin:0}",
        );
        // The displayed value.
        assert!(
            glyph_color(&cmds, "hi").is_some(),
            "expected 'hi' glyph: {cmds:?}"
        );
        // A border FillRect in the UA border color #767676.
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { color, .. }
                    if color.r == 0x76 && color.g == 0x76 && color.b == 0x76
            )),
            "expected a #767676 border fill: {cmds:?}"
        );
        // A PushClip/PopClip pair brackets the text.
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })));
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopClip)));
    }

    #[test]
    fn input_placeholder_is_grey() {
        let cmds = list(
            "<html><body><input placeholder='name'></body></html>",
            "body{margin:0}",
        );
        let grey = Rgba {
            r: 0x75,
            g: 0x75,
            b: 0x75,
            a: 255,
        };
        assert_eq!(
            glyph_color(&cmds, "name"),
            Some(grey),
            "placeholder grey: {cmds:?}"
        );
    }

    #[test]
    fn password_masks_value() {
        let cmds = list(
            "<html><body><input type='password' value='abc'></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&cmds, "\u{2022}\u{2022}\u{2022}").is_some(),
            "expected masked bullets: {cmds:?}"
        );
    }

    #[test]
    fn textarea_shows_text_content() {
        let cmds = list(
            "<html><body><textarea>hello</textarea></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&cmds, "hello").is_some(),
            "expected 'hello' glyph: {cmds:?}"
        );
    }

    #[test]
    fn button_label_with_grey_bg() {
        let cmds = list(
            "<html><body><button>Go</button></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&cmds, "Go").is_some(),
            "expected 'Go' glyph: {cmds:?}"
        );
        // UA button background #e9e9ed.
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { color, .. }
                    if color.r == 0xe9 && color.g == 0xe9 && color.b == 0xed
            )),
            "expected a #e9e9ed button bg: {cmds:?}"
        );
    }

    #[test]
    fn submit_and_reset_default_labels() {
        let s = list(
            "<html><body><input type='submit'></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&s, "Submit").is_some(),
            "expected 'Submit': {s:?}"
        );
        let r = list(
            "<html><body><input type='reset'></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&r, "Reset").is_some(),
            "expected 'Reset': {r:?}"
        );
    }

    #[test]
    fn non_form_page_emits_no_form_clip() {
        // Sanity: a plain paragraph never triggers the form-control paint path.
        let cmds = list("<html><body><p>hi</p></body></html>", "body{margin:0}");
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })),
            "plain page must not push a form clip: {cmds:?}"
        );
    }

    // --- E14-M2: native choice form controls ---

    /// True if the list has a stroked Path SvgShape (the checkbox tick).
    fn has_stroked_path(cmds: &[PaintCmd]) -> bool {
        cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::SvgShape {
                    geom: SvgGeom::Path(_),
                    stroke: Some(_),
                    ..
                }
            )
        })
    }

    /// True if the list has a filled-color Path SvgShape (the select arrow).
    fn has_filled_path(cmds: &[PaintCmd]) -> bool {
        cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::SvgShape {
                    geom: SvgGeom::Path(_),
                    fill: Some(SvgPaint::Color(_)),
                    ..
                }
            )
        })
    }

    #[test]
    fn checkbox_checked_draws_tick_unchecked_does_not() {
        let unchecked = list(
            "<html><body><input type='checkbox'></body></html>",
            "body{margin:0}",
        );
        // The box outline (a rect with the #767676 stroke) is always present.
        assert!(
            unchecked.iter().any(|c| matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Rect { .. }, stroke: Some(SvgPaint::Color(s)), .. }
                    if s.r == 0x76 && s.g == 0x76 && s.b == 0x76
            )),
            "checkbox box outline: {unchecked:?}"
        );
        assert!(
            !has_stroked_path(&unchecked),
            "unchecked checkbox has no tick: {unchecked:?}"
        );

        let checked = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0}",
        );
        assert!(
            has_stroked_path(&checked),
            "checked checkbox draws a tick: {checked:?}"
        );
    }

    #[test]
    fn radio_checked_draws_filled_dot() {
        let unchecked = list(
            "<html><body><input type='radio'></body></html>",
            "body{margin:0}",
        );
        // outline circle, no filled centre dot.
        let filled_dots = |cmds: &[PaintCmd]| {
            cmds.iter()
                .filter(|c| {
                    matches!(
                        c,
                        PaintCmd::SvgShape {
                            geom: SvgGeom::Ellipse { .. },
                            fill: Some(SvgPaint::Color(f)),
                            stroke: None,
                            ..
                        } if f.r == 0x33 && f.g == 0x33 && f.b == 0x33
                    )
                })
                .count()
        };
        assert_eq!(
            filled_dots(&unchecked),
            0,
            "unchecked radio has no dot: {unchecked:?}"
        );
        let checked = list(
            "<html><body><input type='radio' checked></body></html>",
            "body{margin:0}",
        );
        assert_eq!(
            filled_dots(&checked),
            1,
            "checked radio has one filled dot: {checked:?}"
        );
    }

    #[test]
    fn select_shows_selected_text_and_arrow() {
        let cmds = list(
            "<html><body><select><option>A<option selected>Banana</select></body></html>",
            "body{margin:0}",
        );
        // selected option text.
        assert!(
            glyph_color(&cmds, "Banana").is_some(),
            "expected 'Banana' glyph: {cmds:?}"
        );
        // the unselected option is NOT a separate glyph run.
        assert!(
            glyph_color(&cmds, "A").is_none(),
            "non-selected option must not render: {cmds:?}"
        );
        // the dropdown-arrow triangle (filled Path).
        assert!(
            has_filled_path(&cmds),
            "expected an arrow triangle Path: {cmds:?}"
        );
    }

    #[test]
    fn select_empty_emits_arrow_only() {
        let cmds = list(
            "<html><body><select></select></body></html>",
            "body{margin:0}",
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { .. })));
        assert!(
            has_filled_path(&cmds),
            "empty select still draws the arrow: {cmds:?}"
        );
    }

    // --- E14-M3 color / range / hidden ---

    #[test]
    fn color_swatch_fills_value_color() {
        let cmds = list(
            "<html><body><input type='color' value='#ff0000'></body></html>",
            "body{margin:0}",
        );
        // The inner swatch is a filled Rect with no stroke, in the value colour.
        let swatch = svg_shapes(&cmds).into_iter().find(|c| {
            matches!(
                c,
                PaintCmd::SvgShape {
                    geom: SvgGeom::Rect { .. },
                    fill: Some(SvgPaint::Color(f)),
                    stroke: None,
                    ..
                } if *f == red()
            )
        });
        assert!(swatch.is_some(), "expected a red swatch rect: {cmds:?}");
    }

    #[test]
    fn color_swatch_defaults_black_when_no_value() {
        let cmds = list(
            "<html><body><input type='color'></body></html>",
            "body{margin:0}",
        );
        let black = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let swatch = svg_shapes(&cmds).into_iter().find(|c| {
            matches!(
                c,
                PaintCmd::SvgShape {
                    geom: SvgGeom::Rect { .. },
                    fill: Some(SvgPaint::Color(f)),
                    stroke: None,
                    ..
                } if *f == black
            )
        });
        assert!(
            swatch.is_some(),
            "expected a black default swatch: {cmds:?}"
        );
    }

    /// The thumb's centre-x of a range slider (the stroked ellipse).
    fn range_thumb_cx(cmds: &[PaintCmd]) -> Option<f32> {
        cmds.iter().find_map(|c| match c {
            PaintCmd::SvgShape {
                geom: SvgGeom::Ellipse { cx, .. },
                stroke: Some(_),
                ..
            } => Some(*cx),
            _ => None,
        })
    }

    #[test]
    fn range_thumb_position_tracks_value() {
        let left = list(
            "<html><body><input type='range' min='0' max='100' value='0'></body></html>",
            "body{margin:0}",
        );
        let mid = list(
            "<html><body><input type='range' min='0' max='100' value='50'></body></html>",
            "body{margin:0}",
        );
        let right = list(
            "<html><body><input type='range' min='0' max='100' value='100'></body></html>",
            "body{margin:0}",
        );
        let (l, m, r) = (
            range_thumb_cx(&left).expect("left thumb"),
            range_thumb_cx(&mid).expect("mid thumb"),
            range_thumb_cx(&right).expect("right thumb"),
        );
        assert!(
            l < m && m < r,
            "thumb moves rightward with value: {l} {m} {r}"
        );
    }

    #[test]
    fn hidden_input_emits_no_control() {
        let cmds = list(
            "<html><body><input type='hidden'></body></html>",
            "body{margin:0}",
        );
        // display:none → no form-control box, glyph, or shape from the input.
        assert!(
            svg_shapes(&cmds).is_empty(),
            "hidden input draws no shapes: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { .. })),
            "hidden input draws no glyph: {cmds:?}"
        );
    }

    // --- E16-M2: background-image layers (url / size / position / repeat) ---

    /// Build a display list for `html`+`css` after writing an `iw`×`ih` image
    /// named `bg.png` into the doc dir and decoding the styled bg url layers
    /// (mirroring the render_document pre-pass via `bg_url_srcs`).
    fn bg_list(html: &str, css: &str, iw: u32, ih: u32) -> Vec<PaintCmd> {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-bg-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::from_pixel(iw, ih, image::Rgba([0, 0, 255, 255]));
        img.save(dir.join("bg.png")).unwrap();

        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        // Decode the bg url layers of every element (no pseudos here).
        let mut stack = vec![doc.root()];
        while let Some(id) = stack.pop() {
            if let Some(s) = styled.get(id) {
                for l in &s.background_layers {
                    if let BgImage::Url(src) = &l.image {
                        images.get(src);
                    }
                }
            }
            for c in doc.children(id) {
                stack.push(c);
            }
        }
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    fn blits(cmds: &[PaintCmd]) -> Vec<Rect> {
        cmds.iter()
            .filter_map(|c| match c {
                PaintCmd::ImageBlit { dest, .. } => Some(*dest),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn url_bg_renders_one_blit_no_repeat() {
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:80px;\
             background-image:url(bg.png);background-repeat:no-repeat}",
            10,
            10,
        );
        let bs = blits(&cmds);
        assert_eq!(bs.len(), 1, "no-repeat → one tile: {cmds:?}");
        // Default size auto → 10×10 intrinsic; default position 0% 0% → top-left.
        assert_eq!(
            bs[0],
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0
            }
        );
        // The blit is clipped to the box.
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })));
    }

    #[test]
    fn bg_size_cover_and_contain() {
        // Box 100×40, image 10×20 (aspect 1:2).
        let cover = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:40px;\
             background-image:url(bg.png);background-repeat:no-repeat;background-size:cover}",
            10,
            20,
        );
        // cover = max(100/10, 40/20) = 10 → 100×200.
        assert_eq!(blits(&cover)[0].width, 100.0);
        assert_eq!(blits(&cover)[0].height, 200.0);
        let contain = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:40px;\
             background-image:url(bg.png);background-repeat:no-repeat;background-size:contain}",
            10,
            20,
        );
        // contain = min(100/10, 40/20) = 2 → 20×40.
        assert_eq!(blits(&contain)[0].width, 20.0);
        assert_eq!(blits(&contain)[0].height, 40.0);
    }

    #[test]
    fn bg_position_shifts_origin() {
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:100px;\
             background-image:url(bg.png);background-repeat:no-repeat;\
             background-position:100% 100%}",
            10,
            10,
        );
        // 100% of (box-tile) free space: x=0+1.0*(100-10)=90, y likewise.
        assert_eq!(
            blits(&cmds)[0],
            Rect {
                x: 90.0,
                y: 90.0,
                width: 10.0,
                height: 10.0
            }
        );
    }

    #[test]
    fn bg_repeat_tiles_across_box() {
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;\
             background-image:url(bg.png);background-repeat:repeat}",
            10,
            10,
        );
        // 50/10 = 5 tiles per axis → 25 blits.
        assert_eq!(blits(&cmds).len(), 25, "5x5 tiling: {cmds:?}");
    }

    #[test]
    fn bg_repeat_x_only() {
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;\
             background-image:url(bg.png);background-repeat:repeat-x}",
            10,
            10,
        );
        let bs = blits(&cmds);
        assert_eq!(bs.len(), 5, "repeat-x → one row: {cmds:?}");
        assert!(bs.iter().all(|r| r.y == 0.0));
    }

    #[test]
    fn multiple_layers_url_over_gradient_command_order() {
        // Two layers: a url (top) over a gradient (bottom), plus a bg color.
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:40px;height:40px;\
             background-color:#ff0000;\
             background-image:url(bg.png),linear-gradient(red,blue);\
             background-repeat:no-repeat}",
            10,
            10,
        );
        let fill = cmds.iter().position(
            |c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0),
        );
        let grad = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GradientRect { .. }));
        let blit = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::ImageBlit { .. }));
        let (fill, grad, blit) = (
            fill.expect("fill"),
            grad.expect("grad"),
            blit.expect("blit"),
        );
        // color first (bottom), then the gradient layer (bottom image), then the
        // url layer on top.
        assert!(fill < grad, "color before gradient");
        assert!(grad < blit, "gradient before url blit");
    }

    #[test]
    fn single_color_div_still_one_fillrect_byte_identical() {
        // A page with NO image layers must produce the SAME command sequence as
        // before E16-M2: exactly one FillRect for an opaque color, nothing else
        // background-related (an empty div paints its bg once).
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;background:#00ff00}",
            10,
            10,
        );
        let fills: Vec<_> = cmds
            .iter()
            .filter(
                |c| matches!(c, PaintCmd::FillRect { color, .. } if color.g == 255 && color.r == 0),
            )
            .collect();
        assert_eq!(fills.len(), 1);
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })));
    }

    #[test]
    fn single_gradient_div_still_one_gradientrect_byte_identical() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;\
             background:linear-gradient(to right,#ff0000,#0000ff)}",
        );
        let grads: Vec<_> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::GradientRect { .. }))
            .collect();
        assert_eq!(grads.len(), 1);
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::FillRect { .. })));
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
    }

    #[test]
    fn broken_bg_url_emits_no_blit_no_panic() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:50px;height:50px;background-image:url(nope.png)}",
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
    }

    #[test]
    fn huge_box_tiny_tile_is_capped() {
        // A 1px image repeated in a giant box must not hang / explode: the tile
        // count is capped at MAX_BG_TILES_PER_AXIS per axis.
        let cmds = bg_list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:20000px;height:20000px;\
             background-image:url(bg.png);background-repeat:repeat}",
            1,
            1,
        );
        let n = blits(&cmds).len();
        assert!(n <= MAX_BG_TILES_PER_AXIS * MAX_BG_TILES_PER_AXIS);
        assert_eq!(
            n,
            MAX_BG_TILES_PER_AXIS * MAX_BG_TILES_PER_AXIS,
            "capped both axes"
        );
    }

    // --- E16-M3: radial/conic gradients, text-shadow, outline ---

    #[test]
    fn radial_gradient_bg_emits_radial_rect() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;\
             background:radial-gradient(red, blue)}",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::RadialRect { .. })),
            "expected a RadialRect: {cmds:?}"
        );
    }

    #[test]
    fn conic_gradient_bg_emits_conic_rect() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;\
             background:conic-gradient(red, blue)}",
        );
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::ConicRect { .. })),
            "expected a ConicRect: {cmds:?}"
        );
    }

    #[test]
    fn text_shadow_emits_glyph_shadow_before_glyph_run() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{text-shadow:2px 2px #0000ff}",
        );
        let shadow = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphShadow { .. }));
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (s, g) = (shadow.expect("shadow"), glyph.expect("glyph"));
        assert!(s < g, "shadow {s} before glyph {g}");
    }

    #[test]
    fn outline_emits_outline_cmds() {
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:40px;height:40px;outline:3px solid #ff0000}",
        );
        // Solid outline → four red FillRects forming the frame outside the box.
        let reds = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.g == 0 && color.b == 0))
            .count();
        assert_eq!(reds, 4, "expected four outline fill rects: {cmds:?}");
    }

    #[test]
    fn no_feature_page_emits_no_new_cmds() {
        // Byte-identity sentinel: a page using none of the M3 features must emit
        // no Radial/Conic/GlyphShadow/outline commands.
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{background:#ff0000;border:2px solid #0000ff}",
        );
        assert!(!cmds.iter().any(|c| matches!(
            c,
            PaintCmd::RadialRect { .. } | PaintCmd::ConicRect { .. } | PaintCmd::GlyphShadow { .. }
        )));
        // A single linear gradient still → exactly one GradientRect (unchanged).
        let grad = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:80px;height:40px;background:linear-gradient(red, blue)}",
        );
        assert_eq!(
            grad.iter()
                .filter(|c| matches!(c, PaintCmd::GradientRect { .. }))
                .count(),
            1,
            "single linear gradient → one GradientRect"
        );
    }

    #[test]
    fn multicol_rule_emits_stroke_line_in_gap() {
        // column-count:2, gap:20, width:220 → col_w=100; the single inter-column
        // gap center sits at x = 0 + 1*100 + 0.5*20 = 110.
        let cmds = list(
            "<html><body><div id='mc'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} \
             #mc{margin:0;width:220px;column-count:2;column-gap:20px;\
             column-rule:2px solid #000} \
             #mc>div{margin:0;height:30px}",
        );
        let rule = cmds.iter().find(|c| {
            matches!(
                c,
                PaintCmd::StrokeLine { from, width, style, .. }
                    if from.0 == 110.0 && *width == 2.0 && *style == BorderStyle::Solid
            )
        });
        assert!(
            rule.is_some(),
            "expected a column rule StrokeLine: {cmds:?}"
        );
    }

    #[test]
    fn multicol_without_rule_emits_no_stroke_line() {
        let cmds = list(
            "<html><body><div id='mc'>\
             <div id='a'>a</div><div id='b'>b</div></div></body></html>",
            "body{margin:0} \
             #mc{margin:0;width:220px;column-count:2;column-gap:20px} \
             #mc>div{margin:0;height:30px}",
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::StrokeLine { .. })),
            "no column-rule → no StrokeLine: {cmds:?}"
        );
    }

    // --- E18-M3: vertical writing modes ---

    #[test]
    fn vertical_sideways_text_wraps_glyphrun_in_transform() {
        // A sideways/mixed vertical run is rotated: a PushTransform must directly
        // precede the GlyphRun, with a matching PopTransform after.
        let cmds = list(
            "<html><body><div id='v'>hi</div></body></html>",
            "body{margin:0} #v{margin:0;height:200px;writing-mode:vertical-rl}",
        );
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "hi"))
            .expect("glyph run");
        // The command immediately before the glyph is a PushTransform.
        assert!(
            matches!(cmds[glyph - 1], PaintCmd::PushTransform { .. }),
            "PushTransform should wrap the rotated run: {cmds:?}"
        );
        // A PopTransform follows the glyph run.
        assert!(
            cmds[glyph + 1..]
                .iter()
                .any(|c| matches!(c, PaintCmd::PopTransform)),
            "PopTransform should close the rotated run: {cmds:?}"
        );
    }

    #[test]
    fn vertical_upright_stacks_per_char_glyphruns() {
        // text-orientation:upright emits one GlyphRun per char, same x, increasing
        // y. With "AB" we expect two single-char runs.
        let cmds = list(
            "<html><body><div id='v'>AB</div></body></html>",
            "body{margin:0} \
             #v{margin:0;height:200px;writing-mode:vertical-rl;text-orientation:upright;font-size:20px}",
        );
        let runs: Vec<(f32, f32, String)> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::GlyphRun { origin, text, .. } => Some((origin.0, origin.1, text.clone())),
                _ => None,
            })
            .collect();
        // two single-char runs.
        assert_eq!(runs.len(), 2, "per-char runs: {runs:?}");
        assert_eq!(runs[0].2, "A");
        assert_eq!(runs[1].2, "B");
        // same x column, advancing y.
        assert_eq!(runs[0].0, runs[1].0, "upright chars share the column x");
        assert!(
            runs[1].1 > runs[0].1,
            "upright chars advance down Y: {} then {}",
            runs[0].1,
            runs[1].1
        );
        // No rotation transform for upright.
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::PushTransform { .. })),
            "upright should not rotate: {cmds:?}"
        );
    }

    #[test]
    fn horizontal_plain_text_has_no_transform() {
        // Byte-identity guard: a default (horizontal-tb) page emits the glyph run
        // with no surrounding PushTransform.
        let cmds = list("<html><body><div>hi</div></body></html>", "body{margin:0}");
        let glyph = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "hi"))
            .expect("glyph run");
        assert!(glyph == 0 || !matches!(cmds[glyph - 1], PaintCmd::PushTransform { .. }));
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::PushTransform { .. })),
            "plain horizontal text has no transform: {cmds:?}"
        );
    }

    // --- E21-M3: mask-image + backdrop-filter ---

    #[test]
    fn mask_forces_push_layer_with_mask() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{mask-image:linear-gradient(black,rgba(0,0,0,0));\
             background:#ff0000;width:50px;height:50px}",
        );
        let pushed = cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::PushLayer { mask: Some(_), .. }));
        assert!(pushed, "mask must force a PushLayer{{mask:Some}}: {cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopLayer)));
    }

    #[test]
    fn mask_box_carries_distinct_geometry_boxes() {
        // With non-zero padding + border the three geometry boxes differ; the
        // emitted MaskBox must carry all three (E32-M2).
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{mask-image:linear-gradient(black,rgba(0,0,0,0));\
             padding:10px;border:5px solid #000;width:50px;height:50px}",
        );
        let mb = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::PushLayer { mask: Some(mb), .. } => Some(mb),
                _ => None,
            })
            .expect("PushLayer with mask");
        assert_ne!(mb.rect, mb.padding_box, "border box != padding box");
        assert_ne!(mb.padding_box, mb.content_box, "padding box != content box");
        // border box contains the padding box contains the content box.
        assert!(mb.rect.width > mb.padding_box.width);
        assert!(mb.padding_box.width > mb.content_box.width);
    }

    #[test]
    fn no_mask_no_push_layer() {
        // Byte-identity sentinel: no mask + no other layer trigger → no PushLayer.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:50px;height:50px}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })),
            "no mask must not push a layer: {cmds:?}"
        );
    }

    #[test]
    fn backdrop_filter_emits_apply_before_push() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{backdrop-filter:blur(3px);width:50px;height:50px}",
        );
        let apply = cmds
            .iter()
            .position(
                |c| matches!(c, PaintCmd::ApplyBackdropFilter { filter, .. } if !filter.is_empty()),
            )
            .expect("ApplyBackdropFilter");
        // The backdrop-filter must precede the box's own layer push so it filters
        // the parent backdrop, not the box's fresh (empty) layer.
        if let Some(push) = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushLayer { .. }))
        {
            assert!(
                apply < push,
                "ApplyBackdropFilter {apply} before PushLayer {push}"
            );
        }
    }

    #[test]
    fn no_backdrop_filter_no_apply() {
        // Byte-identity sentinel: empty backdrop-filter emits no ApplyBackdropFilter.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:50px;height:50px}",
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::ApplyBackdropFilter { .. })),
            "no backdrop-filter must not emit ApplyBackdropFilter: {cmds:?}"
        );
    }
}
