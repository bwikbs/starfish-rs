//! The flat display list (M5 §3): a pre-order walk of the box tree turning each
//! box into background/border fill-rects and each text run into a glyph run, in
//! correct paint order (parent before child; bg → border → text).

use starfish_dom::{CanvasImageSrc, CanvasOp, Document, NodeId};
use starfish_layout::{
    control_label, form_control_kind, input_display, parse_view_box, range_fraction, range_values,
    selected_option_text, textarea_value, BoxKind, BoxStyleRef, FontQuery, FormControl, LayoutBox,
    Rect, TextFlavor, ViewBox,
};
use starfish_style::{
    BackgroundLayer, BgAttachment, BgGeometryBox, BgImage, BgSize, BgSizeAxis, BlendMode,
    BorderImageRepeat, BorderImageWidth, BorderStyle, BoxShadow,
    ClipShape,
    ComputedStyle, ConicGradient, EmphasisMark, EmphasisShape, FilterFn, Float, FontKerning,
    FontStyle, FontWeight, ImageRendering,
    Isolation, Length,
    LengthPct,
    LinearGradient, ObjectFit, Outline, Overflow, Position, PseudoElement, RadialGradient, Rgba,
    ScrollbarWidth, StyledTree, TextDecorationLine, TextDecorationStyle, TextOrientation,
    TransformFn,
};
use tiny_skia::Transform;

use crate::font::FontDb;
use crate::image_store::ImageStore;

// Re-export the mask types so the rasterizer can refer to them via `crate::display`.
pub use starfish_style::{MaskGeometryBox, MaskImage, MaskMode, MaskSpec};

/// A resolved mask box (E21-M3; E47-M3 multi-layer): the computed `mask` layers
/// plus the box geometry the mask sources are rendered against (the border box +
/// its corner radii). The layers' combined coverage (E47-M3: union/MAX) multiplies
/// the offscreen layer's alpha on pop. `padding_box` and `content_box` (E32-M2)
/// let the rasterizer resolve `mask-origin`/`mask-clip` per layer.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskBox {
    pub specs: Vec<MaskSpec>,
    pub rect: Rect, // border box
    pub padding_box: Rect,
    pub content_box: Rect,
    pub radius: [f32; 4],
}

/// E47-M2: one text run's glyph parameters, used to build a glyph coverage mask
/// for `background-clip: text`. The same shaping inputs as a `GlyphRun`/
/// `GlyphShadow` (minus paint color) — the rasterizer re-shapes the run and
/// accumulates each glyph's coverage into the clip mask.
#[derive(Debug, Clone, PartialEq)]
pub struct TextClipGlyph {
    pub origin: (f32, f32),
    pub text: String,
    pub font_size: f32,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub family: Vec<String>,
    pub ascent: f32,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub features: Vec<([u8; 4], u32)>,
    pub kerning: FontKerning,
    pub variations: Vec<([u8; 4], f32)>,
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
        /// E46-M1 `font-feature-settings`: (tag, value) pairs threaded into the
        /// painter's `shape()` so paint reproduces the measured run; empty = none.
        features: Vec<([u8; 4], u32)>,
        /// E46-M1 `font-kerning`: `None` disables `kern`.
        kerning: FontKerning,
        /// E46-M3 `font-variation-settings`: variable-font (axis, coord) pairs
        /// set on the face before shaping; empty = none.
        variations: Vec<([u8; 4], f32)>,
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
        /// E46-M1: see `GlyphRun`.
        features: Vec<([u8; 4], u32)>,
        /// E46-M1: see `GlyphRun`.
        kerning: FontKerning,
        /// E46-M3: see `GlyphRun`.
        variations: Vec<([u8; 4], f32)>,
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
    /// E47-M2: begin a `background-clip: text` layer. The bracketed background
    /// commands are drawn into an offscreen layer; on `PopTextClip` the layer's
    /// alpha is multiplied by the union of `glyphs`' coverage (true glyph shapes),
    /// so the background only shows through the element's text. Empty `glyphs`
    /// (no text descendants) masks everything away (nothing paints).
    PushTextClip { glyphs: Vec<TextClipGlyph> },
    /// Composite the current text-clip layer, masked by its glyph coverage.
    PopTextClip,
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
    /// E38-M3: begin an SVG `clip-path="url(#cp)"` region: the referencing
    /// element (shape or `<g>` subtree) painted until the matching `PopClip` is
    /// clipped to the UNION of `geoms` (each a clipPath child shape's user-space
    /// geometry + its device transform `[a,b,c,d,e,f]`). Closed by `PopClip`.
    /// `clipPathUnits=userSpaceOnUse` only (MVP); union = non-zero fill of all
    /// child paths into one mask.
    PushSvgClip { geoms: Vec<(SvgGeom, [f32; 6])> },
    /// E55-M3: begin an SVG `mask="url(#m)"` region. The referencing element
    /// (shape or subtree) is painted into an offscreen layer until the matching
    /// `PopLayer`; on pop, the layer's premultiplied bytes are multiplied by the
    /// luminance × alpha coverage of `mask_cmds` (the `<mask>`'s children rendered
    /// into a scratch pixmap). A white mask shape → fully visible; black/absent →
    /// hidden. This is a TRUE luminance mask (not a clip-to-shapes approximation).
    PushSvgMask { mask_cmds: Vec<PaintCmd> },
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
    // E45-M3: a backface-visibility:hidden box whose effective transform flips it
    // away from the viewer (det < 0) is not painted — skip the box + its subtree.
    if backface_culled(b, styled) {
        return;
    }
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
    // E37-M2: a scroll box's clipped content is translated by (-scrollLeft,
    // -scrollTop) INSIDE the content clip (so scrolled content shifts up/left and
    // is clipped to the box). Zero offset → no transform (byte-identical to M1).
    let scroll = scroll_offset_of(b, styled, doc);
    if let Some((sx, sy)) = scroll {
        out.push(PaintCmd::PushTransform {
            matrix: [1.0, 0.0, 0.0, 1.0, -sx, -sy],
        });
    }
    // E54-M3: CSS 2.1 stacking order. The in-flow + float content must paint
    // BETWEEN the negative-z positioned descendants (behind) and the
    // zero/auto/positive-z ones (in front). `collect_inflow` both paints in-flow
    // content and fills the float/positioned buckets in one traversal, so paint
    // in-flow + floats into a scratch buffer; that lets the negative-z entries
    // (known only after the buckets are filled + flattened + sorted) be emitted
    // into `out` FIRST, then the scratch, then the non-negative entries.
    let mut inflow: Vec<PaintCmd> = Vec::new();
    for child in b.children() {
        collect_inflow(
            child,
            styled,
            fonts,
            images,
            doc,
            &mut inflow,
            &mut floats,
            &mut positioned,
        );
    }
    for f in floats {
        paint_subtree(f, styled, fonts, images, doc, &mut inflow);
    }
    // E54-M1: paint the positioned bucket in z-index order (auto/0 sort as 0).
    // E54-M2: a positioned box with `z-index: auto` and no other context trigger
    // does NOT establish a stacking context — its positioned descendants bubble up
    // into THIS context's bucket (so they interleave by z-index with this level's
    // positioned boxes), rather than being confined. Flatten through such boxes
    // first, recording tree order, then sort once.
    let mut entries: Vec<PositionedEntry> = Vec::new();
    for p in positioned {
        flatten_positioned(p, styled, &mut entries);
    }
    // Stable sort by z-index → ties + autos keep tree (collection) order, so a
    // default page (no nested positioned-in-positioned) is byte-identical to M1.
    entries.sort_by_key(|e| e.z);
    // E54-M3: partition the sorted entries by sign. Negatives paint behind the
    // box's in-flow content; non-negatives in front. With no negative-z boxes the
    // partition point is 0 → `negative` is empty and the output is byte-identical
    // to E54-M2 (negatives emitted, then in-flow scratch, then non-negatives).
    let split = entries.partition_point(|e| e.z < 0);
    let (negative, non_negative) = entries.split_at(split);
    for e in negative {
        paint_entry(e, styled, fonts, images, doc, out);
    }
    out.extend(inflow);
    for e in non_negative {
        paint_entry(e, styled, fonts, images, doc, out);
    }
    if scroll.is_some() {
        out.push(PaintCmd::PopTransform);
    }
    if clip.is_some() {
        out.push(PaintCmd::PopClip);
    }
    // E37-M1: overlay scrollbar (overflow:scroll/auto) — painted ON TOP of the
    // box's content (after PopClip so it is NOT clipped, and outside the M2
    // content transform), still inside any clip-path/layer/transform bracket.
    if let Some(cmds) = scrollbar_of(b, styled, doc) {
        out.extend(cmds);
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

// E54-M3: paint one sorted positioned entry within the current stacking context.
// A confined entry establishes its own context (paint the whole subtree); an
// auto-positioned one paints only its own content here (its positioned
// descendants were already flattened into the entry list).
fn paint_entry(
    e: &PositionedEntry,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    if e.confined {
        paint_subtree(e.b, styled, fonts, images, doc, out);
    } else {
        paint_positioned_noncontext(e.b, styled, fonts, images, doc, out);
    }
}

// E54-M2: a box establishes a stacking context when it is positioned with an
// explicit `z-index` (not `auto`), OR it has any compositing trigger that already
// forces an offscreen group: opacity < 1, a non-empty filter / transform, a mask,
// `isolation: isolate`, or `mix-blend-mode != normal`. The compositing triggers
// reuse the exact predicates `layer_effect`/`layer_transform` use, so a box that
// already pushes a layer is treated as context-establishing for descendant
// positioned sorting too. A positioned box with `z-index: auto` and no compositing
// trigger does NOT establish a context (its positioned descendants bubble up).
fn establishes_stacking_context(b: &LayoutBox, styled: &StyledTree) -> bool {
    let Some(s) = b.style(styled) else {
        return false;
    };
    if s.position != Position::Static && s.z_index.is_some() {
        return true;
    }
    layer_effect(b, styled).is_some() || layer_transform(b, styled).is_some()
}

// E54-M2: one positioned box to paint within the current stacking context, with
// its z-index (auto/0 → 0) and whether it is confined (establishes a context).
struct PositionedEntry<'a> {
    b: &'a LayoutBox,
    z: i32,
    confined: bool,
}

// E54-M2: flatten a positioned subtree root `p` into paint entries for the CURRENT
// stacking context. If `p` establishes a context it is opaque (one confined entry;
// its positioned descendants stay inside it). Otherwise (z-index:auto, no trigger)
// `p` is one non-confined entry AND its own positioned descendants bubble up into
// the same `entries` list (recursively through further auto-positioned boxes), so
// they interleave by z-index with this context's positioned boxes. Pre-order DFS
// preserves tree order, which the stable sort uses as the tie-break.
fn flatten_positioned<'a>(
    p: &'a LayoutBox,
    styled: &StyledTree,
    entries: &mut Vec<PositionedEntry<'a>>,
) {
    let z = p.style(styled).and_then(|s| s.z_index).unwrap_or(0);
    if establishes_stacking_context(p, styled) {
        entries.push(PositionedEntry { b: p, z, confined: true });
        return;
    }
    entries.push(PositionedEntry { b: p, z, confined: false });
    // Bubble this auto-positioned box's positioned descendants into the current
    // context. Walk only its in-flow content (not floats/other positioned roots'
    // confined subtrees) to find the positioned descendants it would defer.
    let mut bubbled: Vec<&LayoutBox> = Vec::new();
    for child in p.children() {
        collect_positioned_descendants(child, styled, &mut bubbled);
    }
    for d in bubbled {
        flatten_positioned(d, styled, entries);
    }
}

// E54-M2: collect the positioned subtree roots reachable from `b` through in-flow
// content only (mirroring `collect_inflow`'s deferral: a positioned box is a root
// we stop at; an in-flow box we descend into; a float we ignore for stacking).
fn collect_positioned_descendants<'a>(
    b: &'a LayoutBox,
    styled: &StyledTree,
    out: &mut Vec<&'a LayoutBox>,
) {
    match role(b, styled) {
        Role::Positioned => out.push(b),
        Role::Float => {}
        Role::InFlow => {
            for child in b.children() {
                collect_positioned_descendants(child, styled, out);
            }
        }
    }
}

// E54-M2: paint a positioned box that does NOT establish a stacking context, at the
// current context level. Mirrors `paint_subtree`'s brackets + in-flow + float
// passes, but its positioned descendants were already flattened into the parent's
// entry list (so the positioned bucket here is discarded, not re-confined).
fn paint_positioned_noncontext(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    if backface_culled(b, styled) {
        return;
    }
    let xform = layer_transform(b, styled);
    if let Some(m) = xform {
        out.push(PaintCmd::PushTransform { matrix: m });
    }
    if let Some((rect, filter)) = backdrop_of(b, styled) {
        out.push(PaintCmd::ApplyBackdropFilter { rect, filter });
    }
    let layer = layer_effect(b, styled);
    if let Some((o, ref filter, blend, ref mask)) = layer {
        out.push(PaintCmd::PushLayer {
            opacity: o,
            filter: filter.clone(),
            blend,
            mask: mask.clone(),
        });
    }
    let cpath = clip_path_of(b, styled);
    if let Some((shape, bbox)) = &cpath {
        out.push(PaintCmd::PushClipPath {
            shape: shape.clone(),
            border_box: *bbox,
        });
    }
    emit_self(b, styled, fonts, images, doc, out);
    let clip = clip_of(b, styled);
    if let Some((rect, radius)) = clip {
        out.push(PaintCmd::PushClip { rect, radius });
    }
    let scroll = scroll_offset_of(b, styled, doc);
    if let Some((sx, sy)) = scroll {
        out.push(PaintCmd::PushTransform {
            matrix: [1.0, 0.0, 0.0, 1.0, -sx, -sy],
        });
    }
    let mut floats: Vec<&LayoutBox> = Vec::new();
    // The positioned descendants here were already flattened into the parent's
    // entry list, so this bucket is intentionally discarded after the walk.
    let mut positioned: Vec<&LayoutBox> = Vec::new();
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
    if scroll.is_some() {
        out.push(PaintCmd::PopTransform);
    }
    if clip.is_some() {
        out.push(PaintCmd::PopClip);
    }
    if let Some(cmds) = scrollbar_of(b, styled, doc) {
        out.extend(cmds);
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
            // E45-M3: skip a backface-visibility:hidden box flipped away from the
            // viewer (det < 0) — the box + its in-flow descendants paint nothing.
            if backface_culled(b, styled) {
                return;
            }
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
            // E37-M2: translate the clipped content by (-scrollLeft, -scrollTop)
            // inside the content clip. Zero offset → no transform (M1-identical).
            let scroll = scroll_offset_of(b, styled, doc);
            if let Some((sx, sy)) = scroll {
                out.push(PaintCmd::PushTransform {
                    matrix: [1.0, 0.0, 0.0, 1.0, -sx, -sy],
                });
            }
            for child in b.children() {
                collect_inflow(child, styled, fonts, images, doc, out, floats, positioned);
            }
            if scroll.is_some() {
                out.push(PaintCmd::PopTransform);
            }
            if clip.is_some() {
                out.push(PaintCmd::PopClip);
            }
            // E37-M1: overlay scrollbar painted on top of (and outside) the
            // content clip + M2 content transform, but inside any
            // clip-path/layer/transform bracket.
            if let Some(cmds) = scrollbar_of(b, styled, doc) {
                out.extend(cmds);
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
/// a `mask` OR `isolation: isolate` (which forces a group so descendant blending
/// is confined to this subtree), else `None` (the fast path, byte-identical to no
/// layer). (E2-M5 §4.2, E21-M1/M2/M3, E32-M3)
fn layer_effect(
    b: &LayoutBox,
    styled: &StyledTree,
) -> Option<(f32, Vec<FilterFn>, BlendMode, Option<MaskBox>)> {
    let s = b.style(styled)?;
    if s.opacity < 1.0
        || !s.filter.is_empty()
        || s.mix_blend_mode != BlendMode::Normal
        || !s.mask.is_empty()
        || s.isolation == Isolation::Isolate
    {
        let mask = if s.mask.is_empty() {
            None
        } else {
            Some(MaskBox {
                specs: s.mask.clone(),
                rect: b.dimensions().border_box(),
                padding_box: b.dimensions().padding_box(),
                content_box: b.dimensions().content_box(),
                radius: s.border_radius,
            })
        };
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

// E37-M1: overlay vertical scrollbar geometry.
const SCROLLBAR_WIDTH: f32 = 12.0;
// E37-M3: `scrollbar-width: thin` paints a narrower bar.
const SCROLLBAR_WIDTH_THIN: f32 = 6.0;
const SCROLLBAR_TRACK_COLOR: Rgba = Rgba {
    r: 0xf0,
    g: 0xf0,
    b: 0xf0,
    a: 0xff,
};
const SCROLLBAR_THUMB_COLOR: Rgba = Rgba {
    r: 0xc0,
    g: 0xc0,
    b: 0xc0,
    a: 0xff,
};
const SCROLLBAR_THUMB_RADIUS: f32 = 6.0;

/// E37-M1/M2: a scroll box's geometry — `(padding_box, scroll_width,
/// scroll_height)` — for a box with `overflow: scroll` (always) or `auto` (only
/// when content overflows vertically). `Visible`/`Hidden`/`Clip` → `None` (the
/// fast path). `scrollWidth`/`scrollHeight` are the max right/bottom edges of the
/// box's children border boxes (relative to the padding box), never less than the
/// padding-box extent (= `clientWidth`/`clientHeight`).
fn scroll_geometry(b: &LayoutBox, styled: &StyledTree) -> Option<(Rect, f32, f32)> {
    let s = b.style(styled)?;
    let always = match s.overflow {
        Overflow::Scroll => true,
        Overflow::Auto => false,
        _ => return None,
    };
    let pad = b.dimensions().padding_box();
    if pad.height <= 0.0 {
        return None;
    }
    let mut scroll_width = pad.width;
    let mut scroll_height = pad.height;
    for child in b.children() {
        let cb = child.dimensions().border_box();
        let right = (cb.x + cb.width) - pad.x;
        if right > scroll_width {
            scroll_width = right;
        }
        let bottom = (cb.y + cb.height) - pad.y;
        if bottom > scroll_height {
            scroll_height = bottom;
        }
    }
    // overflow: auto only shows when content overflows (epsilon for float noise).
    if !always && scroll_height <= pad.height + 0.5 {
        return None;
    }
    Some((pad, scroll_width, scroll_height))
}

/// E37-M2: a scroll box's APPLIED scroll offset `(x, y)` — the stored
/// `scrollLeft`/`scrollTop` clamped to `[0, max(0, scrollExtent - clientExtent)]`
/// (the stored value can exceed the content; the painter clamps here since it has
/// layout). `None` for non-scroll boxes or a zero offset (so no transform is
/// emitted, keeping the no-scroll path byte-identical to M1).
fn scroll_offset_of(b: &LayoutBox, styled: &StyledTree, doc: &Document) -> Option<(f32, f32)> {
    let (pad, scroll_width, scroll_height) = scroll_geometry(b, styled)?;
    let (sx, sy) = doc.scroll_offset(b.style.node());
    let max_x = (scroll_width - pad.width).max(0.0);
    let max_y = (scroll_height - pad.height).max(0.0);
    let mut x = sx.clamp(0.0, max_x);
    let mut y = sy.clamp(0.0, max_y);
    // E60-M3: if the box is a snap container (`scroll-snap-type` with a non-empty
    // axis), snap the clamped offset to the nearest snap-aligned child along each
    // requested axis. Re-clamp after snapping. Non-snap boxes skip this entirely
    // (byte-identical to E37-M2).
    if let Some((snap_x, snap_y)) = snap_axes(b, styled) {
        if snap_x {
            if let Some(t) = nearest_snap_offset(b, styled, pad, x, false) {
                x = t.clamp(0.0, max_x);
            }
        }
        if snap_y {
            if let Some(t) = nearest_snap_offset(b, styled, pad, y, true) {
                y = t.clamp(0.0, max_y);
            }
        }
    }
    if x == 0.0 && y == 0.0 {
        return None;
    }
    Some((x, y))
}

/// E60-M3: the requested snap axes `(x, y)` from `scroll-snap-type` on the
/// CONTAINER. `None` when the box has no `scroll-snap-type` (or it is `none` /
/// empty) — the common case, which skips all snap geometry. The stored string is
/// the raw value (e.g. `"y mandatory"`, `"both proximity"`, `"x"`); we only need
/// the axis keyword (first token). `mandatory` and `proximity` both snap in the
/// MVP.
fn snap_axes(b: &LayoutBox, styled: &StyledTree) -> Option<(bool, bool)> {
    let s = b.style(styled)?;
    let snap = s.scroll_snap.as_ref()?;
    let ty = snap.snap_type.as_deref()?;
    let axis = ty.split_whitespace().next()?;
    match axis {
        "x" => Some((true, false)),
        "y" => Some((false, true)),
        "both" => Some((true, true)),
        _ => None, // `none` / unknown → no snapping.
    }
}

/// E60-M3: among the container's in-flow children that carry `scroll-snap-align`
/// (start/center/end), the snap offset NEAREST to `current` along one axis
/// (`vertical` true → y, false → x). `None` when there are no snap targets (the
/// offset is left at its clamped E37-M2 value).
///
/// For each axis the candidate offset aligns the child's snap-area edge to the
/// snapport edge, where the snapport is the padding box inset by `scroll-padding`
/// and the child's snap area is its border box outset by `scroll-margin`:
///   - start:  child.start - snapport_pad_start
///   - end:    child.end + snap_margin_end - (snapport_extent - snapport_pad_end)
///   - center: child_center - snapport_center
///
/// (all in the scroll container's content coordinate space, i.e. an offset from
/// the unscrolled position).
fn nearest_snap_offset(
    b: &LayoutBox,
    styled: &StyledTree,
    pad: Rect,
    current: f32,
    vertical: bool,
) -> Option<f32> {
    // snapport = padding box inset by the container's `scroll-padding` (% against
    // the snapport extent on that axis).
    let sp = b.style(styled).map(|s| s.scroll_padding()).unwrap_or([LengthPct::Px(0.0); 4]);
    let (pad_start, pad_end, port_extent, port_origin) = if vertical {
        (
            resolve_lp(sp[0], pad.height),
            resolve_lp(sp[2], pad.height),
            pad.height,
            pad.y,
        )
    } else {
        (
            resolve_lp(sp[3], pad.width),
            resolve_lp(sp[1], pad.width),
            pad.width,
            pad.x,
        )
    };
    let mut best: Option<f32> = None;
    for child in b.children() {
        let cs = match child.style(styled) {
            Some(s) => s,
            None => continue,
        };
        let align = match cs.scroll_snap.as_ref().and_then(|s| s.snap_align.as_deref()) {
            Some(a) => a,
            None => continue,
        };
        // `scroll-snap-align` may be two values (block / inline); the relevant
        // one for this axis is the last token for inline (x) and first for block
        // (y). MVP: take the matching token, default to the single value.
        let align = snap_align_for_axis(align, vertical);
        if align == "none" || align.is_empty() {
            continue;
        }
        let cb = child.dimensions().border_box();
        let (c_start, c_extent) = if vertical {
            (cb.y - port_origin, cb.height)
        } else {
            (cb.x - port_origin, cb.width)
        };
        // child snap area = border box outset by the child's scroll-margin.
        let (m_start, m_end) = if vertical {
            (cs.scroll_margin()[0], cs.scroll_margin()[2])
        } else {
            (cs.scroll_margin()[3], cs.scroll_margin()[1])
        };
        let area_start = c_start - m_start;
        let area_end = c_start + c_extent + m_end;
        let offset = match align {
            "start" => area_start - pad_start,
            "end" => area_end - (port_extent - pad_end),
            "center" => {
                let area_center = (area_start + area_end) / 2.0;
                let port_center = (pad_start + (port_extent - pad_end)) / 2.0;
                area_center - port_center
            }
            _ => continue,
        };
        match best {
            Some(prev) if (prev - current).abs() <= (offset - current).abs() => {}
            _ => best = Some(offset),
        }
    }
    best
}

/// E60-M3: pick the `scroll-snap-align` keyword for one axis. The property is
/// `<align>{1,2}` (block then inline). One value applies to both axes; two values
/// → first = block (vertical), second = inline (horizontal).
fn snap_align_for_axis(align: &str, vertical: bool) -> &str {
    let mut it = align.split_whitespace();
    let first = it.next().unwrap_or("");
    match it.next() {
        Some(second) => {
            if vertical {
                first
            } else {
                second
            }
        }
        None => first,
    }
}

/// E37-M1/M2: the OVERLAY vertical-scrollbar paint commands (track + thumb) for a
/// scroll box. `None` for non-scroll boxes (the fast path, byte-identical to no
/// scrollbar). Painted ON TOP of the box's content (not clipped), so the caller
/// emits them after `PopClip`.
///
/// The scrollbar is an overlay at the right edge of the padding box; it does NOT
/// reserve a gutter, so layout is unchanged. The thumb height is
/// `clientHeight / scrollHeight * trackHeight`; its top reflects the applied
/// `scrollTop` (E37-M2): `applied_y / scrollHeight * trackHeight`.
fn scrollbar_of(b: &LayoutBox, styled: &StyledTree, doc: &Document) -> Option<Vec<PaintCmd>> {
    let (pad, _scroll_width, scroll_height) = scroll_geometry(b, styled)?;
    // E37-M3: `scrollbar-width`/`scrollbar-color` style the overlay scrollbar.
    let (sb_width_kind, sb_color) = b
        .style(styled)
        .map_or((ScrollbarWidth::Auto, None), |s| {
            (s.scrollbar_width, s.scrollbar_color)
        });
    let width = match sb_width_kind {
        ScrollbarWidth::None => return None, // hidden: no paint.
        ScrollbarWidth::Thin => SCROLLBAR_WIDTH_THIN,
        ScrollbarWidth::Auto => SCROLLBAR_WIDTH,
    };
    let (thumb_color, track_color) =
        sb_color.unwrap_or((SCROLLBAR_THUMB_COLOR, SCROLLBAR_TRACK_COLOR));
    let client_height = pad.height;
    let track = Rect {
        x: pad.x + pad.width - width,
        y: pad.y,
        width,
        height: client_height,
    };
    // thumb height proportional to the visible fraction; clamped to [min, track].
    let ratio = (client_height / scroll_height).min(1.0);
    let thumb_height = (track.height * ratio).max(width).min(track.height);
    // E37-M2: thumb top reflects the applied (clamped) scrollTop.
    let applied_y = scroll_offset_of(b, styled, doc).map_or(0.0, |(_, y)| y);
    let thumb_top = (track.y + (applied_y / scroll_height) * track.height)
        .clamp(track.y, track.y + track.height - thumb_height);
    let thumb = Rect {
        x: track.x + 1.0,
        y: thumb_top,
        width: width - 2.0,
        height: thumb_height,
    };
    Some(vec![
        PaintCmd::FillRect {
            rect: track,
            color: track_color,
            radius: [0.0; 4],
            blend: BlendMode::Normal,
        },
        PaintCmd::FillRect {
            rect: thumb,
            color: thumb_color,
            radius: [SCROLLBAR_THUMB_RADIUS; 4],
            blend: BlendMode::Normal,
        },
    ])
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
    // E45-M1: an individual translate/rotate/scale also triggers a layer.
    if s.transform.is_empty() && s.individual_transform.is_none() {
        return None;
    }
    Some(compose_transform(s, &b.dimensions().border_box()))
}

/// E45-M3: `backface-visibility: hidden` culls a box whose effective transform
/// flips it so its back faces the viewer. After the E45-M2 flatten, a rotateX/
/// rotateY past 90° becomes a negative axis-scale, so the effective matrix has a
/// negative determinant (`m[0]*m[3] - m[1]*m[2] < 0`). A `visible` box (default)
/// or a front-facing / non-flipping transform (det ≥ 0) is never culled, so the
/// fast path is byte-identical to no backface-visibility.
fn backface_culled(b: &LayoutBox, styled: &StyledTree) -> bool {
    let Some(s) = b.style(styled) else {
        return false;
    };
    if !s.backface_visibility_hidden {
        return false;
    }
    let Some(m) = layer_transform(b, styled) else {
        return false;
    };
    m[0] * m[3] - m[1] * m[2] < 0.0
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

    // E45-M1: the effective list is the individual props (translate, rotate,
    // scale — in that spec order) followed by the `transform` functions.
    let mut acc = Transform::identity();
    if let Some(it) = &style.individual_transform {
        if let Some((x, y)) = it.translate {
            acc = acc.pre_concat(fn_matrix(&TransformFn::Translate(x, y), bb));
        }
        if let Some(r) = it.rotate {
            acc = acc.pre_concat(fn_matrix(&TransformFn::Rotate(r), bb));
        }
        if let Some((sx, sy)) = it.scale {
            acc = acc.pre_concat(fn_matrix(&TransformFn::Scale(sx, sy), bb));
        }
    }
    // f1·f2·…·fn (pre_concat applies the next factor to points first).
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
        BoxKind::Svg => emit_svg(b, styled, fonts, images, doc, out),
        BoxKind::Media => emit_media(b, images, doc, out),
        BoxKind::Canvas => emit_canvas(b, doc, out),
        BoxKind::Embed => emit_embed(b, styled, fonts, out), // E61-M2
        BoxKind::FormControl => emit_form_control(b, styled, fonts, images, doc, out),
        // E47-M2: `background-clip: text` brackets the box's background with a
        // glyph-coverage clip built from the element's own descendant text runs.
        _ if bg_clip_is_text(b.style(styled).unwrap_or(&ComputedStyle::initial())) => {
            let glyphs = collect_text_clip_glyphs(b, styled, fonts);
            out.push(PaintCmd::PushTextClip { glyphs });
            emit_box(b, styled, images, out);
            out.push(PaintCmd::PopTextClip);
        }
        _ => emit_box(b, styled, images, out),
    }
}

/// E47-M2: true iff this element's background should be clipped to its text
/// glyphs — the color clip or any layer clip is `BgGeometryBox::Text`.
fn bg_clip_is_text(style: &ComputedStyle) -> bool {
    style.background_color_clip == BgGeometryBox::Text
        || style
            .background_layers
            .iter()
            .any(|l| l.clip == BgGeometryBox::Text)
}

/// E47-M2: gather the glyph parameters of every TextRun descendant of `b` (the
/// element painting a `background-clip: text` background). Mirrors the horizontal
/// path of `emit_text`. Vertical writing modes / markers are skipped (MVP — the
/// coverage just omits them). The collected runs build the glyph clip mask.
fn collect_text_clip_glyphs(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
) -> Vec<TextClipGlyph> {
    let mut out = Vec::new();
    collect_text_clip_rec(b, styled, fonts, &mut out);
    out
}

fn collect_text_clip_rec(
    b: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    out: &mut Vec<TextClipGlyph>,
) {
    if b.kind() == BoxKind::TextRun {
        if let (Some(style), Some(text)) = (b.style(styled), b.text()) {
            if !text.is_empty() && !style.writing_mode.is_vertical() {
                let c = b.dimensions().content;
                let q = FontQuery {
                    family: &style.font_family,
                    style: style.font_style,
                    weight: style.font_weight,
                    size: style.font_size,
                    letter_spacing: style.letter_spacing,
                    word_spacing: style.word_spacing,
                    features: style.font_features(),
                    kerning: style.font_kerning,
                    variations: style.font_variations(),
                };
                let lm = fonts.line_metrics(&q);
                out.push(TextClipGlyph {
                    origin: (c.x, c.y),
                    text: text.to_string(),
                    font_size: style.font_size,
                    weight: style.font_weight,
                    style: style.font_style,
                    family: style.font_family.clone(),
                    ascent: lm.ascent,
                    letter_spacing: style.letter_spacing,
                    word_spacing: style.word_spacing,
                    features: style.effective_font_features(),
                    kerning: style.font_kerning,
                    variations: style.font_variations().to_vec(),
                });
            }
        }
    }
    for child in b.children() {
        collect_text_clip_rec(child, styled, fonts, out);
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
// --- E39-M1: gauge (progress/meter) colors ---
/// The progress/meter track (#e6e6e6).
const GAUGE_TRACK: Rgba = Rgba {
    r: 0xe6,
    g: 0xe6,
    b: 0xe6,
    a: 255,
};
/// The <progress> fill (#2680eb blue).
const PROGRESS_FILL: Rgba = Rgba {
    r: 0x26,
    g: 0x80,
    b: 0xeb,
    a: 255,
};
/// The <meter> fill (#22aa22 green).
const METER_FILL: Rgba = Rgba {
    r: 0x22,
    g: 0xaa,
    b: 0x22,
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
    // E51-M2: `appearance: none` strips the UA control chrome (tick/dot/dropdown
    // triangle/range track+thumb/color swatch). The control then renders as a
    // plain box styled only by author CSS, so paint its own background/border via
    // the normal box path and skip all the chrome below. Text controls
    // (TextInput/TextArea/Button) already paint only their box + text content via
    // `emit_text_control` (no hard-coded chrome beyond the box itself), so they
    // fall through unchanged — their text content still renders.
    let appearance_none = b.style(styled).is_some_and(|s| s.appearance_none);
    if appearance_none
        && matches!(
            kind,
            FormControl::Checkbox { .. }
                | FormControl::Radio { .. }
                | FormControl::Select
                | FormControl::Color
                | FormControl::Range
                | FormControl::Progress { .. }
                | FormControl::Meter { .. }
        )
    {
        emit_box(b, styled, images, out);
        return;
    }
    // E51-M1: `accent-color` (inherited). `None` = `auto` → keep the UA colors.
    let accent = b.style(styled).and_then(|s| s.accent_color);
    match kind {
        FormControl::Checkbox { checked } => emit_checkbox(b, checked, accent, out),
        FormControl::Radio { checked } => emit_radio(b, checked, accent, out),
        FormControl::Select => emit_select(b, styled, fonts, images, doc, out),
        FormControl::Color => emit_color(b, doc, out),
        FormControl::Range => emit_range(b, doc, accent, out),
        FormControl::Progress { value, max } => emit_progress(b, value, max, accent, out),
        FormControl::Meter { value, min, max } => emit_meter(b, value, min, max, accent, out),
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
fn emit_checkbox(b: &LayoutBox, checked: bool, accent: Option<Rgba>, out: &mut Vec<PaintCmd>) {
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
        // E51-M1: tint the checked tick with accent-color (`None` keeps #333333).
        emit_shape(
            SvgGeom::Path(crate::svg_path::points_to_ops(&pts, false)),
            None,
            Some(accent.unwrap_or(FC_MARK)),
            2.0,
            out,
        );
    }
}

/// Emit `<input type=radio>` (E14-M2): a white circle with a #767676 outline;
/// when checked, a filled #333333 centre dot.
fn emit_radio(b: &LayoutBox, checked: bool, accent: Option<Rgba>, out: &mut Vec<PaintCmd>) {
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
        // E51-M1: tint the checked dot with accent-color (`None` keeps #333333).
        emit_shape(
            SvgGeom::Ellipse {
                cx,
                cy,
                rx: 0.25 * min,
                ry: 0.25 * min,
            },
            Some(accent.unwrap_or(FC_MARK)),
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
fn emit_range(b: &LayoutBox, doc: &Document, accent: Option<Rgba>, out: &mut Vec<PaintCmd>) {
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
    // E51-M1: tint the thumb with accent-color (`None` keeps the white #FFFFFF).
    let cx = tx0 + frac * (tx1 - tx0);
    emit_shape(
        SvgGeom::Ellipse {
            cx,
            cy,
            rx: r,
            ry: r,
        },
        Some(accent.unwrap_or(FC_BG)),
        Some(FC_BORDER),
        1.0,
        out,
    );
}

/// E39-M1: emit a gauge bar — a rounded #e6e6e6 track filling the content box,
/// then a `fill` rectangle from the left whose width is `frac` of the track.
/// `frac` is already clamped to 0..1 by the caller.
fn emit_gauge(b: &LayoutBox, frac: f32, fill: Rgba, out: &mut Vec<PaintCmd>) {
    let cb = b.dimensions().content;
    let radius = (cb.height / 2.0).min(4.0);
    // Track: the whole content box.
    out.push(PaintCmd::FillRect {
        rect: cb,
        color: GAUGE_TRACK,
        radius: [radius; 4],
        blend: BlendMode::Normal,
    });
    // Fill: from the left, `frac` of the width.
    let fw = (frac.clamp(0.0, 1.0) * cb.width).max(0.0);
    if fw > 0.0 {
        out.push(PaintCmd::FillRect {
            rect: Rect {
                x: cb.x,
                y: cb.y,
                width: fw,
                height: cb.height,
            },
            color: fill,
            radius: [radius; 4],
            blend: BlendMode::Normal,
        });
    }
}

/// Emit `<progress>` (E39-M1). Determinate: fill = `value/max` (clamped). The
/// indeterminate sentinel (`value < 0`, i.e. no/invalid `value` attribute) draws
/// a moderate 0.5 fill as the MVP indeterminate state (no animation).
fn emit_progress(b: &LayoutBox, value: f32, max: f32, accent: Option<Rgba>, out: &mut Vec<PaintCmd>) {
    let frac = if value < 0.0 || max <= 0.0 {
        // Indeterminate MVP: a half-filled track.
        0.5
    } else {
        (value / max).clamp(0.0, 1.0)
    };
    // E51-M1: tint the fill with accent-color (`None` keeps the #2680eb blue).
    emit_gauge(b, frac, accent.unwrap_or(PROGRESS_FILL), out);
}

/// Emit `<meter>` (E39-M1). Fill = `(value-min)/(max-min)` (clamped); `0` for an
/// empty/reversed span (`max <= min`).
fn emit_meter(b: &LayoutBox, value: f32, min: f32, max: f32, accent: Option<Rgba>, out: &mut Vec<PaintCmd>) {
    let frac = range_fraction(value, min, max);
    // E51-M1: tint the fill with accent-color (`None` keeps the #22aa22 green).
    emit_gauge(b, frac, accent.unwrap_or(METER_FILL), out);
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
            features: style.font_features(), // E46-M1
            kerning: style.font_kerning,
            variations: style.font_variations(), // E46-M3
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
            features: style.effective_font_features(), // E46-M2
            kerning: style.font_kerning,
            variations: style.font_variations().to_vec(), // E46-M3
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

    // E58-M1: UA chrome for number/search/date-like flavors (drawn on top of the
    // field box, before the value text, so it shows even for an empty value).
    // `appearance:none` strips the indicator (the field still renders its box +
    // value text), matching the E51-M2 chrome-suppression rule.
    if let FormControl::TextInput { flavor, .. } = kind {
        if !style.appearance_none {
            emit_input_chrome(b, style.font_size, flavor, out);
        }
    }

    let grey = Rgba {
        r: 0x75,
        g: 0x75,
        b: 0x75,
        a: 255,
    };
    // E35-M2: text style used for the placeholder run. When an `input::placeholder`
    // rule produced a pseudo style, its color/font properties drive the placeholder
    // text; otherwise we keep the control's own style + the hard-coded grey, leaving
    // the no-rule path byte-identical.
    let mut text_style = style;
    let (text, color) = match kind {
        FormControl::TextInput { password, .. } => {
            let (t, is_placeholder) = input_display(doc, id, password);
            let color = if is_placeholder {
                match styled.pseudo_style(id, PseudoElement::Placeholder) {
                    Some(ps) => {
                        text_style = ps;
                        ps.color
                    }
                    None => grey,
                }
            } else {
                style.color
            };
            (t, color)
        }
        FormControl::TextArea => (textarea_value(doc, id), style.color),
        FormControl::Button => (control_label(doc, id), style.color),
        // Choice controls are dispatched to their own emitters upstream.
        _ => unreachable!("non-text control in emit_text_control"),
    };
    if text.is_empty() {
        return;
    }
    let style = text_style;

    let q = FontQuery {
        family: &style.font_family,
        style: style.font_style,
        weight: style.font_weight,
        size: style.font_size,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
        features: style.font_features(), // E46-M1
        kerning: style.font_kerning,
        variations: style.font_variations(), // E46-M3
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
        features: style.effective_font_features(), // E46-M2
        kerning: style.font_kerning,
        variations: style.font_variations().to_vec(), // E46-M3
    });
    out.push(PaintCmd::PopClip);
}

/// E58-M1: draw the UA chrome indicator for a text-input flavor at the right edge
/// of the field. `Plain` draws nothing (byte-identical to a plain text input).
/// - `Number`: two stacked #333333 triangles (up ▲ / down ▼) — a spinner.
/// - `Search`: a small magnifier dot (a #767676 circle outline).
/// - `DateLike`: a single #333333 down-triangle picker indicator.
///
/// The arrow slot is `font_size` wide on the right; the field's value text is
/// painted afterwards over the full content box (it clips to `cb`), matching the
/// existing text-input layout.
fn emit_input_chrome(b: &LayoutBox, font_size: f32, flavor: TextFlavor, out: &mut Vec<PaintCmd>) {
    if flavor == TextFlavor::Plain {
        return;
    }
    let cb = b.dimensions().content;
    let slot_w = font_size.min(cb.width);
    let acx = cb.x + cb.width - slot_w / 2.0;
    match flavor {
        TextFlavor::Plain => {}
        TextFlavor::Number => {
            // Two small triangles stacked: up over down, centered in the slot.
            let cy = cb.y + cb.height / 2.0;
            let tw = 0.24 * font_size;
            let th = 0.16 * font_size;
            let gap = 0.10 * font_size;
            // Up triangle (apex above its baseline).
            let up = [
                (acx, cy - gap - th),
                (acx - tw, cy - gap),
                (acx + tw, cy - gap),
            ];
            // Down triangle (apex below its baseline).
            let down = [
                (acx - tw, cy + gap),
                (acx + tw, cy + gap),
                (acx, cy + gap + th),
            ];
            emit_shape(
                SvgGeom::Path(crate::svg_path::points_to_ops(&up, true)),
                Some(FC_MARK),
                None,
                0.0,
                out,
            );
            emit_shape(
                SvgGeom::Path(crate::svg_path::points_to_ops(&down, true)),
                Some(FC_MARK),
                None,
                0.0,
                out,
            );
        }
        TextFlavor::DateLike => {
            // A single down-triangle picker indicator (mirrors the <select> arrow).
            let acy = cb.y + cb.height / 2.0;
            let pts = [
                (acx - 0.30 * font_size, acy - 0.18 * font_size),
                (acx + 0.30 * font_size, acy - 0.18 * font_size),
                (acx, acy + 0.18 * font_size),
            ];
            emit_shape(
                SvgGeom::Path(crate::svg_path::points_to_ops(&pts, true)),
                Some(FC_MARK),
                None,
                0.0,
                out,
            );
        }
        TextFlavor::Search => {
            // A small magnifier dot: a #767676 circle outline in the slot.
            let acy = cb.y + cb.height / 2.0;
            let r = 0.22 * font_size;
            emit_shape(
                SvgGeom::Ellipse {
                    cx: acx,
                    cy: acy,
                    rx: r,
                    ry: r,
                },
                None,
                Some(FC_BORDER),
                (0.08 * font_size).max(1.0),
                out,
            );
        }
    }
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

    // E47-M1: the padding/content boxes for `background-origin`/`-clip`. The
    // box passed as `rect` stays the byte-identical default (border-box, or the
    // padding-box in the rounded+border case); non-default boxes use these.
    let pad_box = d.padding_box();
    let cont_box = d.content_box();
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
            emit_background_at(pb, irad, pad_box, cont_box, style, images, out);
        } else {
            emit_background_at(bb, radius, pad_box, cont_box, style, images, out);
        }
    } else {
        emit_background_at(bb, radius, pad_box, cont_box, style, images, out);
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

    // E68-M1: border-image (9-slice). Painted OVER the solid border in the
    // border region. Early-returns for the no-border-image case (byte-identical).
    emit_border_image(b, styled, images, out);
}

/// E68-M1: paint a `border-image` as 8 nearest-neighbour blits (4 corners + 4
/// edges) of the source image's 9-slice regions into the box's border region.
/// Center fill (`slice.fill`) is not painted yet (M2). Returns early when there
/// is no border-image, an empty source, or the source image isn't decoded yet
/// (the already-painted solid border then remains).
fn emit_border_image(
    b: &LayoutBox,
    styled: &StyledTree,
    images: &ImageStore,
    out: &mut Vec<PaintCmd>,
) {
    // Border-image is painted once, on the element's PRINCIPAL box. Anonymous
    // boxes borrow the element's style ref, and a `LineBox` clones it (see
    // `inline.rs`); both must be skipped or they would re-paint a second,
    // offset 9-slice. (M1 was a no-op on them only because the default
    // `1×border` thickness is 0 on a 0-border line/anonymous box; E68-M2's
    // `border-image-width` can give a non-zero thickness, exposing the dup.)
    if matches!(b.style, BoxStyleRef::Anonymous(_))
        || matches!(b.kind(), BoxKind::LineBox | BoxKind::AnonymousBlock)
    {
        return;
    }
    let Some(style) = b.style(styled) else { return };
    let Some(bi) = style.border_image.as_deref() else {
        return;
    };
    if bi.source.is_empty() {
        return;
    }
    let Some(img) = images.peek(&bi.source) else {
        return;
    };
    let (iw, ih) = (img.width as f32, img.height as f32);
    let sl = &bi.slice;
    // Resolve each side's slice into source px, clamped to its image dimension.
    let resolve = |v: f32, pct: bool, dim: f32| -> f32 {
        (if pct { v * dim } else { v }).clamp(0.0, dim)
    };
    let st = resolve(sl.top, sl.percent[0], ih);
    let sr = resolve(sl.right, sl.percent[1], iw);
    let sb = resolve(sl.bottom, sl.percent[2], ih);
    let sslf = resolve(sl.left, sl.percent[3], iw);

    let d = b.dimensions();
    let bb = d.border_box();
    let (x, y, w, h) = (bb.x, bb.y, bb.width, bb.height);
    let (abt, abr, abbm, abl) = (d.border.top, d.border.right, d.border.bottom, d.border.left);

    // E68-M2: the DEST border thickness per side comes from `border-image-width`
    // (initial `1` = 1× the used border width, so M1 output is unchanged). The
    // natural slice px (`Auto`) and the actual border widths feed the resolution.
    let nat = [st, sr, sb, sslf]; // top, right, bottom, left natural slice px
    let act = [abt, abr, abbm, abl];
    let bw_of = |bw: BorderImageWidth, side: usize| -> f32 {
        match bw {
            BorderImageWidth::Number(n) => (n * act[side]).max(0.0),
            BorderImageWidth::Length(px) => px.max(0.0),
            // top/bottom % of box height; left/right % of box width.
            BorderImageWidth::Percent(f) => {
                (f * if side == 0 || side == 2 { h } else { w }).max(0.0)
            }
            BorderImageWidth::Auto => nat[side].max(0.0),
        }
    };
    let mut bt = bw_of(bi.width[0], 0);
    let mut br = bw_of(bi.width[1], 1);
    let mut bbm = bw_of(bi.width[2], 2);
    let mut bl = bw_of(bi.width[3], 3);
    // Proportional clamp so opposing widths don't exceed the box on either axis.
    if bl + br > w && bl + br > 0.0 {
        let s = w / (bl + br);
        bl *= s;
        br *= s;
    }
    if bt + bbm > h && bt + bbm > 0.0 {
        let s = h / (bt + bbm);
        bt *= s;
        bbm *= s;
    }

    let blit = |dx: f32, dy: f32, dw: f32, dh: f32, cx: f32, cy: f32, cw: f32, ch: f32,
                out: &mut Vec<PaintCmd>| {
        if dw <= 0.0 || dh <= 0.0 || cw <= 0.0 || ch <= 0.0 {
            return;
        }
        out.push(PaintCmd::ImageBlit {
            dest: Rect { x: dx, y: dy, width: dw, height: dh },
            src: bi.source.clone(),
            src_crop: Rect { x: cx, y: cy, width: cw, height: ch },
            smooth: false,
            blend: BlendMode::Normal,
        });
    };

    // E68-M2 `fill`: paint the center slice into the inner (content+padding) box
    // before the corners/edges (the border tiles overpaint any seam).
    if bi.slice.fill {
        blit(
            x + bl, y + bt, w - bl - br, h - bt - bbm,
            sslf, st, iw - sslf - sr, ih - st - sb,
            out,
        );
    }

    // Corners (always stretched).
    blit(x, y, bl, bt, 0.0, 0.0, sslf, st, out); // TL
    blit(x + w - br, y, br, bt, iw - sr, 0.0, sr, st, out); // TR
    blit(x, y + h - bbm, bl, bbm, 0.0, ih - sb, sslf, sb, out); // BL
    blit(x + w - br, y + h - bbm, br, bbm, iw - sr, ih - sb, sr, sb, out); // BR

    // Edges (tiled per `border-image-repeat`). repeat[0] = horizontal axis →
    // top/bottom edges (tile along x); repeat[1] = vertical → left/right (along y).
    let src = bi.source.clone();
    // top
    emit_edge(
        Rect { x: x + bl, y, width: w - bl - br, height: bt },
        Rect { x: sslf, y: 0.0, width: iw - sslf - sr, height: st },
        bi.repeat[0], true, &src, out,
    );
    // bottom
    emit_edge(
        Rect { x: x + bl, y: y + h - bbm, width: w - bl - br, height: bbm },
        Rect { x: sslf, y: ih - sb, width: iw - sslf - sr, height: sb },
        bi.repeat[0], true, &src, out,
    );
    // left
    emit_edge(
        Rect { x, y: y + bt, width: bl, height: h - bt - bbm },
        Rect { x: 0.0, y: st, width: sslf, height: ih - st - sb },
        bi.repeat[1], false, &src, out,
    );
    // right
    emit_edge(
        Rect { x: x + w - br, y: y + bt, width: br, height: h - bt - bbm },
        Rect { x: iw - sr, y: st, width: sr, height: ih - st - sb },
        bi.repeat[1], false, &src, out,
    );
}

/// E68-M2: emit the blits for ONE border-image edge into `dest`, tiling the
/// source slice `src` per `mode`. `axis_is_horizontal` selects the tiling axis
/// (x for top/bottom, y for left/right). `Stretch` is the M1 single-blit path
/// (kept byte-identical); `Repeat` tiles the slice at its corner-matched natural
/// size, centered, clipping the end tiles; `Round` adjusts the tile size so a
/// whole number fit exactly; `Space` is treated as `Repeat` (MVP).
fn emit_edge(
    dest: Rect,
    src: Rect,
    mode: BorderImageRepeat,
    axis_is_horizontal: bool,
    src_name: &str,
    out: &mut Vec<PaintCmd>,
) {
    if dest.width <= 0.0 || dest.height <= 0.0 || src.width <= 0.0 || src.height <= 0.0 {
        return;
    }
    let push = |dest: Rect, crop: Rect, out: &mut Vec<PaintCmd>| {
        if dest.width <= 0.0 || dest.height <= 0.0 || crop.width <= 0.0 || crop.height <= 0.0 {
            return;
        }
        out.push(PaintCmd::ImageBlit {
            dest,
            src: src_name.to_string(),
            src_crop: crop,
            smooth: false,
            blend: BlendMode::Normal,
        });
    };

    if matches!(mode, BorderImageRepeat::Stretch) {
        push(dest, src, out);
        return;
    }

    // Length along the tiling axis and the fixed thickness across it.
    let edge_len = if axis_is_horizontal { dest.width } else { dest.height };
    // Corner scale = dest thickness / slice thickness (across-axis), so a tile
    // matches the corner scale; this scales the slice's along-axis source size.
    let (slice_thick, slice_along) = if axis_is_horizontal {
        (src.height, src.width)
    } else {
        (src.width, src.height)
    };
    if slice_thick <= 0.0 {
        push(dest, src, out);
        return;
    }
    let dest_thick = if axis_is_horizontal { dest.height } else { dest.width };
    let scale = dest_thick / slice_thick;
    let mut tile_len = (slice_along * scale).max(0.01);

    // `Round` (and an exact-fit refinement) snaps to a whole tile count.
    if matches!(mode, BorderImageRepeat::Round) {
        let count = (edge_len / tile_len).round().max(1.0);
        tile_len = edge_len / count;
    }

    // Tile centered along the edge: start so the run is centered, then clip the
    // (partial) end tiles' dest + src_crop proportionally.
    let n = (edge_len / tile_len).ceil().max(1.0) as i32;
    let total = n as f32 * tile_len;
    let start = -(total - edge_len) / 2.0; // ≤ 0 (overhang split both ends)
    for i in 0..n {
        let t0 = start + i as f32 * tile_len; // tile start along edge (may be < 0)
        let t1 = t0 + tile_len;
        // Clip to [0, edge_len].
        let c0 = t0.max(0.0);
        let c1 = t1.min(edge_len);
        let vis = c1 - c0;
        if vis <= 0.0 {
            continue;
        }
        // Fraction of the tile cut at the leading/trailing edge → crop the source.
        let lead = (c0 - t0) / tile_len; // 0..1 cut from the tile's start
        let frac = vis / tile_len; // 0..1 visible fraction of the tile
        let (dest_r, crop_r) = if axis_is_horizontal {
            (
                Rect { x: dest.x + c0, y: dest.y, width: vis, height: dest.height },
                Rect {
                    x: src.x + lead * src.width,
                    y: src.y,
                    width: frac * src.width,
                    height: src.height,
                },
            )
        } else {
            (
                Rect { x: dest.x, y: dest.y + c0, width: dest.width, height: vis },
                Rect {
                    x: src.x,
                    y: src.y + lead * src.height,
                    width: src.width,
                    height: frac * src.height,
                },
            )
        };
        push(dest_r, crop_r, out);
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
/// E47-M1: resolve a layer's `background-origin`/`background-clip` box.
/// `BorderBox` maps to the painter's default `rect` (the border box, or the
/// padding box in the rounded+border case) so default layers stay byte-identical;
/// `PaddingBox`/`ContentBox` use the element's actual geometry boxes.
fn bg_geometry_box(b: BgGeometryBox, rect: Rect, pad_box: Rect, cont_box: Rect) -> Rect {
    match b {
        BgGeometryBox::BorderBox => rect,
        BgGeometryBox::PaddingBox => pad_box,
        BgGeometryBox::ContentBox => cont_box,
        // E47-M2: `text` is not a rect — the glyph clip is applied by the
        // `PushTextClip`/`PopTextClip` bracket in `emit_self`. For the rect-based
        // fill/position fall back to the border box (the paint area is the box;
        // the glyph mask then carves it).
        BgGeometryBox::Text => rect,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_background_at(
    rect: Rect,
    radius: [f32; 4],
    pad_box: Rect,
    cont_box: Rect,
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
    // E47-M1: per spec the color uses `background_color_clip` (the last clip
    // value; applies even with no image layers). Default (BorderBox) keeps
    // `rect`/`radius` byte-identical; a non-border clip shrinks the fill to that
    // box (sharp corners, MVP).
    if style.background_color.a != 0 {
        let (crect, crad) = if style.background_color_clip != BgGeometryBox::BorderBox {
            (
                bg_geometry_box(style.background_color_clip, rect, pad_box, cont_box),
                [0.0; 4],
            )
        } else {
            (rect, radius)
        };
        out.push(PaintCmd::FillRect {
            rect: crect,
            color: style.background_color,
            radius: crad,
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
        // E47-M1: the box gradients fill / images clip to. Default (BorderBox)
        // = `rect`, so default layers keep `rect`/`radius` byte-identically.
        let clip_box = bg_geometry_box(layer.clip, rect, pad_box, cont_box);
        let layer_default = layer.clip == BgGeometryBox::BorderBox;
        let (mut lrect, lrad) = if layer_default {
            (rect, radius)
        } else {
            // A non-border clip box drops the (border-box) corner rounding — the
            // padding/content box has its own (inset) geometry; MVP uses sharp.
            (clip_box, [0.0; 4])
        };
        // E47-M2: `background-attachment: fixed` — best-effort. One-shot render
        // has no scroll, so a fixed layer is anchored against the viewport origin
        // (0,0): keep the painted size but move its top-left to the page origin so
        // the gradient/image positions against the viewport rather than the box.
        // `scroll`/`local` keep the element-relative `lrect` (byte-identical).
        if layer.attachment == BgAttachment::Fixed {
            lrect.x = 0.0;
            lrect.y = 0.0;
        }
        emit_one_bg_image(
            &layer.image, lrect, lrad, rect, pad_box, cont_box, layer, blend, images, out,
        );
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

/// Emit one background `BgImage` into the resolved layer geometry (`lrect`/
/// `lrad` are the clip rect + radius; `rect`/`pad_box`/`cont_box` are the
/// origin-box candidates). Factored out of `emit_background_at` so
/// `cross-fade()` can recurse on its two operands. (E48-M3)
#[allow(clippy::too_many_arguments)]
fn emit_one_bg_image(
    image: &BgImage,
    lrect: Rect,
    lrad: [f32; 4],
    rect: Rect,
    pad_box: Rect,
    cont_box: Rect,
    layer: &BackgroundLayer,
    blend: BlendMode,
    images: &ImageStore,
    out: &mut Vec<PaintCmd>,
) {
    match image {
        BgImage::Gradient(g) => {
            out.push(PaintCmd::GradientRect {
                rect: lrect,
                gradient: g.clone(),
                radius: lrad,
                blend,
            });
        }
        BgImage::Radial(g) => {
            out.push(PaintCmd::RadialRect {
                rect: lrect,
                gradient: g.clone(),
                radius: lrad,
                blend,
            });
        }
        BgImage::Conic(g) => {
            out.push(PaintCmd::ConicRect {
                rect: lrect,
                gradient: g.clone(),
                radius: lrad,
                blend,
            });
        }
        // E47-M1: origin box positions/sizes the image; clip box bounds it.
        BgImage::Url(src) => {
            let mut origin_box = bg_geometry_box(layer.origin, rect, pad_box, cont_box);
            // E47-M2: a fixed layer positions the tile against the viewport.
            if layer.attachment == BgAttachment::Fixed {
                origin_box.x = 0.0;
                origin_box.y = 0.0;
            }
            emit_bg_image(lrect, lrad, origin_box, src, layer, blend, images, out)
        }
        // E48-M3: `cross-fade(a p, b)` = visually `a*p + b*(1-p)`. MVP "a over b
        // at alpha p": emit `b` at full opacity, then emit `a` wrapped in a
        // PushLayer of opacity `p`. For two opaque operands (the common case —
        // two opaque gradients) `a` source-over `b` at alpha `p` equals
        // `a*p + b*(1-p)` exactly. Both operands recurse through this same emit.
        BgImage::CrossFade { a, b, p } => {
            emit_one_bg_image(b, lrect, lrad, rect, pad_box, cont_box, layer, blend, images, out);
            out.push(PaintCmd::PushLayer {
                opacity: p.clamp(0.0, 1.0),
                filter: Vec::new(),
                blend: BlendMode::Normal,
                mask: None,
            });
            emit_one_bg_image(
                a,
                lrect,
                lrad,
                rect,
                pad_box,
                cont_box,
                layer,
                BlendMode::Normal,
                images,
                out,
            );
            out.push(PaintCmd::PopLayer);
        }
    }
}

/// Painter cap: never emit more than this many tiles per axis for a repeating
/// background, so a 1px image in a huge box can't blow up the display list.
const MAX_BG_TILES_PER_AXIS: usize = 4096;

/// Emit one `url(...)` background layer (E16-M2): resolve the tile size from
/// `background-size`, the origin from `background-position`, then blit the tile
/// once (no-repeat) or across the box (repeat), clipped to `rect`. A missing /
/// zero-size image emits nothing.
/// E47-M1: `rect`/`radius` is the clip box (border-box default); `origin_box` is
/// the `background-origin` box the tile size-percent + position resolve against.
/// When both default to border-box, `origin_box == rect` → byte-identical.
#[allow(clippy::too_many_arguments)]
fn emit_bg_image(
    rect: Rect,
    radius: [f32; 4],
    origin_box: Rect,
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
    let (tw, th) = bg_tile_size(layer.size, iw, ih, origin_box.width, origin_box.height);
    if tw <= 0.0 || th <= 0.0 {
        return;
    }
    let ox = origin_box.x + align(layer.position.0, origin_box.width - tw);
    let oy = origin_box.y + align(layer.position.1, origin_box.height - th);
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
        emit_svg_into(&parsed.doc, styled, fonts, images, parsed.svg_id, dest, out);
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
            features: s.font_features(), // E46-M1
            kerning: s.font_kerning,
            variations: s.font_variations(), // E46-M3
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
            features: s.effective_font_features(), // E46-M2
            kerning: s.font_kerning,
            variations: s.font_variations().to_vec(), // E46-M3
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
    let mut poster_painted = false;
    if let Some(poster) = b.text() {
        if let Some(img) = images.peek(poster) {
            // E61-M3: a `<video>` poster is fitted with `object-fit: contain`
            // (centered, letterboxed) into the media box rather than stretched —
            // reuse the E15-M1 `fit_image` Contain math with centered position.
            let (iw, ih) = (img.width as f32, img.height as f32);
            let center = (LengthPct::Percent(50.0), LengthPct::Percent(50.0));
            let (drect, src_crop) = fit_image(dest, iw, ih, ObjectFit::Contain, center);
            out.push(PaintCmd::ImageBlit {
                dest: drect,
                src: poster.to_string(),
                src_crop,
                smooth: false,
                blend: BlendMode::Normal,
            });
            poster_painted = true;
        }
    }
    let is_video = doc.tag_name(b.style.node()) == Some("video");
    if !poster_painted {
        emit_media_placeholder(dest, is_video, out);
    }
    // E61-M1: a `<video controls>`/`<audio controls>` paints a bottom control
    // bar over the poster/placeholder. Gated on the `controls` attr so a media
    // element without it is byte-identical to E15-M3.
    if doc.get_attribute(b.style.node(), "controls").is_some() {
        emit_media_controls(dest, out);
    }
}

/// Paint the E61-M1 media control bar: a dark bar along the bottom of `dest`
/// holding a small left-aligned play triangle and a thin rounded timeline track
/// (with a filled head + knob). Static chrome only — no interaction.
fn emit_media_controls(dest: Rect, out: &mut Vec<PaintCmd>) {
    // Bar: ~24px tall, clamped so it never exceeds half the box (tiny boxes).
    let bar_h = 24.0_f32.min(dest.height * 0.5);
    if bar_h <= 0.0 {
        return;
    }
    let bar = Rect {
        x: dest.x,
        y: dest.y + dest.height - bar_h,
        width: dest.width,
        height: bar_h,
    };
    let bar_bg = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 153, // ~0.6 alpha
    };
    out.push(fill(bar, bar_bg));

    let light = Rgba {
        r: 0xee,
        g: 0xee,
        b: 0xee,
        a: 255,
    };
    // Play triangle on the left, inscribed in the bar with a small inset.
    let pad = (bar_h * 0.3).max(1.0);
    let tri_h = bar_h - 2.0 * pad;
    let tri_w = tri_h * 0.85;
    let tx = bar.x + pad;
    let ty = bar.y + pad;
    let pts = [
        (tx, ty),
        (tx, ty + tri_h),
        (tx + tri_w, ty + tri_h * 0.5),
    ];
    emit_shape(
        SvgGeom::Path(crate::svg_path::points_to_ops(&pts, true)),
        Some(light),
        None,
        0.0,
        out,
    );

    // Timeline track: a thin rounded bar spanning the rest of the width.
    let track_x = tx + tri_w + pad;
    let track_h = (bar_h * 0.18).max(2.0);
    let track_y = bar.y + (bar_h - track_h) * 0.5;
    let track_w = bar.x + bar.width - pad - track_x;
    if track_w <= 0.0 {
        return;
    }
    let r = track_h * 0.5;
    let track_dim = Rgba {
        r: 0x88,
        g: 0x88,
        b: 0x88,
        a: 255,
    };
    out.push(PaintCmd::FillRect {
        rect: Rect {
            x: track_x,
            y: track_y,
            width: track_w,
            height: track_h,
        },
        color: track_dim,
        radius: [r; 4],
        blend: BlendMode::Normal,
    });
    // Filled head (~15% of the track) at the start, in the light colour.
    let head_w = (track_w * 0.15).min(track_w);
    out.push(PaintCmd::FillRect {
        rect: Rect {
            x: track_x,
            y: track_y,
            width: head_w,
            height: track_h,
        },
        color: light,
        radius: [r; 4],
        blend: BlendMode::Normal,
    });
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

/// Emit an `<iframe>`/`<embed>`/`<object>` placeholder (E61-M2): a 1px grey
/// border around the content box plus the element's `src`/`data` URL label
/// (carried in `b.text`) drawn centered, clipped to the box. No cross-document
/// content is loaded. Mirrors the broken-image alt-text path for the label.
fn emit_embed(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    let dest = b.dimensions().content;
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return;
    }
    // 1px grey border around the box (same colour as the broken-image border).
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
    // The src/data URL label, drawn centered (vertically) and clipped. Skipped
    // when there is no URL.
    let Some(label) = b.text().filter(|t| !t.is_empty()) else {
        return;
    };
    let initial = ComputedStyle::initial();
    let s = b.style(styled).unwrap_or(&initial);
    let q = FontQuery {
        family: &s.font_family,
        style: s.font_style,
        weight: s.font_weight,
        size: s.font_size,
        letter_spacing: s.letter_spacing,
        word_spacing: s.word_spacing,
        features: s.font_features(),
        kerning: s.font_kerning,
        variations: s.font_variations(),
    };
    let lm = fonts.line_metrics(&q);
    let cy = dest.y + (dest.height - lm.ascent - lm.descent) / 2.0;
    out.push(PaintCmd::PushClip {
        rect: dest,
        radius: [0.0; 4],
    });
    out.push(PaintCmd::GlyphRun {
        origin: (dest.x + 2.0, cy.max(dest.y)),
        text: label.to_string(),
        font_size: s.font_size,
        weight: s.font_weight,
        style: s.font_style,
        family: s.font_family.clone(),
        color: s.color,
        ascent: lm.ascent,
        letter_spacing: s.letter_spacing,
        word_spacing: s.word_spacing,
        features: s.effective_font_features(),
        kerning: s.font_kerning,
        variations: s.font_variations().to_vec(),
    });
    out.push(PaintCmd::PopClip);
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
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
    doc: &Document,
    out: &mut Vec<PaintCmd>,
) {
    emit_svg_into(
        doc,
        styled,
        fonts,
        images,
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
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
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
        walk_svg(doc, styled, fonts, images, child, root_t, &ctx, &grads, 0, out); // E38-M1
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
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
    id: NodeId,
    parent_t: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize, // E38-M1: <use> expansion depth, bounded against cycles
    out: &mut Vec<PaintCmd>,
) {
    let Some(tag) = doc.tag_name(id) else { return }; // skip text/comment nodes
    let eff = effective_transform(parent_t, doc.get_attribute(id, "transform"));
    // E55-M3: `filter="url(#f)"` (feGaussianBlur) and `mask="url(#m)"` bracket the
    // WHOLE element (its clip-path + paint) in an offscreen layer. `mask`/`filter`
    // themselves are defs (handled in the render-nothing arm) and are never their
    // own bracket targets. Filter wraps the mask layer (filter applies first, then
    // the mask multiplies — matching the CSS layer order).
    if tag != "mask" && tag != "filter" && tag != "clipPath" {
        let blur = svg_filter_blur(doc, id);
        let mask_cmds = svg_mask_cmds(doc, styled, fonts, images, id, eff, ctx, grads, depth);
        if blur.is_some() || mask_cmds.is_some() {
            if let Some(stddev) = blur {
                out.push(PaintCmd::PushLayer {
                    opacity: 1.0,
                    filter: vec![FilterFn::Blur(stddev)],
                    blend: BlendMode::Normal,
                    mask: None,
                });
            }
            let has_mask = mask_cmds.is_some();
            if let Some(mc) = mask_cmds {
                out.push(PaintCmd::PushSvgMask { mask_cmds: mc });
            }
            walk_svg_clipped(doc, styled, fonts, images, id, tag, eff, ctx, grads, depth, out);
            // Pop innermost-first: mask layer (PopLayer), then filter layer.
            if has_mask {
                out.push(PaintCmd::PopLayer);
            }
            if blur.is_some() {
                out.push(PaintCmd::PopLayer);
            }
            return;
        }
    }
    walk_svg_clipped(doc, styled, fonts, images, id, tag, eff, ctx, grads, depth, out);
}

/// E38-M3 + E55-M3: apply `clip-path` (if any), then walk the element. Factored
/// out of `walk_svg` so the E55-M3 `mask`/`filter` bracket can wrap the clip too.
#[allow(clippy::too_many_arguments)]
fn walk_svg_clipped(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    id: NodeId,
    tag: &str,
    eff: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) {
    // E38-M3: a `<clipPath>` directly walked paints nothing (handled in the
    // render-nothing arm below); but a referencing element with
    // `clip-path="url(#cp)"` is clipped to the union of cp's child shapes.
    // `clipPath` itself is excluded so its own children are never clip targets.
    if tag != "clipPath" {
        if let Some(geoms) = svg_clip_geoms(doc, id, eff) {
            out.push(PaintCmd::PushSvgClip { geoms });
            walk_svg_unclipped(doc, styled, fonts, images, id, tag, eff, ctx, grads, depth, out);
            out.push(PaintCmd::PopClip);
            return;
        }
    }
    walk_svg_unclipped(doc, styled, fonts, images, id, tag, eff, ctx, grads, depth, out);
}

/// E38-M3: the body of `walk_svg` after `clip-path` handling — dispatches one
/// element by `tag` with its already-computed effective transform `eff`.
#[allow(clippy::too_many_arguments)]
fn walk_svg_unclipped(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
    id: NodeId,
    tag: &str,
    eff: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) {
    match tag {
        "g" | "svg" | "a" => {
            let child_ctx = ctx.inherit(doc, id);
            for c in doc.children(id) {
                walk_svg(doc, styled, fonts, images, c, eff, &child_ctx, grads, depth, out);
            }
        }
        "use" => walk_svg_use(doc, styled, fonts, images, id, eff, ctx, grads, depth, out), // E38-M1
        // E55-M1: <image href x y width height> blits a decoded raster (or an
        // already-parsed SVG file) into the local (x,y,w,h) rect, bracketed by the
        // element's effective SVG transform.
        "image" => emit_svg_image(doc, styled, fonts, images, id, eff, out),
        // E38-M1: <symbol> is a template; rendered only via <use>, never directly.
        // E38-M2: <pattern> is a fill template; painted only via fill="url(#id)".
        // E38-M3: a directly-walked <clipPath> paints nothing (it's a clip
        // template, used only via clip-path="url(#id)").
        // E55-M2: a directly-walked <marker> paints nothing (it's a vertex
        // template, used only via marker-start/-mid/-end / the `marker` shorthand).
        // E55-M3: a directly-walked <mask>/<filter> paints nothing — they are
        // defs-like templates, applied only via mask="url(#m)" / filter="url(#f)".
        "defs" | "symbol" | "pattern" | "clipPath" | "marker" | "mask" | "filter"
        | "linearGradient" | "radialGradient" | "stop" | "title" | "desc"
        | "metadata" => {}
        "text" => emit_svg_text(doc, styled, fonts, id, eff, ctx, grads, out),
        _ => {
            // E38-M2: a shape filled by a <pattern> tiles the pattern's children
            // across (clipped to) the shape's bbox; otherwise the normal path.
            if walk_svg_pattern_fill(doc, styled, fonts, images, id, eff, ctx, grads, depth, out) {
                return;
            }
            if let Some(cmd) = build_shape(doc, styled, id, eff, ctx, grads) {
                out.push(cmd);
            }
            // E55-M2: paint vertex markers (marker-start/-end / `marker`) AFTER
            // the shape, oriented by the path tangent (`orient="auto"`).
            emit_svg_markers(doc, styled, fonts, images, id, tag, eff, grads, depth, out);
        }
    }
}

/// E38-M1: max `<use>`-expansion depth, bounding cycles (use → target → use → …).
const SVG_USE_DEPTH_CAP: usize = 32;

/// E38-M1: instantiate a `<use href="#id" x y>`. `use_t` is the use element's
/// effective matrix (parent · its `transform`); we further translate the instance
/// by the use's `x`/`y`. If the target is a `<symbol>`/`<svg>` template, its
/// CHILDREN are walked; otherwise the target element itself. Bounded by `depth`.
#[allow(clippy::too_many_arguments)]
fn walk_svg_use(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
    id: NodeId,
    use_t: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) {
    if depth >= SVG_USE_DEPTH_CAP {
        return; // cycle guard
    }
    let href = doc
        .get_attribute(id, "href")
        .or_else(|| doc.get_attribute(id, "xlink:href"));
    let Some(href) = href else { return };
    let target_id = href.strip_prefix('#').unwrap_or(href);
    let Some(target) = svg_find_by_id(doc, target_id) else {
        return;
    };
    // The instance is translated by x/y after the use's own transform (SVG: a
    // `<use>` is `transform · translate(x,y)`).
    let (x, y) = (attr_f(doc, id, "x"), attr_f(doc, id, "y"));
    let inst_t = to_transform(use_t).pre_concat(Transform::from_translate(x, y));
    let inst_t = [inst_t.sx, inst_t.ky, inst_t.kx, inst_t.sy, inst_t.tx, inst_t.ty];
    // fill/stroke set on the <use> cascade into the instance.
    let child_ctx = ctx.inherit(doc, id);
    match doc.tag_name(target) {
        Some("symbol") | Some("svg") => {
            // Template: render its children (the symbol/svg itself paints nothing).
            for c in doc.children(target) {
                walk_svg(
                    doc,
                    styled,
                    fonts,
                    images,
                    c,
                    inst_t,
                    &child_ctx,
                    grads,
                    depth + 1,
                    out,
                );
            }
        }
        Some(_) => {
            walk_svg(
                doc,
                styled,
                fonts,
                images,
                target,
                inst_t,
                &child_ctx,
                grads,
                depth + 1,
                out,
            );
        }
        None => {}
    }
}

/// E38-M1: DFS the whole document for an element whose `id` attribute equals
/// `target_id` (SVG ids are document-unique in practice). `None` if absent.
fn svg_find_by_id(doc: &Document, target_id: &str) -> Option<NodeId> {
    fn rec(doc: &Document, id: NodeId, target: &str) -> Option<NodeId> {
        if doc.get_attribute(id, "id") == Some(target) {
            return Some(id);
        }
        for c in doc.children(id) {
            if let Some(found) = rec(doc, c, target) {
                return Some(found);
            }
        }
        None
    }
    rec(doc, doc.root(), target_id)
}

/// E38-M2: max tile count per axis for a `<pattern>` fill (mirrors the
/// background-tiling cap so a tiny tile over a huge bbox can't explode).
const SVG_PATTERN_MAX_TILES_PER_AXIS: usize = 4096;

/// E38-M2: if shape `id`'s resolved `fill` is `url(#p)` where `#p` is a
/// `<pattern>` and the shape's user-space bounding box is cheaply computable
/// (rect/circle/ellipse — MVP), tile the pattern's children across the bbox
/// clipped to it, returning `true` (the caller then skips `build_shape`). Any
/// other case returns `false` so the normal solid/gradient/none path runs
/// unchanged (byte-identical). Limitations: clip is the bbox RECT (not the
/// exact shape outline), pattern content coords are treated tile-local, and
/// `patternTransform`/`patternContentUnits` are ignored (roadmap non-goals).
#[allow(clippy::too_many_arguments)]
fn walk_svg_pattern_fill(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore, // E55-M1: SVG `<image>` blits decoded pixels
    id: NodeId,
    eff: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) -> bool {
    if depth >= SVG_USE_DEPTH_CAP {
        return false; // recursion guard (pattern child referencing the shape, etc.)
    }
    // Resolve `fill` like resolve_paints: own style/attr, then inherited ctx.
    let style = doc.get_attribute(id, "style");
    let fill_v = svg_style_prop(style, "fill")
        .or_else(|| doc.get_attribute(id, "fill").map(str::to_string))
        .or_else(|| ctx.fill.clone());
    let Some(fill_v) = fill_v else { return false };
    let Some(pat_id) = parse_url_ref(fill_v.trim()) else {
        return false;
    };
    let Some(pat) = svg_find_by_id(doc, pat_id) else {
        return false;
    };
    if doc.tag_name(pat) != Some("pattern") {
        return false; // url(#…) to a gradient/etc. → normal path resolves it
    }
    // User-space bbox of the filled shape (MVP shapes only).
    let Some(bbox) = shape_user_bbox(doc, id) else {
        return false; // path/polygon/line/… → fall back to the normal path
    };
    if bbox.width <= 0.0 || bbox.height <= 0.0 {
        return false;
    }

    // patternUnits: objectBoundingBox (default) → x/y/w/h are fractions of the
    // bbox; userSpaceOnUse → absolute user-space lengths.
    let obb = doc.get_attribute(pat, "patternUnits") != Some("userSpaceOnUse");
    let (px, py) = (attr_f(doc, pat, "x"), attr_f(doc, pat, "y"));
    let pw = attr_f(doc, pat, "width");
    let ph = attr_f(doc, pat, "height");
    let (tw, th, ox, oy) = if obb {
        (
            pw * bbox.width,
            ph * bbox.height,
            px * bbox.width,
            py * bbox.height,
        )
    } else {
        (pw, ph, px, py)
    };
    if tw <= 0.0 || th <= 0.0 {
        return false; // degenerate tile → paint nothing for this fill
    }

    // Tile origins step by (tw,th) from (bbox.x+ox, bbox.y+oy), covering bbox.
    let nx = ((bbox.width / tw).ceil() as usize + 1).min(SVG_PATTERN_MAX_TILES_PER_AXIS);
    let ny = ((bbox.height / th).ceil() as usize + 1).min(SVG_PATTERN_MAX_TILES_PER_AXIS);

    // Clip to the shape's bbox in DEVICE space (axis-aligned bounds of the
    // transformed bbox corners — exact for axis-aligned transforms, MVP approx
    // for rotation/skew).
    let clip = transformed_bounds(eff, bbox);
    out.push(PaintCmd::PushClip {
        rect: clip,
        radius: [0.0; 4],
    });
    let child_ctx = ctx.inherit(doc, id);
    for j in 0..ny {
        for i in 0..nx {
            let tx = bbox.x + ox + (i as f32) * tw;
            let ty = bbox.y + oy + (j as f32) * th;
            let tile_t = to_transform(eff).pre_concat(Transform::from_translate(tx, ty));
            let tile_t = [
                tile_t.sx, tile_t.ky, tile_t.kx, tile_t.sy, tile_t.tx, tile_t.ty,
            ];
            for c in doc.children(pat) {
                walk_svg(
                    doc,
                    styled,
                    fonts,
                    images,
                    c,
                    tile_t,
                    &child_ctx,
                    grads,
                    depth + 1,
                    out,
                );
            }
        }
    }
    out.push(PaintCmd::PopClip);
    true
}

/// E38-M2: the user-space bounding box of a shape from its attributes, for the
/// MVP-supported tags only (`rect`/`circle`/`ellipse`). `None` for shapes whose
/// bbox isn't cheaply attribute-derivable (`path`/`polygon`/`line`/…) so the
/// caller falls back to the non-pattern fill path.
fn shape_user_bbox(doc: &Document, id: NodeId) -> Option<Rect> {
    match doc.tag_name(id)? {
        "rect" => {
            let w = attr_f(doc, id, "width");
            let h = attr_f(doc, id, "height");
            if w <= 0.0 || h <= 0.0 {
                return None;
            }
            Some(Rect {
                x: attr_f(doc, id, "x"),
                y: attr_f(doc, id, "y"),
                width: w,
                height: h,
            })
        }
        "circle" => {
            let r = attr_f(doc, id, "r");
            if r <= 0.0 {
                return None;
            }
            Some(Rect {
                x: attr_f(doc, id, "cx") - r,
                y: attr_f(doc, id, "cy") - r,
                width: 2.0 * r,
                height: 2.0 * r,
            })
        }
        "ellipse" => {
            let rx = attr_f(doc, id, "rx");
            let ry = attr_f(doc, id, "ry");
            if rx <= 0.0 || ry <= 0.0 {
                return None;
            }
            Some(Rect {
                x: attr_f(doc, id, "cx") - rx,
                y: attr_f(doc, id, "cy") - ry,
                width: 2.0 * rx,
                height: 2.0 * ry,
            })
        }
        _ => None,
    }
}

/// E38-M2: device-space axis-aligned bounds of `rect` mapped through `m`
/// (a,b,c,d,e,f). Exact for axis-aligned transforms; an over-approximation for
/// rotation/skew (acceptable MVP clip).
fn transformed_bounds(m: [f32; 6], rect: Rect) -> Rect {
    let t = to_transform(m);
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ];
    let (mut minx, mut miny) = (f32::INFINITY, f32::INFINITY);
    let (mut maxx, mut maxy) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (cx, cy) in corners {
        let dx = t.sx * cx + t.kx * cy + t.tx;
        let dy = t.ky * cx + t.sy * cy + t.ty;
        minx = minx.min(dx);
        miny = miny.min(dy);
        maxx = maxx.max(dx);
        maxy = maxy.max(dy);
    }
    Rect {
        x: minx,
        y: miny,
        width: maxx - minx,
        height: maxy - miny,
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

/// Build the user-space `SvgGeom` for a basic-shape element, or `None` for an
/// unknown tag (`defs`/…) or a degenerate shape. (Factored from `build_shape`
/// so E38-M3 `<clipPath>` children can reuse the geometry.)
fn build_geom(doc: &Document, id: NodeId) -> Option<SvgGeom> {
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
    Some(geom)
}

/// E38-M3: if element `id` has `clip-path="url(#cp)"` (attribute or
/// `style="clip-path:url(#cp)"`) and `#cp` is a `<clipPath>` with usable child
/// shapes, collect each child's `(SvgGeom, eff_transform)`. `eff_transform` is
/// the referencing element's effective transform (clipPathUnits defaults to
/// userSpaceOnUse → the clip shapes share the referencing element's user space).
/// Returns `None` (paint unclipped) if there's no `clip-path`, `#cp` is missing/
/// not a clipPath, or it has no usable shapes — graceful degradation.
fn svg_clip_geoms(doc: &Document, id: NodeId, eff: [f32; 6]) -> Option<Vec<(SvgGeom, [f32; 6])>> {
    let cp_v = svg_style_prop(doc.get_attribute(id, "style"), "clip-path")
        .or_else(|| doc.get_attribute(id, "clip-path").map(str::to_string))?;
    let cp_id = parse_url_ref(cp_v.trim())?;
    let cp = svg_find_by_id(doc, cp_id)?;
    if doc.tag_name(cp) != Some("clipPath") {
        return None;
    }
    let mut geoms = Vec::new();
    for c in doc.children(cp) {
        if let Some(geom) = build_geom(doc, c) {
            // Each clip child may carry its own `transform` (composed onto the
            // referencing element's effective user→device matrix).
            let ct = effective_transform(eff, doc.get_attribute(c, "transform"));
            geoms.push((geom, ct));
        }
    }
    if geoms.is_empty() {
        None
    } else {
        Some(geoms)
    }
}

/// E55-M3: if element `id` has `filter="url(#f)"` (attribute or
/// `style="filter:url(#f)"`) and `#f` is a `<filter>` whose first `feGaussianBlur`
/// child has a `stdDeviation`, return that std deviation. SVG `stdDeviation` is a
/// gaussian std dev in user px — the SAME unit the CSS `FilterFn::Blur` carries
/// (CSS `blur(Npx)` → `FilterFn::Blur(N)`, which maps N→box radius in raster), so
/// we pass `stdDeviation` straight through as `FilterFn::Blur(stdDeviation)`.
/// `None` (no layer) if there's no filter, `#f` is missing/not a `<filter>`, or it
/// has no `feGaussianBlur` — graceful degradation (other primitives are non-goals).
fn svg_filter_blur(doc: &Document, id: NodeId) -> Option<f32> {
    let f_v = svg_style_prop(doc.get_attribute(id, "style"), "filter")
        .or_else(|| doc.get_attribute(id, "filter").map(str::to_string))?;
    let f_id = parse_url_ref(f_v.trim())?;
    let f = svg_find_by_id(doc, f_id)?;
    if doc.tag_name(f) != Some("filter") {
        return None;
    }
    for c in doc.children(f) {
        if doc.tag_name(c) == Some("feGaussianBlur") {
            // `stdDeviation` may be "x" or "x y"; use the first component.
            let sd = doc.get_attribute(c, "stdDeviation")?;
            let first = sd.split_whitespace().next()?;
            return parse_len(first).filter(|v| *v > 0.0);
        }
    }
    None
}

/// E55-M3: if element `id` has `mask="url(#m)"` (attribute or `style="mask:..."`)
/// and `#m` is a `<mask>` with usable children, render those children into a flat
/// command list (in the referencing element's user space — `maskContentUnits`
/// defaults to userSpaceOnUse). On pop the rasterizer multiplies the element layer
/// by these commands' luminance × alpha (a TRUE luminance mask). `None` (no mask
/// bracket) if there's no mask, `#m` is missing/not a `<mask>`, or it produced no
/// paint — graceful degradation.
#[allow(clippy::too_many_arguments)]
fn svg_mask_cmds(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    id: NodeId,
    eff: [f32; 6],
    ctx: &SvgCtx,
    grads: &GradientRegistry,
    depth: usize,
) -> Option<Vec<PaintCmd>> {
    let m_v = svg_style_prop(doc.get_attribute(id, "style"), "mask")
        .or_else(|| doc.get_attribute(id, "mask").map(str::to_string))?;
    let m_id = parse_url_ref(m_v.trim())?;
    let m = svg_find_by_id(doc, m_id)?;
    if doc.tag_name(m) != Some("mask") {
        return None;
    }
    let mut cmds = Vec::new();
    let child_ctx = ctx.inherit(doc, m);
    for c in doc.children(m) {
        walk_svg(doc, styled, fonts, images, c, eff, &child_ctx, grads, depth, &mut cmds);
    }
    if cmds.is_empty() {
        None
    } else {
        Some(cmds)
    }
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
    let geom = build_geom(doc, id)?;

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

/// E55-M2: a marker placement on a shape — a user-space vertex and the path
/// tangent (radians) there, used to orient an `orient="auto"` marker.
struct MarkerVertex {
    x: f32,
    y: f32,
    angle: f32, // path tangent in radians (atan2)
}

/// E55-M2: extract the (start, end) marker vertices+tangents for a shape, or
/// `None` if the shape has no usable vertices. MVP: `<line>`, `<polyline>`,
/// `<polygon>`, and `<path>` (first move-to + last on-path point, best-effort);
/// other shapes (rect/circle/ellipse) have no marker vertices.
fn marker_vertices(geom: &SvgGeom) -> Option<(MarkerVertex, MarkerVertex)> {
    match geom {
        &SvgGeom::Line { x1, y1, x2, y2 } => {
            let a = (y2 - y1).atan2(x2 - x1);
            Some((
                MarkerVertex { x: x1, y: y1, angle: a },
                MarkerVertex { x: x2, y: y2, angle: a },
            ))
        }
        SvgGeom::Path(ops) => marker_path_vertices(ops),
        _ => None, // rect/ellipse have no path vertices (MVP)
    }
}

/// E55-M2: first/last on-path points of a parsed op list, with tangents. The
/// start tangent points toward the next distinct point; the end tangent comes
/// from the previous distinct point. Returns `None` if fewer than two distinct
/// points are available.
fn marker_path_vertices(ops: &[crate::svg_path::PathOp]) -> Option<(MarkerVertex, MarkerVertex)> {
    use crate::svg_path::PathOp;
    // Flatten ops to their endpoint coordinates (the on-path anchor points).
    let mut pts: Vec<(f32, f32)> = Vec::new();
    for op in ops {
        match *op {
            PathOp::MoveTo(x, y) | PathOp::LineTo(x, y) => pts.push((x, y)),
            PathOp::QuadTo(_, _, x, y) | PathOp::CubicTo(_, _, _, _, x, y) => pts.push((x, y)),
            PathOp::Close => {}
        }
    }
    if pts.len() < 2 {
        return None;
    }
    let (sx, sy) = pts[0];
    let (s2x, s2y) = pts[1];
    let (ex, ey) = *pts.last().unwrap();
    let (e0x, e0y) = pts[pts.len() - 2];
    Some((
        MarkerVertex { x: sx, y: sy, angle: (s2y - sy).atan2(s2x - sx) },
        MarkerVertex { x: ex, y: ey, angle: (ey - e0y).atan2(ex - e0x) },
    ))
}

/// E55-M2: after painting a shape, paint its referenced markers at the start and
/// end vertices, oriented by `orient` (`"auto"` ⇒ path tangent; a number ⇒ that
/// many degrees; default 0). The marker's children are walked with the composed
/// matrix `eff · translate(vertex) · rotate(angle)` (MVP: identity scale,
/// refX/refY=0, markerUnits ignored). A directly-walked `<marker>` paints
/// nothing; here we walk its children explicitly.
#[allow(clippy::too_many_arguments)]
fn emit_svg_markers(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    id: NodeId,
    tag: &str,
    eff: [f32; 6],
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) {
    // Only shapes that have path vertices carry markers.
    if !matches!(tag, "line" | "polyline" | "polygon" | "path") {
        return;
    }
    let style = doc.get_attribute(id, "style");
    let short = svg_style_prop(style, "marker")
        .or_else(|| doc.get_attribute(id, "marker").map(str::to_string));
    let start_ref = svg_style_prop(style, "marker-start")
        .or_else(|| doc.get_attribute(id, "marker-start").map(str::to_string))
        .or_else(|| short.clone());
    let end_ref = svg_style_prop(style, "marker-end")
        .or_else(|| doc.get_attribute(id, "marker-end").map(str::to_string))
        .or(short);
    if start_ref.is_none() && end_ref.is_none() {
        return;
    }
    let Some(geom) = build_geom(doc, id) else {
        return;
    };
    let Some((start, end)) = marker_vertices(&geom) else {
        return;
    };
    if let Some(r) = start_ref.as_deref().and_then(|v| parse_url_ref(v.trim())) {
        paint_one_marker(
            doc, styled, fonts, images, r, &start, eff, grads, depth, out,
        );
    }
    if let Some(r) = end_ref.as_deref().and_then(|v| parse_url_ref(v.trim())) {
        paint_one_marker(doc, styled, fonts, images, r, &end, eff, grads, depth, out);
    }
}

/// E55-M2: resolve `#marker_id` to a `<marker>` and walk its children with the
/// composed marker matrix. `orient="auto"` uses the vertex tangent; a numeric
/// `orient` overrides it (degrees); default 0.
#[allow(clippy::too_many_arguments)]
fn paint_one_marker(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    marker_id: &str,
    v: &MarkerVertex,
    eff: [f32; 6],
    grads: &GradientRegistry,
    depth: usize,
    out: &mut Vec<PaintCmd>,
) {
    if depth >= SVG_USE_DEPTH_CAP {
        return; // cycle guard (a marker child could reference more markers)
    }
    let Some(m) = svg_find_by_id(doc, marker_id) else {
        return;
    };
    if doc.tag_name(m) != Some("marker") {
        return;
    }
    // Orientation: "auto" (and "auto-start-reverse" → treated as auto, MVP) uses
    // the path tangent; a numeric value rotates by that many degrees; default 0.
    let angle_deg = match doc.get_attribute(m, "orient") {
        Some(o) if o.trim().eq_ignore_ascii_case("auto")
            || o.trim().eq_ignore_ascii_case("auto-start-reverse") =>
        {
            v.angle.to_degrees()
        }
        Some(o) => parse_len(o).unwrap_or(0.0),
        None => 0.0,
    };
    // Compose: eff · translate(vertex) · rotate(angle). (MVP: identity scale,
    // refX/refY default 0, markerUnits ignored.)
    let base = to_transform(eff)
        .pre_concat(Transform::from_translate(v.x, v.y))
        .pre_concat(Transform::from_rotate(angle_deg));
    let marker_t = [base.sx, base.ky, base.kx, base.sy, base.tx, base.ty];
    let ctx = SvgCtx::root();
    for c in doc.children(m) {
        walk_svg(
            doc, styled, fonts, images, c, marker_t, &ctx, grads, depth + 1, out,
        );
    }
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
        // E49-M2: SVG offsets are always 0..1 fractions.
        let pos = prop("offset")
            .and_then(|o| parse_offset(&o))
            .map(starfish_style::GradientStopPos::Frac);
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
            features: &[], // E46-M1: SVG text has no font-feature-settings
            kerning: FontKerning::Auto,
            variations: &[], // E46-M3: SVG text has no font-variation-settings
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
            features: Vec::new(), // E46-M1: SVG text has no font-feature-settings
            kerning: FontKerning::Auto,
            variations: Vec::new(), // E46-M3: SVG text has no font-variation-settings
        });
    }
    out.push(PaintCmd::PopTransform);
}

/// E55-M1: emit an SVG `<image href x y width height>`. The href (or `xlink:href`)
/// resolves like any other image through the `ImageStore` (raster → `ImageBlit`;
/// an `*.svg` href → the inline-SVG painter). Because SVG content is transformed
/// by an arbitrary matrix (not necessarily axis-aligned) while the blit's dest is
/// an axis-aligned `Rect`, we bracket the blit with `PushTransform { eff }` /
/// `PopTransform` and paint at the LOCAL `(x,y,w,h)` rect — mirroring how
/// `emit_svg_text` applies its transform. The raster is fit `xMidYMid meet`
/// (the `preserveAspectRatio` default: scaled-to-fit-inside, centered).
fn emit_svg_image(
    doc: &Document,
    styled: &StyledTree,
    fonts: &FontDb,
    images: &ImageStore,
    id: NodeId,
    eff: [f32; 6],
    out: &mut Vec<PaintCmd>,
) {
    let Some(href) = doc
        .get_attribute(id, "href")
        .or_else(|| doc.get_attribute(id, "xlink:href"))
    else {
        return;
    };
    let (x, y) = (attr_f(doc, id, "x"), attr_f(doc, id, "y"));
    let (w, h) = (attr_f(doc, id, "width"), attr_f(doc, id, "height"));
    if w <= 0.0 || h <= 0.0 {
        return; // an `<image>` with no/zero box paints nothing
    }
    let dest = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    out.push(PaintCmd::PushTransform { matrix: eff });
    // An `*.svg` href paints via the inline-SVG painter into the local rect.
    if let Some(parsed) = images.peek_svg(href) {
        emit_svg_into(&parsed.doc, styled, fonts, images, parsed.svg_id, dest, out);
    } else if let Some(img) = images.peek(href) {
        // `xMidYMid meet`: scale to fit inside the rect, centered.
        let (iw, ih) = (img.width as f32, img.height as f32);
        let drect = if iw > 0.0 && ih > 0.0 {
            let s = (w / iw).min(h / ih);
            let (dw, dh) = (iw * s, ih * s);
            Rect {
                x: x + (w - dw) / 2.0,
                y: y + (h - dh) / 2.0,
                width: dw,
                height: dh,
            }
        } else {
            dest
        };
        out.push(PaintCmd::ImageBlit {
            dest: drect,
            src: href.to_string(),
            src_crop: Rect {
                x: 0.0,
                y: 0.0,
                width: iw,
                height: ih,
            },
            smooth: false,
            blend: BlendMode::Normal,
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
                    features: style.effective_font_features(), // E46-M2
                    kerning: style.font_kerning,
                    variations: style.font_variations().to_vec(), // E46-M3
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
                features: style.effective_font_features(), // E46-M2
                kerning: style.font_kerning,
                variations: style.font_variations().to_vec(), // E46-M3
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
        features: style.font_features(), // E46-M1
        kerning: style.font_kerning,
        variations: style.font_variations(), // E46-M3
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
                features: style.effective_font_features(), // E46-M2
                kerning: style.font_kerning,
                variations: style.font_variations().to_vec(), // E46-M3
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
        features: style.effective_font_features(), // E46-M2
        kerning: style.font_kerning,
        variations: style.font_variations().to_vec(), // E46-M3
    });

    // E41-M3: text-emphasis marks — only for real text runs, never markers.
    if b.kind() == BoxKind::TextRun {
        if let Some(mark) = &style.text_emphasis {
            emit_emphasis_marks(text, &c, style, mark, &lm, fonts, &q, out);
        }
    }

    // text-decoration lines — only for real text runs, never markers (§4.1).
    if b.kind() != BoxKind::TextRun {
        return;
    }
    let deco = style.text_decoration_line;
    if deco.is_none() {
        return;
    }
    // E41-M2: explicit thickness overrides the derived default (auto = None).
    let thickness = style
        .text_decoration_thickness
        .unwrap_or((style.font_size / 16.0).max(1.0));
    // E41-M1: decoration color defaults to the element's text color.
    let color = style.text_decoration_color.unwrap_or(style.color);
    let deco_style = style.text_decoration_style;
    let baseline = c.y + lm.ascent;
    let mut line = |y: f32| {
        // E41-M1: `Solid` keeps the single-rect path (byte-identical to the
        // pre-E41 output when the color also defaults). Other styles draw a
        // variant of the line; gate them so the common case is unchanged.
        match deco_style {
            TextDecorationStyle::Solid => out.push(fill(
                Rect {
                    x: c.x,
                    y,
                    width: c.width,
                    height: thickness,
                },
                color,
            )),
            _ => draw_decoration_styled(out, c.x, y, c.width, thickness, color, deco_style),
        }
    };
    if deco.contains(TextDecorationLine::UNDERLINE) {
        // E41-M2: text-underline-offset moves the underline down (default 0).
        line(baseline + 1.0 + style.text_underline_offset); // just below baseline
    }
    if deco.contains(TextDecorationLine::LINE_THROUGH) {
        line(baseline - lm.ascent * 0.3); // ~middle / x-height
    }
    if deco.contains(TextDecorationLine::OVERLINE) {
        line(c.y); // top of the content box
    }
}

/// E41-M3: draw a `text-emphasis` mark centered over (or under) each non-space
/// base character of the run. The mark color defaults to the element's `color`.
///
/// Per-char x-centers come from cumulative `advance_width` of the char prefix
/// (so letter/word-spacing are honored). Shapes:
/// - `Dot`/`Sesame`: a small disc, radius ~0.1em. `open` → a stroked ring.
///   (Sesame is approximated by a dot.)
/// - `Circle`: a slightly larger disc/ring (~0.13em).
/// - `DoubleCircle`: an outer ring + an inner disc/ring (approximation).
/// - `Triangle`: a small filled/stroked triangle (`SvgGeom::Path`).
/// - `Str(s)`: the string drawn as a tiny `GlyphRun` (~0.5em) centered per char.
#[allow(clippy::too_many_arguments)]
fn emit_emphasis_marks(
    text: &str,
    c: &Rect,
    style: &ComputedStyle,
    mark: &EmphasisMark,
    lm: &starfish_layout::LineMetrics,
    fonts: &FontDb,
    q: &FontQuery,
    out: &mut Vec<PaintCmd>,
) {
    let color = style.text_emphasis_color.unwrap_or(style.color);
    if color.a == 0 {
        return;
    }
    let em = style.font_size;
    // Mark band: over → above the content top; under → below the descent.
    let mark_size = em * 0.25;
    let cy = if style.text_emphasis_over {
        c.y - mark_size * 0.6
    } else {
        c.y + lm.ascent + lm.descent + mark_size * 0.6
    };

    let chars: Vec<char> = text.chars().collect();
    let mut prefix = String::new();
    let mut prev_w = 0.0f32;
    for ch in chars {
        prefix.push(ch);
        let cum = fonts.advance_width(&prefix, q);
        let advance = cum - prev_w;
        let center_x = c.x + prev_w + advance / 2.0;
        prev_w = cum;
        // Skip whitespace base chars (no mark over a space).
        if ch.is_whitespace() {
            continue;
        }
        emit_one_mark(center_x, cy, em, mark, color, q, fonts, out);
    }
}

/// E41-M3: emit one emphasis mark centered at `(cx, cy)`.
#[allow(clippy::too_many_arguments)]
fn emit_one_mark(
    cx: f32,
    cy: f32,
    em: f32,
    mark: &EmphasisMark,
    color: Rgba,
    q: &FontQuery,
    fonts: &FontDb,
    out: &mut Vec<PaintCmd>,
) {
    let (fill_c, stroke_c, sw) = if mark.filled {
        (Some(color), None, 0.0)
    } else {
        (None, Some(color), (em * 0.04).max(0.75))
    };
    match &mark.shape {
        // Sesame is approximated by a dot (a small disc).
        EmphasisShape::Dot | EmphasisShape::Sesame => {
            let r = em * 0.1;
            emit_shape(
                SvgGeom::Ellipse { cx, cy, rx: r, ry: r },
                fill_c,
                stroke_c,
                sw,
                out,
            );
        }
        EmphasisShape::Circle => {
            let r = em * 0.13;
            emit_shape(
                SvgGeom::Ellipse { cx, cy, rx: r, ry: r },
                fill_c,
                stroke_c,
                sw,
                out,
            );
        }
        // Approximation: an outer ring + an inner disc/ring.
        EmphasisShape::DoubleCircle => {
            let r = em * 0.14;
            emit_shape(
                SvgGeom::Ellipse { cx, cy, rx: r, ry: r },
                None,
                Some(color),
                (em * 0.04).max(0.75),
                out,
            );
            let ri = em * 0.06;
            emit_shape(
                SvgGeom::Ellipse { cx, cy, rx: ri, ry: ri },
                fill_c,
                stroke_c,
                sw,
                out,
            );
        }
        EmphasisShape::Triangle => {
            let r = em * 0.13;
            // Upward-pointing equilateral-ish triangle centered on (cx, cy).
            let pts = [
                (cx, cy - r),
                (cx - r, cy + r),
                (cx + r, cy + r),
            ];
            emit_shape(
                SvgGeom::Path(crate::svg_path::points_to_ops(&pts, true)),
                fill_c,
                stroke_c,
                sw,
                out,
            );
        }
        // Draw the string small + centered as its own glyph run.
        EmphasisShape::Str(s) => {
            let size = (em * 0.5).max(1.0);
            let w = fonts.advance_width(s, q) * (size / em.max(1.0));
            let mark_lm = fonts.line_metrics(&FontQuery { size, ..*q });
            out.push(PaintCmd::GlyphRun {
                origin: (cx - w / 2.0, cy - mark_lm.ascent / 2.0),
                text: s.clone(),
                font_size: size,
                weight: q.weight,
                style: q.style,
                family: q.family.to_vec(),
                color,
                ascent: mark_lm.ascent,
                letter_spacing: 0.0,
                word_spacing: 0.0,
                features: q.features.to_vec(), // E46-M1
                kerning: q.kerning,
                variations: q.variations.to_vec(), // E46-M3
            });
        }
    }
}

/// E41-M1: draw a non-`Solid` decoration line at `(x,y)` over `width`.
/// `y` is the top of the (solid) line rect; `thickness` its height.
///
/// - `Double`: two thinner rects with a gap (top + bottom of a 3×-thickness band).
/// - `Dotted`/`Dashed`: a `StrokeLine` reusing the E13-M4 dashed/dotted border
///   mechanism, centered vertically on the solid line.
/// - `Wavy`: a zigzag approximated by short `StrokeLine` segments alternating
///   up/down by ~`thickness` over a one-period width of ~`6*thickness`.
fn draw_decoration_styled(
    out: &mut Vec<PaintCmd>,
    x: f32,
    y: f32,
    width: f32,
    thickness: f32,
    color: Rgba,
    style: TextDecorationStyle,
) {
    match style {
        TextDecorationStyle::Solid => unreachable!("Solid handled by the fast path"),
        TextDecorationStyle::Double => {
            // Two thin rects separated by a `thickness` gap (total band 3×).
            let thin = (thickness * 0.5).max(1.0);
            out.push(fill(
                Rect { x, y, width, height: thin },
                color,
            ));
            out.push(fill(
                Rect {
                    x,
                    y: y + thickness * 2.0,
                    width,
                    height: thin,
                },
                color,
            ));
        }
        TextDecorationStyle::Dotted | TextDecorationStyle::Dashed => {
            let bs = if matches!(style, TextDecorationStyle::Dotted) {
                BorderStyle::Dotted
            } else {
                BorderStyle::Dashed
            };
            let cy = y + thickness / 2.0;
            out.push(PaintCmd::StrokeLine {
                from: (x, cy),
                to: (x + width, cy),
                width: thickness,
                color,
                style: bs,
            });
        }
        TextDecorationStyle::Wavy => {
            // Zigzag: alternate up/down by `amp` each half-period. The line stays
            // within a band of height ~`2*amp` centered on the solid line.
            let amp = thickness.max(1.5);
            let period = (thickness * 6.0).max(6.0);
            let half = period / 2.0;
            let cy = y + thickness / 2.0;
            let mut px = x;
            let mut up = true; // start going up
            let mut py = cy + amp; // begin at the low point
            while px < x + width {
                let nx = (px + half).min(x + width);
                let ny = if up { cy - amp } else { cy + amp };
                out.push(PaintCmd::StrokeLine {
                    from: (px, py),
                    to: (nx, ny),
                    width: thickness,
                    color,
                    style: BorderStyle::Solid,
                });
                px = nx;
                py = ny;
                up = !up;
            }
        }
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

    // --- E41-M1: text-decoration-color / -style ---

    #[test]
    fn decoration_color_overrides_text_color() {
        // text color black, decoration color red → the underline rect is red.
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;\
             text-decoration:underline red}",
        );
        let fills: Vec<&PaintCmd> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .collect();
        assert_eq!(fills.len(), 1, "expected one underline rect: {cmds:?}");
        let color = match fills[0] {
            PaintCmd::FillRect { color, .. } => *color,
            _ => unreachable!(),
        };
        assert_eq!(color, Rgba { r: 255, g: 0, b: 0, a: 255 });
    }

    #[test]
    fn decoration_double_emits_two_rects() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration:underline double}",
        );
        let fills = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .count();
        assert_eq!(fills, 2, "double underline → two rects: {cmds:?}");
    }

    #[test]
    fn decoration_dotted_emits_stroke_line() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration:underline dotted}",
        );
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::StrokeLine { style: BorderStyle::Dotted, .. }
            )),
            "dotted underline → a dotted StrokeLine: {cmds:?}"
        );
        // No FillRect underline in the dotted path.
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::FillRect { .. })),
            "dotted underline must not emit a FillRect: {cmds:?}"
        );
    }

    #[test]
    fn decoration_wavy_emits_stroke_segments() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration:underline wavy}",
        );
        let segs = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::StrokeLine { style: BorderStyle::Solid, .. }))
            .count();
        assert!(segs >= 2, "wavy underline → >=2 zigzag segments: {cmds:?}");
    }

    #[test]
    fn decoration_solid_default_byte_identical() {
        // Solid + default color must keep the single-rect path (one FillRect,
        // text color), unchanged from the pre-E41 behavior.
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;text-decoration:underline}",
        );
        let fills: Vec<&PaintCmd> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .collect();
        assert_eq!(fills.len(), 1, "solid default → one rect: {cmds:?}");
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::StrokeLine { .. })),
            "solid default → no StrokeLine: {cmds:?}"
        );
    }

    // --- E41-M2: text-decoration-thickness / text-underline-offset ---

    #[test]
    fn decoration_thickness_sets_rect_height() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration:underline;text-decoration-thickness:4px}",
        );
        let fills: Vec<&PaintCmd> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { .. }))
            .collect();
        assert_eq!(fills.len(), 1, "expected one underline rect: {cmds:?}");
        let height = match fills[0] {
            PaintCmd::FillRect { rect, .. } => rect.height,
            _ => unreachable!(),
        };
        assert_eq!(height, 4.0, "thickness:4px → rect height 4: {cmds:?}");
    }

    #[test]
    fn decoration_underline_offset_lowers_y() {
        // The same paragraph with/without a 6px offset: the offset underline's
        // y is exactly 6px lower than the default.
        let underline_y = |css: &str| {
            let cmds = list("<html><body><p>hi</p></body></html>", css);
            cmds.iter()
                .find_map(|c| match c {
                    PaintCmd::FillRect { rect, .. } => Some(rect.y),
                    _ => None,
                })
                .expect("an underline FillRect")
        };
        let base = underline_y(
            "body{margin:0} p{margin:0;font-size:20px;text-decoration:underline}",
        );
        let offset = underline_y(
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration:underline;text-underline-offset:6px}",
        );
        assert_eq!(offset, base + 6.0, "offset:6px → underline 6px lower");
    }

    // --- E41-M3: text-emphasis marks ---

    #[test]
    fn emphasis_emits_one_mark_per_nonspace_char() {
        // "abcd" → 4 base chars → 4 emphasis-mark SvgShapes (above the run).
        let cmds = list(
            "<html><body><p>abcd</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;text-emphasis:filled dot}",
        );
        let gy = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::GlyphRun { origin, .. } => Some(origin.1),
                _ => None,
            })
            .expect("a glyph run");
        let marks: Vec<&PaintCmd> = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::SvgShape { .. }))
            .collect();
        assert_eq!(marks.len(), 4, "one mark per base char: {cmds:?}");
        // Over marks sit above the content top (≈ glyph origin y).
        for m in &marks {
            if let PaintCmd::SvgShape { bbox, .. } = m {
                assert!(bbox.y < gy, "over mark above run top: {bbox:?} vs {gy}");
            }
        }
    }

    #[test]
    fn emphasis_color_overrides_text_color() {
        let cmds = list(
            "<html><body><p>x</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;\
             text-emphasis:filled dot red}",
        );
        let colored = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::SvgShape { fill: Some(SvgPaint::Color(col)), .. }
                if col.r == 255 && col.g == 0 && col.b == 0
        ));
        assert!(colored, "expected a red emphasis mark: {cmds:?}");
    }

    #[test]
    fn emphasis_open_uses_stroke() {
        let cmds = list(
            "<html><body><p>x</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;text-emphasis:open circle}",
        );
        let stroked = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::SvgShape { fill: None, stroke: Some(_), .. }
        ));
        assert!(stroked, "open mark → stroked (no fill): {cmds:?}");
    }

    #[test]
    fn emphasis_under_below_run() {
        let cmds = list(
            "<html><body><p>x</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-emphasis:filled dot;text-emphasis-position:under}",
        );
        let (_, gy, _) = glyph_with_origin(&cmds, "x");
        let below = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::SvgShape { bbox, .. } if bbox.y > gy
        ));
        assert!(below, "under mark below run top: {cmds:?}");
    }

    #[test]
    fn no_emphasis_emits_no_marks() {
        // No text-emphasis → no SvgShape marks (byte-identical text path).
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::SvgShape { .. })),
            "no marks without text-emphasis: {cmds:?}"
        );
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
    fn marker_pseudo_colors_bullet() {
        // E35-M1: `li::marker{color:#f00}` paints the default bullet glyph red.
        let cmds = list(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0} li::marker { color: #ff0000 }",
        );
        let bullet = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, text, .. } if text == "\u{2022}" => Some(*color),
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
            "expected a red bullet: {cmds:?}"
        );
    }

    #[test]
    fn marker_pseudo_content_replaces_text() {
        // E35-M1: `::marker{content:"X "}` replaces the bullet glyph text.
        let cmds = list(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0} li::marker { content: \"X \" }",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "X ")),
            "expected replaced marker text 'X ': {cmds:?}"
        );
        // The default bullet glyph must be gone.
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")),
            "default bullet should be replaced: {cmds:?}"
        );
    }

    #[test]
    fn first_letter_pseudo_enlarges_and_colors() {
        // E35-M3: `p::first-letter{font-size:2em;color:#c00}` → the "H" glyph
        // run paints red at the enlarged size; the rest stays default.
        let cmds = list(
            "<html><body><p>Hello</p></body></html>",
            "body{margin:0} p{font-size:16px;color:#000} \
             p::first-letter { font-size: 2em; color: #cc0000 }",
        );
        let h = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun {
                text,
                color,
                font_size,
                ..
            } if text == "H" => Some((*color, *font_size)),
            _ => None,
        });
        assert_eq!(
            h,
            Some((
                Rgba {
                    r: 0xcc,
                    g: 0,
                    b: 0,
                    a: 255
                },
                32.0
            )),
            "expected a red 32px 'H': {cmds:?}"
        );
        // The remainder "ello" keeps the default black, 16px.
        let rest = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun {
                text,
                color,
                font_size,
                ..
            } if text == "ello" => Some((*color, *font_size)),
            _ => None,
        });
        assert_eq!(
            rest,
            Some((
                Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255
                },
                16.0
            )),
            "expected default 'ello': {cmds:?}"
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

    // E54-M1: positioned bucket sorts by z-index (higher paints last/on top).
    #[test]
    fn paint_order_positioned_sorted_by_z_index() {
        // Two overlapping abspos boxes: #a (FIRST in source, z-index:2, red) and
        // #b (SECOND in source, z-index:1, blue). The higher z (#a) must paint
        // LAST despite coming first in source order.
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='a'></div>\
             <div id='b'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #a{position:absolute;top:0;left:0;width:30px;height:20px;background:#ff0000;z-index:2} \
             #b{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff;z-index:1}",
        );
        let red = first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("red z2");
        let blue = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("blue z1");
        assert!(blue < red, "z1 blue {blue} before z2 red {red} (z-index, not source)");
    }

    // E54-M1: equal z-index keeps source/tree order (stable sort).
    #[test]
    fn paint_order_positioned_equal_z_keeps_tree_order() {
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='a'></div>\
             <div id='b'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #a{position:absolute;top:0;left:0;width:30px;height:20px;background:#ff0000;z-index:1} \
             #b{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff;z-index:1}",
        );
        let red = first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("red");
        let blue = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("blue");
        assert!(red < blue, "equal z: source order (red {red} before blue {blue})");
    }

    // E54-M2: a high-z child inside a low-z positioned parent (which establishes a
    // stacking context because it has z-index) is CONFINED to the parent's context.
    // The child's z:100 cannot lift it above the parent's z:2 sibling — the whole
    // parent subtree (incl. child) paints before the sibling.
    #[test]
    fn paint_order_stacking_context_confines_child() {
        // #parent (z:1, red) contains #child (z:100, green); #sibling (z:2, blue)
        // is a sibling of #parent. Despite child z:100 > sibling z:2, the child is
        // confined to parent's z:1 context, so sibling (z:2) paints on top of the
        // whole parent subtree → child green before sibling blue.
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='parent'><div id='child'></div></div>\
             <div id='sibling'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #parent{position:absolute;top:0;left:0;width:30px;height:20px;background:#ff0000;z-index:1} \
             #child{position:absolute;top:0;left:0;width:10px;height:10px;background:#00ff00;z-index:100} \
             #sibling{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff;z-index:2}",
        );
        let child = first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("child green");
        let sibling = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("sibling blue");
        assert!(
            child < sibling,
            "z:100 child confined to z:1 parent context paints before z:2 sibling \
             (child {child} before sibling {sibling})"
        );
    }

    // E54-M2: a z-index:auto positioned parent does NOT establish a stacking
    // context, so its positioned child bubbles up into the nearest context and can
    // interleave by z-index with the parent's siblings.
    #[test]
    fn paint_order_auto_positioned_parent_bubbles_child() {
        // #parent (position:absolute, z-index:auto → no context) contains #child
        // (z:5, green); #sibling (z:3, blue) is a sibling of #parent. The child
        // bubbles into #wrap's context, so child z:5 paints ABOVE sibling z:3.
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='parent'><div id='child'></div></div>\
             <div id='sibling'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #parent{position:absolute;top:0;left:0;width:30px;height:20px;background:#ff0000} \
             #child{position:absolute;top:0;left:0;width:10px;height:10px;background:#00ff00;z-index:5} \
             #sibling{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff;z-index:3}",
        );
        let child = first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("child green");
        let sibling = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("sibling blue");
        assert!(
            sibling < child,
            "auto-positioned parent does not confine: z:5 child bubbles above z:3 \
             sibling (sibling {sibling} before child {child})"
        );
    }

    // E54-M3: a negative-z positioned child paints BEHIND its context's in-flow
    // content (CSS 2.1 layer 2 before layers 3-5), while a positive-z one stays in
    // front (layer 7). #wrap holds in-flow red text/bg; #neg (z:-1, green) must
    // paint before the in-flow content; #pos (z:1, blue) after it.
    #[test]
    fn paint_order_negative_z_behind_inflow() {
        // E54-M3
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='neg'></div>\
             <div id='content'>x</div>\
             <div id='pos'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #neg{position:absolute;top:0;left:0;width:30px;height:20px;background:#00ff00;z-index:-1} \
             #content{background:#ff0000;height:20px} \
             #pos{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff;z-index:1}",
        );
        let neg = first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("neg green");
        let content =
            first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("in-flow red bg");
        let pos = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("pos blue");
        assert!(neg < content, "z:-1 child {neg} behind in-flow content {content}");
        assert!(content < pos, "in-flow content {content} behind z:1 child {pos}");
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

    // E55-M1: an SVG `<image href x y width height>` with the image decoded emits
    // an ImageBlit at the local (x,y,w,h) rect, bracketed by PushTransform/PopTransform.
    #[test]
    fn svg_image_emits_imageblit_at_rect() {
        let cmds = list_with_fixture(
            "<html><body><svg width='100' height='100'>\
             <image href='px.png' x='10' y='10' width='40' height='40'/></svg></body></html>",
            "body{margin:0}",
        );
        // The blit must sit between a PushTransform and a PopTransform (the
        // element's effective SVG matrix).
        let blit_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::ImageBlit { src, .. } if src == "px.png"))
            .expect("an ImageBlit for px.png");
        assert!(
            matches!(cmds[blit_i - 1], PaintCmd::PushTransform { .. }),
            "blit must follow a PushTransform: {cmds:?}"
        );
        assert!(
            matches!(cmds[blit_i + 1], PaintCmd::PopTransform),
            "blit must precede a PopTransform: {cmds:?}"
        );
        let (dest, src_crop) = match &cmds[blit_i] {
            PaintCmd::ImageBlit { dest, src_crop, .. } => (*dest, *src_crop),
            _ => unreachable!(),
        };
        // 2×2 source fit `xMidYMid meet` into a 40×40 rect → fills it at (10,10).
        assert_eq!(
            dest,
            Rect {
                x: 10.0,
                y: 10.0,
                width: 40.0,
                height: 40.0
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

    // E55-M1: the legacy `xlink:href` attribute works too.
    #[test]
    fn svg_image_xlink_href_works() {
        let cmds = list_with_fixture(
            "<html><body><svg width='100' height='100'>\
             <image xlink:href='px.png' x='0' y='0' width='20' height='20'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::ImageBlit { src, .. } if src == "px.png")),
            "xlink:href should emit an ImageBlit: {cmds:?}"
        );
    }

    // E55-M1: `xMidYMid meet` centers a non-square fit. A 2×2 source into a
    // 40×80 rect scales to 40×40, centered vertically → dest at (0,20,40,40).
    #[test]
    fn svg_image_meet_centers_fit() {
        let cmds = list_with_fixture(
            "<html><body><svg width='100' height='100'>\
             <image href='px.png' x='0' y='0' width='40' height='80'/></svg></body></html>",
            "body{margin:0}",
        );
        let dest = cmds.iter().find_map(|c| match c {
            PaintCmd::ImageBlit { dest, src, .. } if src == "px.png" => Some(*dest),
            _ => None,
        });
        assert_eq!(
            dest,
            Some(Rect {
                x: 0.0,
                y: 20.0,
                width: 40.0,
                height: 40.0
            })
        );
    }

    // --- E55-M2: <marker> vertex markers ---

    // A `<line marker-end="url(#arrow)">` paints the arrow marker's shape at the
    // line's end vertex (100,0), oriented to the rightward tangent (angle 0).
    #[test]
    fn svg_marker_end_paints_at_end_vertex() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <defs><marker id='arrow'><path d='M0,0 L10,5 L0,10 z' fill='red'/></marker></defs>\
             <line x1='0' y1='0' x2='100' y2='0' stroke='black' marker-end='url(#arrow)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        // The marker's path shape must be emitted, placed at (100,0).
        let marker_shape = svg_shapes(&cmds).into_iter().find(|c| matches!(
            c,
            PaintCmd::SvgShape { geom: SvgGeom::Path(_), fill: Some(SvgPaint::Color(col)), .. }
                if *col == red()
        ));
        let Some(PaintCmd::SvgShape { transform, .. }) = marker_shape else {
            panic!("marker arrow path not emitted: {cmds:?}");
        };
        // tangent = 0 (rightward) ⇒ rotation is identity; translate to (100,0).
        assert!(
            approx6(*transform, [1.0, 0.0, 0.0, 1.0, 100.0, 0.0], 1e-3),
            "marker should sit at end vertex with rightward tangent: {transform:?}"
        );
    }

    // A directly-walked `<marker>` paints nothing (it is a vertex template).
    #[test]
    fn svg_marker_template_paints_nothing() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <marker id='arrow'><path d='M0,0 L10,5 L0,10 z' fill='red'/></marker>\
             </svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            svg_shapes(&cmds).is_empty(),
            "a directly-walked marker is a non-rendered template: {cmds:?}"
        );
    }

    // The `marker` shorthand applies to BOTH start and end vertices.
    #[test]
    fn svg_marker_shorthand_applies_to_start_and_end() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <defs><marker id='dot'><circle cx='0' cy='0' r='3' fill='red'/></marker></defs>\
             <line x1='0' y1='0' x2='100' y2='0' stroke='black' marker='url(#dot)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        // Two marker circles: one at (0,0), one at (100,0).
        let positions: Vec<[f32; 6]> = svg_shapes(&cmds)
            .into_iter()
            .filter_map(|c| match c {
                PaintCmd::SvgShape {
                    geom: SvgGeom::Ellipse { .. },
                    fill: Some(SvgPaint::Color(col)),
                    transform,
                    ..
                } if *col == red() => Some(*transform),
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 2, "shorthand paints start + end: {cmds:?}");
        assert!(positions.iter().any(|t| approx6(*t, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 1e-3)));
        assert!(positions.iter().any(|t| approx6(*t, [1.0, 0.0, 0.0, 1.0, 100.0, 0.0], 1e-3)));
    }

    // `orient="auto"` rotates the marker to the path tangent. A vertical line
    // (tangent pointing down, +90°) rotates the marker 90°.
    #[test]
    fn svg_marker_orient_auto_rotates_to_tangent() {
        let cmds = list(
            "<html><body><svg width='100' height='200'>\
             <defs><marker id='a' orient='auto'><path d='M0,0 L10,0 z' fill='red'/></marker></defs>\
             <line x1='0' y1='0' x2='0' y2='100' stroke='black' marker-end='url(#a)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let marker = svg_shapes(&cmds).into_iter().find_map(|c| match c {
            PaintCmd::SvgShape {
                geom: SvgGeom::Path(_),
                fill: Some(SvgPaint::Color(col)),
                transform,
                ..
            } if *col == red() => Some(*transform),
            _ => None,
        });
        let t = marker.expect("oriented marker path not emitted");
        // rotate(90°)·translate(0,100): cos90≈0, sin90≈1 ⇒ [0,1,-1,0,0,100].
        assert!(
            approx6(t, [0.0, 1.0, -1.0, 0.0, 0.0, 100.0], 1e-3),
            "marker should rotate 90° to the downward tangent: {t:?}"
        );
    }

    // A polyline's start marker sits at the first point, oriented toward the 2nd.
    #[test]
    fn svg_marker_polyline_start_vertex() {
        let cmds = list(
            "<html><body><svg width='200' height='100'>\
             <defs><marker id='m'><circle cx='0' cy='0' r='2' fill='red'/></marker></defs>\
             <polyline points='10,10 50,10 90,50' fill='none' stroke='black' marker-start='url(#m)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let pos = svg_shapes(&cmds).into_iter().find_map(|c| match c {
            PaintCmd::SvgShape {
                geom: SvgGeom::Ellipse { .. },
                fill: Some(SvgPaint::Color(col)),
                transform,
                ..
            } if *col == red() => Some(*transform),
            _ => None,
        });
        let t = pos.expect("polyline start marker not emitted");
        assert!(
            approx6(t, [1.0, 0.0, 0.0, 1.0, 10.0, 10.0], 1e-3),
            "start marker at first polyline point: {t:?}"
        );
    }

    // E53-M1: a `list-style-image: url(px.png)` paints the decoded image as the
    // list marker (an ImageBlit carrying that url), not a bullet glyph.
    #[test]
    fn list_style_image_marker_emits_imageblit() {
        let cmds = list_with_fixture(
            "<html><body><ul style='list-style-image:url(px.png)'><li>a</li></ul></body></html>",
            "body{margin:0}",
        );
        let marker_blit = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::ImageBlit { src, .. } if src == "px.png"
        ));
        assert!(marker_blit, "the marker image should emit an ImageBlit");
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

    // --- E61-M2: <iframe>/<embed>/<object> placeholder ---

    #[test]
    fn iframe_emits_border_and_src_label() {
        let cmds = list_with_fixture(
            "<html><body><iframe src='page.html' width='200' height='100'></iframe></body></html>",
            "body{margin:0}",
        );
        // No cross-document load → no blit.
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })));
        // 4 grey (0x80) placeholder border edges.
        let grey = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 0x80 && color.g == 0x80 && color.b == 0x80))
            .count();
        assert_eq!(grey, 4, "expected 4 border rects: {cmds:?}");
        // The src URL drawn as a glyph run.
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "page.html")),
            "expected a src label glyph run: {cmds:?}"
        );
    }

    #[test]
    fn object_emits_data_label() {
        let cmds = list_with_fixture(
            "<html><body><object data='doc.pdf' width='80' height='60'></object></body></html>",
            "body{margin:0}",
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "doc.pdf")),
            "expected a data label glyph run: {cmds:?}"
        );
        let grey = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.r == 0x80 && color.g == 0x80 && color.b == 0x80))
            .count();
        assert_eq!(grey, 4, "expected 4 border rects: {cmds:?}");
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
        // E61-M3: the 2×2 poster is `object-fit: contain`-fitted into the 100×50
        // box: scale = min(100/2, 50/2) = 25 → 50×50, centered (x = 25, y = 0).
        assert_eq!((dest.width, dest.height), (50.0, 50.0));
        assert_eq!((dest.x, dest.y), (25.0, 0.0));
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

    // --- E61-M1: media controls chrome ---

    #[test]
    fn video_controls_emits_control_bar_and_play_triangle() {
        let cmds = list_with_files(
            "<html><body><video controls width='200' height='120'></video></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        // The dark control bar: a black-ish FillRect (~0.6 alpha) along the
        // bottom 24px, spanning the full 200px width.
        let bar = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, rect, .. }
                    if color.a < 255 && color.r == 0 && color.g == 0 && color.b == 0
                        && rect.width == 200.0 && (rect.height - 24.0).abs() < 0.01
                        && (rect.y - (120.0 - 24.0)).abs() < 0.01
            )
        });
        assert!(bar, "expected a bottom control bar: {cmds:?}");
        // The timeline track + filled head: rounded (radius>0) light FillRects.
        let track = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { radius, .. } if radius[0] > 0.0))
            .count();
        assert!(track >= 2, "expected timeline track + head rects: {cmds:?}");
        // A play triangle (a filled Path) somewhere in the list.
        let tri = cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::SvgShape { geom: SvgGeom::Path(_), .. }));
        assert!(tri, "expected a play triangle: {cmds:?}");
    }

    #[test]
    fn audio_controls_emits_control_bar() {
        let cmds = list_with_files(
            "<html><body><audio controls></audio></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        let bar = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::FillRect { color, .. }
                    if color.a < 255 && color.r == 0 && color.g == 0 && color.b == 0
            )
        });
        assert!(bar, "expected a control bar for <audio controls>: {cmds:?}");
        // Rounded timeline rects present.
        let track = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::FillRect { radius, .. } if radius[0] > 0.0))
            .count();
        assert!(track >= 2, "expected timeline rects: {cmds:?}");
    }

    #[test]
    fn video_without_controls_has_no_control_bar() {
        // No `controls` → byte-identical to E15-M3 (placeholder + triangle only).
        let cmds = list_with_files(
            "<html><body><video width='200' height='120'></video></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        // No semi-transparent black bar, no rounded FillRects.
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                PaintCmd::FillRect { color, .. } if color.a < 255 && color.r == 0
            )),
            "no controls → no bar: {cmds:?}"
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::FillRect { radius, .. } if radius[0] > 0.0)),
            "no controls → no rounded track: {cmds:?}"
        );
    }

    #[test]
    fn video_poster_contain_fit_letterboxes() {
        // E61-M3: a wide 4×1 poster in a 100×100 box → object-fit: contain
        // scales to 100×25 (scale = min(100/4, 100/1) = 25), centered vertically
        // (y = (100-25)/2 = 37.5), NOT stretched to fill the 100×100 box.
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-vpc-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::new(4, 1);
        img.save(dir.join("wide.png")).unwrap();

        let doc = parse(
            "<html><body><video poster='wide.png' width='100' height='100'></video></body></html>",
        );
        let sheet = parse_stylesheet("body{margin:0}");
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        images.get("wide.png");
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        let cmds = build_display_list(&root, &styled, &fonts, &images, &doc);

        let dest = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::ImageBlit { dest, .. } => Some(*dest),
                _ => None,
            })
            .expect("a poster ImageBlit");
        assert_eq!((dest.width, dest.height), (100.0, 25.0));
        assert_eq!((dest.x, dest.y), (0.0, 37.5));
    }

    #[test]
    fn track_inside_video_emits_no_box() {
        // E61-M3: <track> is metadata (display:none); it must not panic or add a
        // box. The video still paints its placeholder.
        let cmds = list_with_files(
            "<html><body><video width='100' height='50'><track kind='subtitles' \
             src='s.vtt'></video></body></html>",
            "body{margin:0}",
            &[],
            &[],
        );
        // The video placeholder (dark box) is present; no ImageBlit / no extra box
        // from the <track>.
        assert!(cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, .. } if color.r == 0x33 && color.g == 0x33 && color.b == 0x33
        )));
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

    // --- E32-M3: isolation ---

    #[test]
    fn isolation_isolate_forces_push_layer() {
        // `isolation: isolate` alone (no opacity/filter/blend/mask) still brackets
        // the subtree so descendant blending is confined to this group.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{isolation:isolate;background:#ff0000;width:50px;height:50px}",
        );
        let pushed = cmds.iter().any(
            |c| matches!(c, PaintCmd::PushLayer { opacity, filter, blend, mask } if *opacity == 1.0 && filter.is_empty() && *blend == BlendMode::Normal && mask.is_none()),
        );
        assert!(pushed, "isolation:isolate must force a PushLayer: {cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::PopLayer)));
    }

    #[test]
    fn isolation_auto_no_push_layer() {
        // Byte-identity sentinel: the initial `auto` must not bracket the subtree.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{isolation:auto;background:#ff0000;width:50px;height:50px}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })),
            "isolation:auto must not push a layer: {cmds:?}"
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

    // --- E45-M1: individual transform properties ---

    #[test]
    fn individual_translate_emits_transform_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{translate:50px 0;width:40px;height:40px}",
        );
        let m = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::PushTransform { matrix } => Some(*matrix),
                _ => None,
            })
            .expect("a matrix");
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(m[0], 1.0) && approx(m[3], 1.0));
        assert!(approx(m[4], 50.0) && approx(m[5], 0.0), "tx,ty={},{}", m[4], m[5]);
    }

    #[test]
    fn individual_props_compose_before_transform() {
        // translate:50px 0 (individual) then transform:scale(2): the effective
        // list is translate · scale, composed about the box center.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{translate:50px 0;transform:scale(2);width:40px;height:40px}",
        );
        let m = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::PushTransform { matrix } => Some(*matrix),
                _ => None,
            })
            .expect("a matrix");
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        // scale 2 about center (20,20): sx=sy=2, plus the 50px translate.
        // tx = ox(1-2) + 50 = 20*(-1) + 50 = 30; ty = oy(1-2) = -20.
        assert!(approx(m[0], 2.0) && approx(m[3], 2.0), "sx,sy={},{}", m[0], m[3]);
        assert!(approx(m[4], 30.0) && approx(m[5], -20.0), "tx,ty={},{}", m[4], m[5]);
    }

    // E45-M2: rotateY(60deg) flattens to a horizontal foreshortening — the
    // transform layer's x-scale ≈ cos 60° = 0.5, y-scale unchanged.
    #[test]
    fn transform_rotatey_foreshortens_x_scale() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{transform:rotateY(60deg);width:40px;height:40px}",
        );
        let m = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::PushTransform { matrix } => Some(*matrix),
                _ => None,
            })
            .expect("a matrix");
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(m[0], 0.5), "x-scale={}", m[0]);
        assert!(approx(m[3], 1.0), "y-scale={}", m[3]);
    }

    #[test]
    fn no_individual_transform_emits_no_layer() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:#ff0000;width:40px;height:40px}",
        );
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, PaintCmd::PushTransform { .. })));
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

    // --- E38-M1: <use>/<symbol>/<defs> instancing ---

    #[test]
    fn svg_use_instantiates_defs_target_at_offset() {
        // <use href="#c" x=20 y=20> paints the <defs> circle translated by (20,20).
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><circle id='c' r='10' cx='0' cy='0' fill='red'/></defs>\
             <use href='#c' x='20' y='20'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1, "one instanced circle: {cmds:?}");
        match shapes[0] {
            PaintCmd::SvgShape {
                geom,
                transform,
                fill,
                ..
            } => {
                // Geometry stays in user coords; the +20,+20 lands in the matrix.
                assert!(matches!(geom, SvgGeom::Ellipse { cx, cy, rx, ry }
                    if *cx == 0.0 && *cy == 0.0 && *rx == 10.0 && *ry == 10.0));
                assert_eq!(paint_color(fill), Some(red()));
                assert!(
                    approx6(*transform, [1.0, 0.0, 0.0, 1.0, 20.0, 20.0], 1e-4),
                    "{transform:?}"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_symbol_renders_only_via_use() {
        // A directly-walked <symbol> paints nothing; <use href="#s"> paints its rect.
        let direct = list(
            "<html><body><svg width='100' height='100'>\
             <symbol id='s'><rect x='0' y='0' width='10' height='10' fill='red'/></symbol>\
             </svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            svg_shapes(&direct).is_empty(),
            "symbol is a non-rendered template: {direct:?}"
        );

        let used = list(
            "<html><body><svg width='100' height='100'>\
             <symbol id='s'><rect x='0' y='0' width='10' height='10' fill='red'/></symbol>\
             <use href='#s'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&used);
        assert_eq!(shapes.len(), 1, "use renders the symbol's children: {used:?}");
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { geom: SvgGeom::Rect { w, h, .. }, fill: Some(SvgPaint::Color(c)), .. }
            if *w == 10.0 && *h == 10.0 && *c == red()));
    }

    #[test]
    fn svg_use_xlink_href_resolves() {
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><circle id='c' r='5' cx='0' cy='0' fill='red'/></defs>\
             <use xlink:href='#c' x='10' y='0'/></svg></body></html>",
            "body{margin:0}",
        );
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1, "xlink:href resolves: {cmds:?}");
        match shapes[0] {
            PaintCmd::SvgShape { transform, .. } => assert!(
                approx6(*transform, [1.0, 0.0, 0.0, 1.0, 10.0, 0.0], 1e-4),
                "{transform:?}"
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn svg_use_cycle_terminates() {
        // <g id="g"> contains a <use href="#g"> → self-reference; must not hang.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <g id='g'><rect x='0' y='0' width='10' height='10' fill='red'/>\
             <use href='#g'/></g></svg></body></html>",
            "body{margin:0}",
        );
        // It terminates (the depth cap stops the recursion); the rect paints at
        // least once and the count is bounded, not infinite.
        let n = svg_shapes(&cmds).len();
        assert!(n >= 1, "the rect paints: {cmds:?}");
        assert!(n <= SVG_USE_DEPTH_CAP + 2, "recursion bounded: got {n}");
    }

    // --- E38-M2: <pattern> fills ---

    #[test]
    fn svg_pattern_fill_tiles_clipped_to_rect() {
        // A 10x10 userSpaceOnUse pattern (one circle) tiles across a 40x40 rect:
        // a PushClip to the rect, then ~16 circle tiles (4x4 grid + edge), PopClip.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><pattern id='p' x='0' y='0' width='10' height='10' patternUnits='userSpaceOnUse'>\
             <circle cx='5' cy='5' r='3' fill='red'/></pattern></defs>\
             <rect x='0' y='0' width='40' height='40' fill='url(#p)'/></svg></body></html>",
            "body{margin:0}",
        );
        // Bracketed by exactly one PushClip + PopClip.
        let pushes = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::PushClip { .. }))
            .count();
        let pops = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::PopClip))
            .count();
        assert_eq!(pushes, 1, "one PushClip: {cmds:?}");
        assert_eq!(pops, 1, "one PopClip: {cmds:?}");
        // The clip rect is the 40x40 rect (body margin 0 → device origin 0,0).
        let clip = cmds.iter().find_map(|c| match c {
            PaintCmd::PushClip { rect, .. } => Some(*rect),
            _ => None,
        });
        let clip = clip.unwrap();
        assert!(
            (clip.width - 40.0).abs() < 1e-3 && (clip.height - 40.0).abs() < 1e-3,
            "clip is the 40x40 bbox: {clip:?}"
        );
        // 5 starts per axis (i in 0..=ceil(40/10)=0..4 → 5) → 25 tiles, each a circle.
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 25, "5x5 circle tiles: {}", shapes.len());
        assert!(shapes.iter().all(|c| matches!(c,
            PaintCmd::SvgShape { geom: SvgGeom::Ellipse { rx, ry, .. }, fill: Some(SvgPaint::Color(col)), .. }
            if *rx == 3.0 && *ry == 3.0 && *col == red())));
    }

    #[test]
    fn svg_pattern_tile_positions_step_by_tile_size() {
        // Each tile's transform is translate(tile_x, tile_y); the first tile sits
        // at the rect origin, the next one tile-width over.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><pattern id='p' width='10' height='10' patternUnits='userSpaceOnUse'>\
             <circle cx='5' cy='5' r='3' fill='red'/></pattern></defs>\
             <rect x='0' y='0' width='20' height='10' fill='url(#p)'/></svg></body></html>",
            "body{margin:0}",
        );
        let txs: Vec<f32> = svg_shapes(&cmds)
            .iter()
            .filter_map(|c| match c {
                PaintCmd::SvgShape { transform, .. } => Some(transform[4]),
                _ => None,
            })
            .collect();
        // ceil(20/10)+1 = 3 starts on x, 1 row on y → 3 tiles at tx 0,10,20.
        assert!(txs.contains(&0.0), "{txs:?}");
        assert!(txs.contains(&10.0), "{txs:?}");
        assert!(txs.contains(&20.0), "{txs:?}");
    }

    #[test]
    fn svg_pattern_object_bounding_box_units() {
        // objectBoundingBox (default): width=0.5 → tile is 0.5*40 = 20px wide.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><pattern id='p' width='0.5' height='0.5'>\
             <circle cx='5' cy='5' r='3' fill='red'/></pattern></defs>\
             <rect x='0' y='0' width='40' height='40' fill='url(#p)'/></svg></body></html>",
            "body{margin:0}",
        );
        let txs: Vec<f32> = svg_shapes(&cmds)
            .iter()
            .filter_map(|c| match c {
                PaintCmd::SvgShape { transform, .. } => Some(transform[4]),
                _ => None,
            })
            .collect();
        // tile = 20px → starts at 0,20,40 (ceil(40/20)+1=3) → contains a 20-step.
        assert!(txs.contains(&20.0), "obb tile is 20px wide: {txs:?}");
        assert!(!txs.contains(&10.0), "not 10px: {txs:?}");
    }

    #[test]
    fn svg_pattern_walked_directly_paints_nothing() {
        // A <pattern> reached by the walk (not via a fill) is a template.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <pattern id='p' width='10' height='10'>\
             <circle cx='5' cy='5' r='3' fill='red'/></pattern></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            svg_shapes(&cmds).is_empty(),
            "pattern is a non-rendered template: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })),
            "no clip for a directly-walked pattern: {cmds:?}"
        );
    }

    #[test]
    fn svg_normal_fill_unchanged_by_pattern_support() {
        // A plain solid fill emits exactly one shape, no clip (byte-identical path).
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(!cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })));
        let shapes = svg_shapes(&cmds);
        assert_eq!(shapes.len(), 1);
        assert!(matches!(shapes[0],
            PaintCmd::SvgShape { fill: Some(SvgPaint::Color(c)), .. } if *c == red()));
    }

    #[test]
    fn svg_pattern_fill_on_unsupported_shape_falls_back() {
        // A <path> filled by a pattern has no cheap bbox → falls back to the
        // normal fill path: no clip, the path paints as a single solid shape.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><pattern id='p' width='10' height='10' patternUnits='userSpaceOnUse'>\
             <circle cx='5' cy='5' r='3' fill='red'/></pattern></defs>\
             <path d='M0 0 L10 0 L10 10 Z' fill='url(#p)'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushClip { .. })),
            "no pattern tiling for a path bbox we can't cheaply derive: {cmds:?}"
        );
    }

    // --- E38-M3: <clipPath> + clip-path ---

    #[test]
    fn svg_clip_path_brackets_shape_with_circle_geom() {
        // A rect with clip-path="url(#cp)" where #cp is a circle: the list is
        // PushSvgClip (carrying the circle geom) ... the rect shape ... PopClip.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><clipPath id='cp'><circle cx='50' cy='50' r='30'/></clipPath></defs>\
             <rect x='0' y='0' width='100' height='100' fill='red' clip-path='url(#cp)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        // exactly one PushSvgClip ... one PopClip.
        let pushes = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::PushSvgClip { .. }))
            .count();
        let pops = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::PopClip))
            .count();
        assert_eq!(pushes, 1, "one PushSvgClip: {cmds:?}");
        assert_eq!(pops, 1, "one PopClip: {cmds:?}");
        // Order: PushSvgClip, then the rect shape, then PopClip.
        let push_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushSvgClip { .. }))
            .unwrap();
        let shape_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::SvgShape { .. }))
            .unwrap();
        let pop_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopClip))
            .unwrap();
        assert!(push_i < shape_i && shape_i < pop_i, "bracketed: {cmds:?}");
        // The clip geom is the circle (r=30), in user space.
        let geoms = cmds.iter().find_map(|c| match c {
            PaintCmd::PushSvgClip { geoms } => Some(geoms),
            _ => None,
        });
        let geoms = geoms.unwrap();
        assert_eq!(geoms.len(), 1, "one clip child: {geoms:?}");
        assert!(matches!(
            &geoms[0].0,
            SvgGeom::Ellipse { cx, cy, rx, ry }
                if *cx == 50.0 && *cy == 50.0 && *rx == 30.0 && *ry == 30.0
        ));
    }

    #[test]
    fn svg_clip_path_directly_walked_paints_nothing() {
        // A <clipPath> reached by the walk (not via clip-path=) is a template.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <clipPath id='cp'><circle cx='50' cy='50' r='30'/></clipPath></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            svg_shapes(&cmds).is_empty(),
            "clipPath is a non-rendered template: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushSvgClip { .. })),
            "no PushSvgClip for a directly-walked clipPath: {cmds:?}"
        );
    }

    #[test]
    fn svg_without_clip_path_unchanged() {
        // A shape without clip-path emits no PushSvgClip (byte-identical path).
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushSvgClip { .. })),
            "no PushSvgClip without clip-path: {cmds:?}"
        );
        assert_eq!(svg_shapes(&cmds).len(), 1);
    }

    #[test]
    fn svg_clip_path_missing_ref_paints_unclipped() {
        // clip-path referencing a missing id → graceful: no PushSvgClip, shape paints.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='0' y='0' width='10' height='10' fill='red' clip-path='url(#nope)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushSvgClip { .. })),
            "missing clipPath → unclipped: {cmds:?}"
        );
        assert_eq!(svg_shapes(&cmds).len(), 1, "shape still paints: {cmds:?}");
    }

    #[test]
    fn svg_clip_path_on_group_clips_subtree() {
        // clip-path on a <g> brackets the whole subtree (both child shapes).
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><clipPath id='cp'><circle cx='50' cy='50' r='30'/></clipPath></defs>\
             <g clip-path='url(#cp)'>\
             <rect x='0' y='0' width='40' height='40' fill='red'/>\
             <rect x='40' y='40' width='40' height='40' fill='blue'/></g></svg></body></html>",
            "body{margin:0}",
        );
        let push_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushSvgClip { .. }))
            .unwrap();
        let pop_i = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopClip))
            .unwrap();
        // Both rect shapes sit between the push and pop.
        let shapes: Vec<usize> = cmds
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, PaintCmd::SvgShape { .. }))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(shapes.len(), 2, "two shapes: {cmds:?}");
        assert!(
            shapes.iter().all(|&i| i > push_i && i < pop_i),
            "both shapes clipped: {cmds:?}"
        );
    }

    // --- E55-M3: <mask> + basic <filter> feGaussianBlur ---

    #[test]
    fn svg_filter_blur_brackets_shape_with_blur_layer() {
        // A rect with filter="url(#b)" where #b is a feGaussianBlur(stdDeviation=3)
        // → a PushLayer carrying a single FilterFn::Blur(3) brackets the rect.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><filter id='b'><feGaussianBlur stdDeviation='3'/></filter></defs>\
             <rect x='10' y='10' width='40' height='40' fill='red' filter='url(#b)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let push_i = cmds.iter().position(|c| {
            matches!(c, PaintCmd::PushLayer { filter, .. }
                if matches!(filter.as_slice(), [FilterFn::Blur(s)] if (*s - 3.0).abs() < 1e-4))
        });
        let push_i =
            push_i.unwrap_or_else(|| panic!("filter must emit a Blur PushLayer: {cmds:?}"));
        let shape_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::SvgShape { .. }))
            .expect("shape");
        let pop_i = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopLayer))
            .expect("PopLayer");
        assert!(
            push_i < shape_i && shape_i < pop_i,
            "rect bracketed by the blur layer: {cmds:?}"
        );
    }

    #[test]
    fn svg_mask_brackets_shape_with_mask_layer() {
        // A rect with mask="url(#m)" where #m is a white rect → a PushSvgMask
        // brackets the rect; the mask content cmds carry the white mask shape.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><mask id='m'><rect x='0' y='0' width='100' height='100' fill='white'/></mask></defs>\
             <rect x='10' y='10' width='40' height='40' fill='red' mask='url(#m)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        let push_i = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushSvgMask { .. }))
            .unwrap_or_else(|| panic!("mask must emit a PushSvgMask: {cmds:?}"));
        // The element shape sits between the mask push and the matching PopLayer.
        let shape_i = cmds
            .iter()
            .enumerate()
            .find(|(i, c)| *i > push_i && matches!(c, PaintCmd::SvgShape { .. }))
            .map(|(i, _)| i)
            .expect("masked shape");
        let pop_i = cmds
            .iter()
            .rposition(|c| matches!(c, PaintCmd::PopLayer))
            .expect("PopLayer");
        assert!(shape_i < pop_i, "rect bracketed by the mask layer: {cmds:?}");
        // The mask content commands carry the white mask rect (luminance source).
        let mask_cmds = cmds.iter().find_map(|c| match c {
            PaintCmd::PushSvgMask { mask_cmds } => Some(mask_cmds),
            _ => None,
        });
        let mask_cmds = mask_cmds.unwrap();
        assert!(
            mask_cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::SvgShape { fill: Some(SvgPaint::Color(col)), .. }
                    if col.r == 255 && col.g == 255 && col.b == 255)),
            "white mask shape in mask content: {mask_cmds:?}"
        );
    }

    #[test]
    fn svg_mask_and_filter_directly_walked_paint_nothing() {
        // A <mask>/<filter> reached by the walk (not via mask=/filter=) is a
        // template and paints nothing — no shapes, no mask/filter layer.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <mask id='m'><rect x='0' y='0' width='10' height='10' fill='white'/></mask>\
             <filter id='b'><feGaussianBlur stdDeviation='3'/></filter></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            svg_shapes(&cmds).is_empty(),
            "mask/filter are non-rendered templates: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushSvgMask { .. })),
            "no mask layer for a directly-walked <mask>: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })),
            "no filter layer for a directly-walked <filter>: {cmds:?}"
        );
    }

    #[test]
    fn svg_without_mask_or_filter_unchanged() {
        // A shape without mask/filter emits no extra layer (byte-identical path).
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <rect x='0' y='0' width='10' height='10' fill='red'/></svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::PushSvgMask { .. } | PaintCmd::PushLayer { .. })),
            "no mask/filter layer without mask=/filter=: {cmds:?}"
        );
        assert_eq!(svg_shapes(&cmds).len(), 1);
    }

    #[test]
    fn svg_filter_missing_or_no_blur_paints_unbracketed() {
        // filter referencing a missing id, or a <filter> with no feGaussianBlur →
        // graceful: no blur layer, the shape paints.
        let cmds = list(
            "<html><body><svg width='100' height='100'>\
             <defs><filter id='empty'></filter></defs>\
             <rect x='0' y='0' width='10' height='10' fill='red' filter='url(#nope)'/>\
             <rect x='0' y='0' width='10' height='10' fill='blue' filter='url(#empty)'/>\
             </svg></body></html>",
            "body{margin:0}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::PushLayer { .. })),
            "no blur layer for missing/blurless filter: {cmds:?}"
        );
        assert_eq!(svg_shapes(&cmds).len(), 2, "both shapes paint: {cmds:?}");
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
                assert_eq!(g.stops[0].pos, Some(starfish_style::GradientStopPos::Frac(0.0)));
                assert_eq!(g.stops[1].color, blue());
                assert_eq!(g.stops[1].pos, Some(starfish_style::GradientStopPos::Frac(1.0)));
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
    fn placeholder_pseudo_colors_text() {
        // E35-M2: `input::placeholder{color:#06c}` paints the placeholder run blue.
        let cmds = list(
            "<html><body><input placeholder='name'></body></html>",
            "body{margin:0} input::placeholder { color: #06c }",
        );
        assert_eq!(
            glyph_color(&cmds, "name"),
            Some(Rgba {
                r: 0,
                g: 0x66,
                b: 0xcc,
                a: 255
            }),
            "placeholder pseudo blue: {cmds:?}"
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
    fn cross_fade_emits_b_then_a_in_opacity_layer() {
        // E48-M3: `cross-fade(grad_a 50%, grad_b)` paints `b` first, then `a`
        // inside a PushLayer{opacity:0.5} ("a over b at alpha p"). So: two
        // GradientRects with a PushLayer(0.5) / PopLayer wrapping the second.
        let cmds = list(
            "<html><body><div id='d'></div></body></html>",
            "body{margin:0} #d{width:80px;height:40px;\
             background:cross-fade(linear-gradient(#f00,#f00) 50%, linear-gradient(#00f,#00f))}",
        );
        let grads = cmds
            .iter()
            .filter(|c| matches!(c, PaintCmd::GradientRect { .. }))
            .count();
        assert_eq!(grads, 2, "cross-fade → two GradientRects: {cmds:?}");
        // The opacity layer carries p = 0.5.
        let push = cmds.iter().position(|c| {
            matches!(c, PaintCmd::PushLayer { opacity, .. } if (*opacity - 0.5).abs() < 1e-6)
        });
        assert!(push.is_some(), "expected PushLayer(opacity=0.5): {cmds:?}");
        let push = push.unwrap();
        // `b` (first gradient) is emitted before the layer; `a` is inside it.
        let first_grad = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GradientRect { .. }))
            .unwrap();
        assert!(first_grad < push, "b's gradient must precede the opacity layer");
        // A PopLayer closes it.
        assert!(
            cmds[push..].iter().any(|c| matches!(c, PaintCmd::PopLayer)),
            "expected PopLayer after the cross-fade opacity layer: {cmds:?}"
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
    fn multicol_three_columns_emit_two_rules_at_gap_centers() {
        // column-count:3, gap:30, width:220 → col_w = (220 - 2*30)/3 = 53.333…
        // Two inter-column gaps; centers at:
        //   i=1: 1*col_w + 0.5*gap = 53.333 + 15      = 68.333…
        //   i=2: 2*col_w + 1.5*gap = 106.667 + 45     = 151.667…
        let cmds = list(
            "<html><body><div id='mc'>\
             <div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></div></body></html>",
            "body{margin:0} \
             #mc{margin:0;width:220px;column-count:3;column-gap:30px;\
             column-rule:2px solid #c00} \
             #mc>div{margin:0;height:30px}",
        );
        let xs: Vec<f32> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::StrokeLine { from, width, style, .. }
                    if *width == 2.0 && *style == BorderStyle::Solid =>
                {
                    Some(from.0)
                }
                _ => None,
            })
            .collect();
        assert_eq!(xs.len(), 2, "expected exactly two column rules: {cmds:?}");
        assert!((xs[0] - 68.333_33).abs() < 0.01, "first rule x = {}", xs[0]);
        assert!((xs[1] - 151.666_67).abs() < 0.01, "second rule x = {}", xs[1]);
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

    // --- E47-M1: background-clip / background-origin ---

    // The red background-color FillRect for the box (ignores the body fill).
    fn bg_color_fill(cmds: &[PaintCmd]) -> Rect {
        cmds.iter()
            .find_map(|c| match c {
                PaintCmd::FillRect { rect, color, .. } if color.r == 255 && color.b == 0 => {
                    Some(*rect)
                }
                _ => None,
            })
            .expect("red bg-color FillRect")
    }

    #[test]
    fn bg_clip_content_box_shrinks_color_fill() {
        // padding:10px → the content box is the border box inset by 10px on each
        // side. `background-clip:content-box` must clip the color to it.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background-color:#ff0000;background-clip:content-box;\
             padding:10px;width:50px;height:50px}",
        );
        let r = bg_color_fill(&cmds);
        // content box = (10,10) 50x50; border box would be (0,0) 70x70.
        assert_eq!((r.x, r.y), (10.0, 10.0), "content-box origin: {r:?}");
        assert_eq!((r.width, r.height), (50.0, 50.0), "content-box size: {r:?}");
    }

    #[test]
    fn bg_clip_default_is_border_box_byte_identical() {
        // No background-clip → the color fills the full border box (padding-box
        // here since there's no border), exactly as pre-E47.
        let with = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background-color:#ff0000;padding:10px;width:50px;height:50px}",
        );
        let r = bg_color_fill(&with);
        // border box = (0,0) 70x70.
        assert_eq!((r.x, r.y), (0.0, 0.0), "default = border-box origin: {r:?}");
        assert_eq!((r.width, r.height), (70.0, 70.0), "default = border-box: {r:?}");
    }

    // --- E47-M2: background-clip:text + background-attachment ---

    #[test]
    fn bg_clip_text_brackets_background_with_glyph_clip() {
        // A gradient background with `background-clip:text; color:transparent`
        // must emit the bg inside a PushTextClip/PopTextClip bracket, and the clip
        // must carry the element's text glyphs (true glyph coverage).
        let cmds = list(
            "<html><body><div id='d'>Hi</div></body></html>",
            "body{margin:0} #d{background:linear-gradient(#f00,#00f);\
             background-clip:text;color:transparent;width:80px;height:40px}",
        );
        let push = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushTextClip { .. }))
            .expect("PushTextClip emitted for background-clip:text");
        let pop = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopTextClip))
            .expect("PopTextClip emitted");
        // The gradient fill is bracketed between push and pop.
        let grad = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::GradientRect { .. }))
            .expect("GradientRect for the background");
        assert!(push < grad && grad < pop, "bg gradient inside the text clip");
        // The clip carries the element's text glyphs.
        if let PaintCmd::PushTextClip { glyphs } = &cmds[push] {
            assert!(
                glyphs.iter().any(|g| g.text.contains("Hi")),
                "text clip carries the element's glyphs: {glyphs:?}"
            );
        }
    }

    #[test]
    fn no_bg_clip_text_no_text_clip() {
        // Byte-identity sentinel: a normal gradient background emits no text clip.
        let cmds = list(
            "<html><body><div id='d'>Hi</div></body></html>",
            "body{margin:0} #d{background:linear-gradient(#f00,#00f);width:80px;height:40px}",
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::PushTextClip { .. })),
            "no background-clip:text must not push a text clip: {cmds:?}"
        );
    }

    #[test]
    fn bg_attachment_fixed_anchors_gradient_to_viewport() {
        // A box offset from the origin with `background-attachment:fixed` paints
        // its gradient from the viewport top-left (0,0), not the box origin.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:linear-gradient(#f00,#00f);\
             background-attachment:fixed;margin-top:30px;width:50px;height:50px}",
        );
        let r = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::GradientRect { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("GradientRect");
        assert_eq!((r.x, r.y), (0.0, 0.0), "fixed anchors to viewport: {r:?}");
    }

    #[test]
    fn bg_attachment_scroll_default_does_not_crash() {
        // Default (scroll) keeps the gradient anchored to the box (byte-identical).
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{background:linear-gradient(#f00,#00f);\
             margin-top:30px;width:50px;height:50px}",
        );
        let r = cmds
            .iter()
            .find_map(|c| match c {
                PaintCmd::GradientRect { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("GradientRect");
        assert_eq!(r.y, 30.0, "scroll keeps the box origin: {r:?}");
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

    // --- E37-M1: overflow:scroll/auto overlay scrollbar ---

    /// The track + thumb FillRects matching the scrollbar palette, in emit order.
    fn scrollbar_rects(cmds: &[PaintCmd]) -> Vec<(Rect, Rgba, [f32; 4])> {
        cmds.iter()
            .filter_map(|c| match c {
                PaintCmd::FillRect {
                    rect,
                    color,
                    radius,
                    ..
                } if *color == SCROLLBAR_TRACK_COLOR || *color == SCROLLBAR_THUMB_COLOR => {
                    Some((*rect, *color, *radius))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn overflow_scroll_emits_track_and_thumb() {
        // 100px-wide box, 50px tall, content 200px tall → scrollbar at right edge,
        // thumb height = 50/200 * 50 = 12.5, but clamped to >= width (12) → 12.5.
        let cmds = list(
            "<html><body><div id='d'>\
               <div id='c'></div>\
             </div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:10px;height:200px}",
        );
        let bars = scrollbar_rects(&cmds);
        assert_eq!(bars.len(), 2, "track + thumb expected: {cmds:?}");
        let (track, tc, _) = bars[0];
        let (thumb, hc, hr) = bars[1];
        assert_eq!(tc, SCROLLBAR_TRACK_COLOR);
        assert_eq!(hc, SCROLLBAR_THUMB_COLOR);
        // track: 12px wide, full padding-box height (50), at the right edge (x=88).
        assert_eq!(track.width, SCROLLBAR_WIDTH);
        assert_eq!(track.height, 50.0);
        assert_eq!(track.x, 100.0 - SCROLLBAR_WIDTH);
        assert_eq!(track.y, 0.0);
        // thumb shorter than the track (clientHeight < scrollHeight), rounded.
        assert!(
            thumb.height < track.height,
            "thumb {} should be shorter than track {}",
            thumb.height,
            track.height
        );
        assert_eq!(thumb.height, 50.0 * (50.0 / 200.0));
        assert_eq!(thumb.y, track.y);
        assert_eq!(hr, [SCROLLBAR_THUMB_RADIUS; 4]);
    }

    #[test]
    fn overflow_auto_no_scrollbar_when_content_fits() {
        // content (20px) fits in the 50px box → auto shows nothing.
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:auto} \
             #c{width:10px;height:20px}",
        );
        assert!(
            scrollbar_rects(&cmds).is_empty(),
            "auto + fitting content must emit no scrollbar: {cmds:?}"
        );
    }

    #[test]
    fn overflow_auto_scrollbar_when_overflowing() {
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:auto} \
             #c{width:10px;height:200px}",
        );
        assert_eq!(
            scrollbar_rects(&cmds).len(),
            2,
            "auto + overflowing content must emit track + thumb: {cmds:?}"
        );
    }

    #[test]
    fn overflow_hidden_emits_no_scrollbar() {
        // Byte-identity sentinel: hidden never paints a scrollbar even when
        // content overflows (it just clips).
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:hidden} \
             #c{width:10px;height:200px}",
        );
        assert!(
            scrollbar_rects(&cmds).is_empty(),
            "overflow:hidden must emit no scrollbar: {cmds:?}"
        );
    }

    #[test]
    fn scrollbar_painted_after_content_clip_pop() {
        // The overlay must come AFTER the box's PopClip (so it is not clipped).
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:10px;height:200px}",
        );
        let pop = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopClip))
            .expect("PopClip");
        let first_bar = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::FillRect { color, .. }
                if *color == SCROLLBAR_TRACK_COLOR))
            .expect("track FillRect");
        assert!(
            first_bar > pop,
            "scrollbar {first_bar} must be emitted after PopClip {pop}"
        );
    }

    // --- E37-M2: scroll offset (scrollTop/scrollLeft) ---

    /// Like `list`, but sets `(scrollLeft, scrollTop)` on the element whose `id`
    /// attribute equals `elem_id` before building the display list.
    fn list_scrolled(html: &str, css: &str, elem_id: &str, sx: f32, sy: f32) -> Vec<PaintCmd> {
        let mut doc = parse(html);
        let target = (0..doc.node_count())
            .map(starfish_dom::NodeId::from_index)
            .find(|&id| doc.get_attribute(id, "id") == Some(elem_id))
            .expect("element with the given id");
        doc.set_scroll_offset(target, sx, sy);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let images = ImageStore::new(Url::parse("file:///").unwrap(), &LocalLoader);
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    #[test]
    fn scroll_offset_translates_content_inside_clip() {
        // 100x50 scroll box, content 200px tall, scrollTop = 40 → the content is
        // wrapped in a translate(0, -40) emitted between the content PushClip and
        // its PopClip.
        let cmds = list_scrolled(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:10px;height:200px}",
            "d",
            0.0,
            40.0,
        );
        let push_clip = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushClip { .. }))
            .expect("content PushClip");
        let pop_clip = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PopClip))
            .expect("content PopClip");
        let xform = cmds
            .iter()
            .position(|c| matches!(c, PaintCmd::PushTransform { matrix } if *matrix == [1.0, 0.0, 0.0, 1.0, 0.0, -40.0]))
            .expect("content translate(0, -40)");
        assert!(
            push_clip < xform && xform < pop_clip,
            "translate {xform} must sit between PushClip {push_clip} and PopClip {pop_clip}"
        );
        // Thumb top moves down: applied_y(40)/scrollHeight(200) * track(50) = 10.
        let bars = scrollbar_rects(&cmds);
        let thumb = bars[1].0;
        assert_eq!(thumb.y, 10.0, "thumb top must reflect scrollTop");
        assert!(thumb.y > 0.0);
    }

    #[test]
    fn scroll_left_translates_content_horizontally() {
        let cmds = list_scrolled(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:300px;height:200px}",
            "d",
            25.0,
            0.0,
        );
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::PushTransform { matrix }
                if *matrix == [1.0, 0.0, 0.0, 1.0, -25.0, 0.0])),
            "scrollLeft=25 must emit translate(-25, 0): {cmds:?}"
        );
    }

    #[test]
    fn scroll_offset_clamps_to_max() {
        // scrollTop wildly past the content → clamped to scrollHeight-clientHeight
        // = 200 - 50 = 150.
        let cmds = list_scrolled(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:10px;height:200px}",
            "d",
            0.0,
            9999.0,
        );
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::PushTransform { matrix }
                if *matrix == [1.0, 0.0, 0.0, 1.0, 0.0, -150.0])),
            "scrollTop must clamp to max (150): {cmds:?}"
        );
    }

    #[test]
    fn zero_scroll_offset_emits_no_transform_byte_identical() {
        // Byte-identity sentinel: scrollTop=0 must be IDENTICAL to no scroll offset
        // (no content transform emitted around the scroll box's children).
        let html = "<html><body><div id='d'><div id='c'></div></div></body></html>";
        let css = "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
                   #c{width:10px;height:200px}";
        let baseline = list(html, css);
        let scrolled_zero = list_scrolled(html, css, "d", 0.0, 0.0);
        assert_eq!(
            scrolled_zero, baseline,
            "zero scroll offset must be byte-identical to M1"
        );
        // and the M1 thumb top stays at the track top.
        let thumb = scrollbar_rects(&baseline)[1].0;
        assert_eq!(thumb.y, 0.0);
    }

    // --- E60-M3: scroll-snap geometry ---

    /// Extract the translate-Y from the scroll box's content transform (the
    /// `f` component of the `PushTransform` matrix `[1,0,0,1,e,f]`). 0.0 when no
    /// transform is emitted (zero offset). Returns the FIRST such transform.
    fn content_translate_y(cmds: &[PaintCmd]) -> f32 {
        cmds.iter()
            .find_map(|c| match c {
                PaintCmd::PushTransform { matrix } if matrix[0] == 1.0 && matrix[3] == 1.0 => {
                    Some(matrix[5])
                }
                _ => None,
            })
            .unwrap_or(0.0)
    }

    #[test]
    fn snap_y_mandatory_aligns_second_child_start_to_top() {
        // 100x50 snap container, scrollTop=0; the 2nd child (top=100) has
        // scroll-snap-align:start → the nearest snap target to 0 is its top at
        // 100, so the content translates by -100.
        let cmds = list_scrolled(
            "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory} \
             .a{width:10px;height:100px} \
             .b{width:10px;height:100px;scroll-snap-align:start}",
            "d",
            0.0,
            0.0,
        );
        assert_eq!(
            content_translate_y(&cmds),
            -100.0,
            "2nd child start must snap to scrollport top (translate -100): {cmds:?}"
        );
    }

    #[test]
    fn snap_y_honors_scroll_padding_top() {
        // Same as above but scroll-padding-top:10 → start aligns to the snapport
        // top inset by 10, so offset = 100 - 10 = 90 → translate -90.
        let cmds = list_scrolled(
            "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory;\
                scroll-padding-top:10px} \
             .a{width:10px;height:100px} \
             .b{width:10px;height:100px;scroll-snap-align:start}",
            "d",
            0.0,
            0.0,
        );
        assert_eq!(
            content_translate_y(&cmds),
            -90.0,
            "scroll-padding-top:10 must inset the snapport (translate -90): {cmds:?}"
        );
    }

    #[test]
    fn snap_y_picks_nearest_target_to_js_scrolltop() {
        // Two snap-start children at top=100 and top=300. scrollTop=280 is nearer
        // the 2nd target (300) than the 1st (100) → snaps to 300, clamped to the
        // max offset (scrollHeight 400 - clientHeight 50 = 350) so 300 stands →
        // translate -300.
        let cmds = list_scrolled(
            "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div><div class='c'></div></div>\
             </body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory} \
             .a{width:10px;height:100px} \
             .b{width:10px;height:200px;scroll-snap-align:start} \
             .c{width:10px;height:100px;scroll-snap-align:start}",
            "d",
            0.0,
            280.0,
        );
        // children border-box tops: .a=0, .b=100, .c=300. snap targets 100, 300.
        // current clamped scrollTop = 280 → nearest is 300.
        assert_eq!(
            content_translate_y(&cmds),
            -300.0,
            "scrollTop 280 must snap to nearest target 300 (translate -300): {cmds:?}"
        );
    }

    #[test]
    fn snap_end_aligns_child_bottom_to_snapport_bottom() {
        // A single tall child with snap-align:end; container client 50.
        // area_end = child bottom = 200; offset = 200 - 50 = 150 → translate -150.
        let cmds = list_scrolled(
            "<html><body><div id='d'><div class='b'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory} \
             .b{width:10px;height:200px;scroll-snap-align:end}",
            "d",
            0.0,
            0.0,
        );
        assert_eq!(
            content_translate_y(&cmds),
            -150.0,
            "snap-align:end must align child bottom to snapport bottom (translate -150): {cmds:?}"
        );
    }

    #[test]
    fn snap_honors_child_scroll_margin() {
        // 2nd child top=100 with scroll-margin-top:8 and snap-align:start. The
        // snap area starts 8px above the border box → offset = 100 - 8 = 92.
        let cmds = list_scrolled(
            "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory} \
             .a{width:10px;height:100px} \
             .b{width:10px;height:100px;scroll-snap-align:start;scroll-margin-top:8px}",
            "d",
            0.0,
            0.0,
        );
        assert_eq!(
            content_translate_y(&cmds),
            -92.0,
            "scroll-margin-top:8 must outset the snap area (translate -92): {cmds:?}"
        );
    }

    #[test]
    fn no_snap_type_is_byte_identical() {
        // Byte-identity sentinel: identical markup WITHOUT scroll-snap-type (but
        // children still carry scroll-snap-align) must NOT snap — offset stays 0,
        // so output equals the no-scroll baseline.
        let html = "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>";
        let css = "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll} \
             .a{width:10px;height:100px} \
             .b{width:10px;height:100px;scroll-snap-align:start}";
        let baseline = list(html, css);
        let scrolled_zero = list_scrolled(html, css, "d", 0.0, 0.0);
        assert_eq!(
            scrolled_zero, baseline,
            "no scroll-snap-type must be byte-identical (no snapping)"
        );
        assert_eq!(content_translate_y(&baseline), 0.0, "no snap → offset 0");
    }

    #[test]
    fn snap_type_without_aligned_children_is_byte_identical() {
        // A snap container whose children carry NO scroll-snap-align → no targets
        // → offset stays at the clamped E37-M2 value (here 0).
        let html = "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>";
        let css = "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scroll-snap-type:y mandatory} \
             .a{width:10px;height:100px} .b{width:10px;height:100px}";
        let baseline = list(
            "<html><body><div id='d'>\
               <div class='a'></div><div class='b'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll} \
             .a{width:10px;height:100px} .b{width:10px;height:100px}",
        );
        let snapped = list_scrolled(html, css, "d", 0.0, 0.0);
        assert_eq!(
            snapped, baseline,
            "snap container with no aligned children must not snap (offset 0)"
        );
    }

    // --- E37-M3: scrollbar-width / scrollbar-color ---

    #[test]
    fn scrollbar_width_none_emits_no_scrollbar() {
        // `scrollbar-width: none` hides the overlay even when content overflows.
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll;scrollbar-width:none} \
             #c{width:10px;height:200px}",
        );
        assert!(
            scrollbar_rects(&cmds).is_empty(),
            "scrollbar-width:none must emit no scrollbar: {cmds:?}"
        );
    }

    #[test]
    fn scrollbar_width_thin_narrows_track() {
        // `scrollbar-width: thin` → 6px track at the right edge (x = 100 - 6).
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll;scrollbar-width:thin} \
             #c{width:10px;height:200px}",
        );
        let bars = scrollbar_rects(&cmds);
        assert_eq!(bars.len(), 2, "thin still paints track + thumb: {cmds:?}");
        let track = bars[0].0;
        assert_eq!(track.width, SCROLLBAR_WIDTH_THIN);
        assert_eq!(track.x, 100.0 - SCROLLBAR_WIDTH_THIN);
    }

    #[test]
    fn scrollbar_color_recolors_thumb_and_track() {
        // `scrollbar-color: #ff0000 #0000ff` → thumb red, track blue.
        let red = Rgba { r: 0xff, g: 0, b: 0, a: 0xff };
        let blue = Rgba { r: 0, g: 0, b: 0xff, a: 0xff };
        let cmds = list(
            "<html><body><div id='d'><div id='c'></div></div></body></html>",
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scrollbar-color:#ff0000 #0000ff} \
             #c{width:10px;height:200px}",
        );
        // The default-grey filter finds nothing now (custom colors).
        assert!(
            scrollbar_rects(&cmds).is_empty(),
            "default greys must be gone: {cmds:?}"
        );
        let track = cmds.iter().find_map(|c| match c {
            PaintCmd::FillRect { rect, color, .. } if *color == blue => Some(*rect),
            _ => None,
        });
        let thumb = cmds.iter().find_map(|c| match c {
            PaintCmd::FillRect { rect, color, .. } if *color == red => Some(*rect),
            _ => None,
        });
        assert!(track.is_some(), "blue track FillRect expected: {cmds:?}");
        assert!(thumb.is_some(), "red thumb FillRect expected: {cmds:?}");
        // geometry unchanged: 12px track at the right edge.
        assert_eq!(track.unwrap().width, SCROLLBAR_WIDTH);
    }

    #[test]
    fn default_scrollbar_styling_byte_identical() {
        // Byte-identity sentinel: a scroll box with default scrollbar-width/color
        // renders IDENTICALLY to the same box without the properties (M1 greys,12px).
        let html = "<html><body><div id='d'><div id='c'></div></div></body></html>";
        let baseline = list(
            html,
            "body{margin:0} #d{width:100px;height:50px;overflow:scroll} \
             #c{width:10px;height:200px}",
        );
        let explicit = list(
            html,
            "body{margin:0} \
             #d{width:100px;height:50px;overflow:scroll;scrollbar-width:auto} \
             #c{width:10px;height:200px}",
        );
        assert_eq!(
            explicit, baseline,
            "scrollbar-width:auto must be byte-identical to M1 default"
        );
    }

    // E39-M1: gauge rendering. Collect the (rect,color) of every FillRect.
    fn fill_rects(cmds: &[PaintCmd]) -> Vec<(Rect, Rgba)> {
        cmds.iter()
            .filter_map(|c| match c {
                PaintCmd::FillRect { rect, color, .. } => Some((*rect, *color)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn progress_emits_track_and_half_fill() {
        // E39-M1: <progress value=0.5> → a #e6e6e6 track + a #2680eb fill ~50%.
        let cmds = list(
            "<html><body><progress value=0.5></progress></body></html>",
            "body{margin:0}",
        );
        let fills = fill_rects(&cmds);
        let track = fills
            .iter()
            .find(|(_, c)| *c == GAUGE_TRACK)
            .expect("a gauge track FillRect");
        let fill = fills
            .iter()
            .find(|(_, c)| *c == PROGRESS_FILL)
            .expect("a progress fill FillRect");
        let frac = fill.0.width / track.0.width;
        assert!((frac - 0.5).abs() < 0.01, "fill ≈ 0.5 of track, got {frac}");
    }

    #[test]
    fn progress_indeterminate_emits_track_and_partial_fill() {
        // E39-M1: <progress> with no value → track + a 0.5 indeterminate fill.
        let cmds = list(
            "<html><body><progress></progress></body></html>",
            "body{margin:0}",
        );
        let fills = fill_rects(&cmds);
        let track = fills
            .iter()
            .find(|(_, c)| *c == GAUGE_TRACK)
            .expect("a gauge track FillRect");
        let fill = fills
            .iter()
            .find(|(_, c)| *c == PROGRESS_FILL)
            .expect("an indeterminate progress fill FillRect");
        let frac = fill.0.width / track.0.width;
        assert!(
            (frac - 0.5).abs() < 0.01,
            "indeterminate fill ≈ 0.5 of track, got {frac}"
        );
    }

    // --- E51-M1: accent-color tints the accented form-control fills ---
    const RED: Rgba = Rgba {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };

    #[test]
    fn accent_color_tints_checkbox_tick() {
        // A checked checkbox with accent-color:red draws its tick in red,
        // not the default #333333.
        let cmds = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0} input{accent-color:red}",
        );
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Path(_), stroke: Some(SvgPaint::Color(s)), .. }
                    if *s == RED
            )),
            "checked checkbox tick is red: {cmds:?}"
        );
        // No leftover #333333 tick.
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Path(_), stroke: Some(SvgPaint::Color(s)), .. }
                    if s.r == 0x33 && s.g == 0x33 && s.b == 0x33
            )),
            "no default-colored tick remains: {cmds:?}"
        );
    }

    #[test]
    fn accent_color_default_checkbox_tick_byte_identical() {
        // Without accent-color the tick keeps the UA #333333 (byte-identical).
        let cmds = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0}",
        );
        assert!(
            cmds.iter().any(|c| matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Path(_), stroke: Some(SvgPaint::Color(s)), .. }
                    if s.r == 0x33 && s.g == 0x33 && s.b == 0x33
            )),
            "default checkbox tick is #333333: {cmds:?}"
        );
    }

    #[test]
    fn accent_color_tints_progress_fill() {
        // <progress value=0.5> with accent-color:red fills red, not #2680eb.
        let cmds = list(
            "<html><body><progress value=0.5></progress></body></html>",
            "body{margin:0} progress{accent-color:red}",
        );
        let fills = fill_rects(&cmds);
        assert!(
            fills.iter().any(|(_, c)| *c == RED),
            "progress fill is red: {fills:?}"
        );
        assert!(
            !fills.iter().any(|(_, c)| *c == PROGRESS_FILL),
            "no default blue fill remains: {fills:?}"
        );
    }

    #[test]
    fn accent_color_auto_progress_byte_identical() {
        // accent-color:auto → the default #2680eb fill (byte-identical to unset).
        let cmds = list(
            "<html><body><progress value=0.5></progress></body></html>",
            "body{margin:0} progress{accent-color:auto}",
        );
        let fills = fill_rects(&cmds);
        assert!(
            fills.iter().any(|(_, c)| *c == PROGRESS_FILL),
            "auto keeps the default blue progress fill: {fills:?}"
        );
    }

    #[test]
    fn meter_emits_track_and_proportional_fill() {
        // E39-M1: <meter value=0.25> → track + a #22aa22 fill ~25%.
        let cmds = list(
            "<html><body><meter value=0.25></meter></body></html>",
            "body{margin:0}",
        );
        let fills = fill_rects(&cmds);
        let track = fills
            .iter()
            .find(|(_, c)| *c == GAUGE_TRACK)
            .expect("a gauge track FillRect");
        let fill = fills
            .iter()
            .find(|(_, c)| *c == METER_FILL)
            .expect("a meter fill FillRect");
        let frac = fill.0.width / track.0.width;
        assert!((frac - 0.25).abs() < 0.01, "fill ≈ 0.25 of track, got {frac}");
    }

    // --- E51-M2: appearance: none strips UA control chrome ---

    #[test]
    fn appearance_none_checkbox_no_tick_chrome() {
        // A checked checkbox with appearance:none + author bg/border renders as a
        // plain box: NO tick polyline, NO UA #767676 box outline.
        let cmds = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0} input{appearance:none;background:red;\
             width:20px;height:20px}",
        );
        // No UA tick.
        assert!(
            !has_stroked_path(&cmds),
            "appearance:none checkbox draws no tick: {cmds:?}"
        );
        // No UA #767676 box outline rect.
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                PaintCmd::SvgShape { geom: SvgGeom::Rect { .. }, stroke: Some(SvgPaint::Color(s)), .. }
                    if s.r == 0x76 && s.g == 0x76 && s.b == 0x76
            )),
            "appearance:none checkbox draws no UA box outline: {cmds:?}"
        );
        // The author background (red) still paints as a plain box fill.
        assert!(
            fill_rects(&cmds).iter().any(|(_, c)| *c == RED),
            "appearance:none checkbox still paints its author red background: {cmds:?}"
        );
    }

    #[test]
    fn appearance_auto_checkbox_keeps_chrome_byte_identical() {
        // appearance:auto keeps the full UA chrome — byte-identical to a plain
        // checked checkbox (tick + #767676 outline present).
        let auto = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0} input{appearance:auto}",
        );
        let plain = list(
            "<html><body><input type='checkbox' checked></body></html>",
            "body{margin:0}",
        );
        assert_eq!(auto, plain, "appearance:auto is byte-identical to default");
        assert!(has_stroked_path(&auto), "auto keeps the tick: {auto:?}");
    }

    #[test]
    fn appearance_none_select_no_dropdown_triangle() {
        // appearance:none on a <select> suppresses the dropdown-arrow triangle.
        let cmds = list(
            "<html><body><select><option selected>Banana</select></body></html>",
            "body{margin:0} select{appearance:none}",
        );
        assert!(
            !has_filled_path(&cmds),
            "appearance:none select draws no dropdown triangle: {cmds:?}"
        );
    }

    // --- E58-M1: input-type chrome (number spinner / search / date indicator) ---

    /// Count filled #333333 triangle Paths (the spinner/date indicator shapes).
    fn fc_triangle_count(cmds: &[PaintCmd]) -> usize {
        cmds.iter()
            .filter(|c| {
                matches!(
                    c,
                    PaintCmd::SvgShape {
                        geom: SvgGeom::Path(_),
                        fill: Some(SvgPaint::Color(f)),
                        ..
                    } if f.r == 0x33 && f.g == 0x33 && f.b == 0x33
                )
            })
            .count()
    }

    #[test]
    fn number_input_shows_value_and_spinner() {
        let cmds = list(
            "<html><body><input type='number' value='5'></body></html>",
            "body{margin:0}",
        );
        // The value text is painted.
        assert!(
            glyph_color(&cmds, "5").is_some(),
            "expected value '5' glyph: {cmds:?}"
        );
        // Two stacked spinner triangles (up + down).
        assert_eq!(
            fc_triangle_count(&cmds),
            2,
            "number input draws two spinner triangles: {cmds:?}"
        );
    }

    #[test]
    fn date_input_shows_value_and_indicator() {
        let cmds = list(
            "<html><body><input type='date' value='2026-01-01'></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&cmds, "2026-01-01").is_some(),
            "expected date value glyph: {cmds:?}"
        );
        // A single picker indicator triangle.
        assert_eq!(
            fc_triangle_count(&cmds),
            1,
            "date input draws one picker indicator: {cmds:?}"
        );
    }

    #[test]
    fn search_input_draws_magnifier_dot() {
        let cmds = list(
            "<html><body><input type='search' value='q'></body></html>",
            "body{margin:0}",
        );
        assert!(
            glyph_color(&cmds, "q").is_some(),
            "expected search value glyph: {cmds:?}"
        );
        // A stroked circle (magnifier dot); no spinner/picker triangles.
        let has_dot = cmds.iter().any(|c| {
            matches!(
                c,
                PaintCmd::SvgShape {
                    geom: SvgGeom::Ellipse { .. },
                    fill: None,
                    stroke: Some(SvgPaint::Color(_)),
                    ..
                }
            )
        });
        assert!(has_dot, "search input draws a magnifier dot: {cmds:?}");
        assert_eq!(
            fc_triangle_count(&cmds),
            0,
            "search input has no spinner/picker triangle: {cmds:?}"
        );
    }

    #[test]
    fn email_input_is_plain_no_chrome() {
        // type=email is a plain text field — identical chrome to plain text.
        let email = list(
            "<html><body><input type='email' value='a@b'></body></html>",
            "body{margin:0}",
        );
        let plain = list(
            "<html><body><input type='text' value='a@b'></body></html>",
            "body{margin:0}",
        );
        assert_eq!(email, plain, "type=email is byte-identical to plain text");
        assert!(
            !cmds_have_svg_shape(&email),
            "email input draws no extra chrome: {email:?}"
        );
    }

    #[test]
    fn appearance_none_number_no_spinner() {
        // appearance:none on a number input strips the spinner chrome.
        let cmds = list(
            "<html><body><input type='number' value='5'></body></html>",
            "body{margin:0} input{appearance:none}",
        );
        assert_eq!(
            fc_triangle_count(&cmds),
            0,
            "appearance:none number draws no spinner: {cmds:?}"
        );
        // The value text still renders (appearance:none keeps content).
        assert!(
            glyph_color(&cmds, "5").is_some(),
            "appearance:none number still shows its value: {cmds:?}"
        );
    }

    /// True if the list has any SvgShape (used to assert "no extra chrome").
    fn cmds_have_svg_shape(cmds: &[PaintCmd]) -> bool {
        cmds.iter()
            .any(|c| matches!(c, PaintCmd::SvgShape { .. }))
    }

    // --- E45-M3: backface-visibility culling ---

    #[test]
    fn backface_hidden_flipped_box_not_painted() {
        // rotateY(180deg) flattens (E45-M2) to Scale(cos180=-1, 1) → det = -1 < 0,
        // so the box's back faces the viewer and backface-visibility:hidden culls
        // it: no FillRect for its green background.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00; \
             transform:rotateY(180deg);backface-visibility:hidden}",
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.g == 255 && color.r == 0)),
            "flipped backface:hidden box must paint no fill: {cmds:?}"
        );
    }

    #[test]
    fn backface_visible_flipped_box_still_painted() {
        // Same flip but backface-visibility:visible (default): still painted.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00; \
             transform:rotateY(180deg);backface-visibility:visible}",
        );
        assert!(
            cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.g == 255 && color.r == 0)),
            "backface:visible box must still paint its fill: {cmds:?}"
        );
    }

    #[test]
    fn backface_hidden_front_facing_box_painted() {
        // rotateY(60deg) → Scale(cos60=0.5, 1), det = 0.5 > 0 (front-facing), so
        // even backface-visibility:hidden paints it.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00; \
             transform:rotateY(60deg);backface-visibility:hidden}",
        );
        assert!(
            cmds
                .iter()
                .any(|c| matches!(c, PaintCmd::FillRect { color, .. } if color.g == 255 && color.r == 0)),
            "front-facing backface:hidden box must paint its fill: {cmds:?}"
        );
    }

    #[test]
    fn backface_hidden_no_transform_byte_identical() {
        // Byte-identity sentinel: backface-visibility:hidden without a flipping
        // transform is identical to the untouched box.
        let html = "<html><body><div id='d'>x</div></body></html>";
        let baseline = list(html, "body{margin:0} #d{width:100px;height:50px;background:#00ff00}");
        let hidden = list(
            html,
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00;backface-visibility:hidden}",
        );
        assert_eq!(hidden, baseline, "backface:hidden w/o flip must be byte-identical");
    }

    // E68-M1: border-image fixture — saves a 48x48 `frame.png` and resolves it.
    fn list_with_frame(html: &str, css: &str) -> Vec<PaintCmd> {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("starfish-bi-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::from_pixel(48, 48, image::Rgba([0, 0, 255, 255]));
        img.save(dir.join("frame.png")).unwrap();

        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let mut images = ImageStore::new(
            file_url_from_path(&dir.join("index.html")).unwrap(),
            &LocalLoader,
        );
        images.get("frame.png");
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts), &images);
        build_display_list(&root, &styled, &fonts, &images, &doc)
    }

    #[test]
    fn border_image_emits_eight_slice_blits() {
        let cmds = list_with_frame(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:120px;height:80px;border:16px solid #000;\
             border-image-source:url(frame.png);border-image-slice:16}",
        );
        let blits: Vec<(Rect, Rect)> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::ImageBlit { dest, src, src_crop, .. } if src == "frame.png" => {
                    Some((*dest, *src_crop))
                }
                _ => None,
            })
            .collect();
        assert_eq!(blits.len(), 8, "expected 8 border-image blits: {cmds:?}");
        // Border box is 120+32 x 80+32 = 152 x 112 at origin (0,0) (body margin:0).
        let r = |x: f32, y: f32, w: f32, h: f32| Rect { x, y, width: w, height: h };
        // TL corner: dest (0,0,16,16), src (0,0,16,16).
        assert!(blits.contains(&(r(0.0, 0.0, 16.0, 16.0), r(0.0, 0.0, 16.0, 16.0))));
        // TR corner: dest (152-16,0,16,16), src (48-16,0,16,16).
        assert!(blits.contains(&(r(136.0, 0.0, 16.0, 16.0), r(32.0, 0.0, 16.0, 16.0))));
        // BR corner: dest (136,96,16,16), src (32,32,16,16).
        assert!(blits.contains(&(r(136.0, 96.0, 16.0, 16.0), r(32.0, 32.0, 16.0, 16.0))));
        // Top edge: dest (16,0,120,16), src (16,0,16,16).
        assert!(blits.contains(&(r(16.0, 0.0, 120.0, 16.0), r(16.0, 0.0, 16.0, 16.0))));
        // Left edge: dest (0,16,16,80), src (0,16,16,16).
        assert!(blits.contains(&(r(0.0, 16.0, 16.0, 80.0), r(0.0, 16.0, 16.0, 16.0))));
    }

    #[test]
    fn border_image_unresolvable_source_no_blit() {
        // Source not in the store → no border-image blit; the solid border remains.
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:120px;height:80px;border:16px solid #000;\
             border-image-source:url(missing.png);border-image-slice:16}",
        );
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })),
            "unresolvable border-image must emit no blit: {cmds:?}"
        );
    }

    #[test]
    fn border_image_repeat_tiles_top_edge() {
        // E68-M2: `border-image-repeat: repeat` on a wide top edge tiles the
        // slice → MORE than one blit for the top edge (vs 1 for stretch).
        let cmds = list_with_frame(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:240px;height:60px;border:16px solid #000;\
             border-image-source:url(frame.png);border-image-slice:16;\
             border-image-repeat:repeat}",
        );
        // Top edge dest band: y in [0,16), x in [16, 224). Count blits there.
        let top_blits = cmds
            .iter()
            .filter(|c| match c {
                PaintCmd::ImageBlit { dest, src, .. } if src == "frame.png" => {
                    dest.y == 0.0 && dest.x >= 16.0 && dest.x < 224.0 && dest.height == 16.0
                }
                _ => false,
            })
            .count();
        assert!(top_blits > 1, "repeat should tile the top edge: {top_blits} blits");
    }

    #[test]
    fn border_image_fill_emits_center_blit() {
        // E68-M2: `... fill` emits a center blit covering the inner box.
        let cmds = list_with_frame(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:120px;height:80px;border:16px solid #000;\
             border-image-source:url(frame.png);border-image-slice:16 fill}",
        );
        // Inner box: dest (16,16,120,80), src center (16,16,16,16).
        let r = |x: f32, y: f32, w: f32, h: f32| Rect { x, y, width: w, height: h };
        let has_center = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::ImageBlit { dest, src, src_crop, .. }
                if src == "frame.png"
                    && *dest == r(16.0, 16.0, 120.0, 80.0)
                    && *src_crop == r(16.0, 16.0, 16.0, 16.0)
        ));
        assert!(has_center, "fill should emit a center blit: {cmds:?}");
    }

    #[test]
    fn border_image_width_overrides_corner_thickness() {
        // E68-M2: `border-image-width: 8` (number, 8× would be huge; use a value
        // that differs from the 16px border) → corner dest thickness = the
        // border-image-width, NOT the actual border width. Default repeat=stretch
        // still emits exactly 8 slice blits (M1 regression).
        let cmds = list_with_frame(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:200px;height:120px;border:16px solid #000;\
             border-image-source:url(frame.png);border-image-slice:16;\
             border-image-width:8px}",
        );
        let blits: Vec<Rect> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::ImageBlit { dest, src, .. } if src == "frame.png" => Some(*dest),
                _ => None,
            })
            .collect();
        assert_eq!(blits.len(), 8, "stretch should emit 8 blits: {cmds:?}");
        // TL corner dest must be 8x8 (the border-image-width), not 16x16.
        let r = |x: f32, y: f32, w: f32, h: f32| Rect { x, y, width: w, height: h };
        assert!(
            blits.contains(&r(0.0, 0.0, 8.0, 8.0)),
            "TL corner should be 8x8 per border-image-width: {blits:?}"
        );
    }

    #[test]
    fn border_image_default_repeat_width_byte_identical_to_m1() {
        // Default repeat (stretch) + default width (1) + no fill must match the
        // M1 path exactly: the same 8 blits as the M1 8-slice test.
        let cmds = list_with_frame(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:120px;height:80px;border:16px solid #000;\
             border-image-source:url(frame.png);border-image-slice:16}",
        );
        let blits: Vec<(Rect, Rect)> = cmds
            .iter()
            .filter_map(|c| match c {
                PaintCmd::ImageBlit { dest, src, src_crop, .. } if src == "frame.png" => {
                    Some((*dest, *src_crop))
                }
                _ => None,
            })
            .collect();
        assert_eq!(blits.len(), 8, "default border-image must emit 8 blits");
        let r = |x: f32, y: f32, w: f32, h: f32| Rect { x, y, width: w, height: h };
        // Same anchors the M1 test asserts.
        assert!(blits.contains(&(r(0.0, 0.0, 16.0, 16.0), r(0.0, 0.0, 16.0, 16.0))));
        assert!(blits.contains(&(r(16.0, 0.0, 120.0, 16.0), r(16.0, 0.0, 16.0, 16.0))));
        assert!(blits.contains(&(r(0.0, 16.0, 16.0, 80.0), r(0.0, 16.0, 16.0, 16.0))));
    }

    #[test]
    fn no_border_image_byte_identical() {
        // A plain solid border with NO border-image emits no ImageBlit and is
        // byte-identical to the untouched box.
        let html = "<html><body><div id='d'>x</div></body></html>";
        let baseline = list(html, "body{margin:0} #d{width:120px;height:80px;border:16px solid #000}");
        assert!(
            !baseline.iter().any(|c| matches!(c, PaintCmd::ImageBlit { .. })),
            "plain solid border must emit no ImageBlit"
        );
    }
}
