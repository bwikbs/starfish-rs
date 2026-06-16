//! `ComputedStyle` and its typed value enums. See M3 design §1.

pub use starfish_css::Rgba;

/// Computed length for box-model sizing/spacing. `em`/`rem` are resolved to
/// `Px` at compute time.
#[derive(Debug, Clone, PartialEq)]
pub enum Length {
    Px(f32),
    /// `50%` → `Percent(50.0)`; resolved against the containing block in M4.
    Percent(f32),
    Auto,
    /// `calc()` reduced to its linear form `px + percent% * cb` (E13-M2). Only
    /// produced when both a px and a percent part are present; pure-px / pure-%
    /// calc() normalizes back to `Px`/`Percent` (so non-calc pages are unchanged).
    Calc {
        px: f32,
        percent: f32,
    },
    /// An unfoldable math-function tree (E24-M1): `min`/`max`/`clamp` mixing px
    /// and %, resolved against the containing block at layout time. Pure-px /
    /// pure-% math folds to `Px`/`Percent` at parse, so this variant only
    /// survives when the comparison depends on the basis.
    Math(std::rc::Rc<crate::calc::MathExpr>),
}

/// `box-sizing`. Initial ContentBox; NOT inherited (E13-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

/// `object-fit` (E15-M1). How a replaced element's content is fitted into its
/// content box. Initial `Fill`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// `image-rendering` (E15-M1). Selects the blit sampler: `Smooth` = bilinear,
/// everything else (incl. `Auto`) = nearest. Initial `Auto`; INHERITED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRendering {
    Auto,
    Smooth,
    Pixelated,
    CrispEdges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    /// block-level table container (E7-M3).
    Table,
    /// atomic-inline table container (E7-M3).
    InlineTable,
    /// thead / tbody / tfoot (E7-M3).
    TableRowGroup,
    /// tr (E7-M3).
    TableRow,
    /// td / th (E7-M3).
    TableCell,
    /// generates no box; children splice into the parent flow (E34-M1).
    Contents,
    /// block container that always establishes a new block formatting context
    /// (contains its floats) (E34-M2).
    FlowRoot,
    None,
}

/// `border-collapse`. Initial `Separate`; INHERITED. M3 implements only
/// `Separate`; `Collapse` is parsed but falls back to the separate model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderCollapse {
    Separate,
    Collapse,
}

/// One explicit track size in a `grid-template-columns`/`-rows` list (E5-M1).
/// `minmax()`/`fit-content()`/`min-content`/`max-content` deferred.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSize {
    /// Fixed length, e.g. `100px` (`em`/`rem` folded to px at compute).
    Px(f32),
    /// `<percentage>` of the grid container's content size on that axis.
    Percent(f32),
    /// Flexible `<flex>` track, e.g. `1fr`.
    Fr(f32),
    /// `auto` — sized to the max content size of items in the track.
    Auto,
}

/// One end (start or end) of a grid item's placement on one axis (E5-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridPlacement {
    /// `auto` — line chosen by placement/auto-flow.
    Auto,
    /// An explicit 1-based line number. Negative counts from the end line
    /// (`-1` = last line). `0` is normalized to `Auto` at parse time.
    Line(i32),
    /// `span N` — spans N tracks from the opposite, resolved edge (N ≥ 1).
    Span(u32),
}

/// A resolved placement for one axis: the `start` and `end` lines (E5-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridLine {
    pub start: GridPlacement,
    pub end: GridPlacement,
}

impl GridLine {
    pub const AUTO: GridLine = GridLine {
        start: GridPlacement::Auto,
        end: GridPlacement::Auto,
    };
}

/// `flex-direction`. Initial `Row`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    /// True if the main axis is horizontal.
    pub fn is_row(self) -> bool {
        matches!(self, FlexDirection::Row | FlexDirection::RowReverse)
    }
    /// True if items are placed against the main-end (reverse order).
    pub fn is_reverse(self) -> bool {
        matches!(
            self,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        )
    }
}

/// `flex-wrap`. Initial `Nowrap`; NOT inherited. (`wrap-reverse` deferred.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexWrap {
    Nowrap,
    Wrap,
}

/// `justify-content` (main-axis distribution). Initial `FlexStart`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// `align-items` (default cross-axis alignment of items). Initial `Stretch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    /// M3: simplified to flex-start (cross-start).
    Baseline,
}

/// `align-self` (per-item override of `align-items`). Initial `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignSelf {
    Auto,
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
}

/// `position`. Initial `Static`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// `sticky` (E16-M4). One-shot render has scroll offset 0, so a sticky box is
    /// never "scrolled past" → it resolves to its in-flow (static) position;
    /// top/right/bottom/left insets are ignored. Real scroll tracking is out of
    /// scope (ROADMAP non-goals).
    Sticky,
}

/// `float`. Initial `None`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

/// `clear`. Initial `None`; NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

/// `overflow` (E13-M4). Initial `Visible`; NOT inherited. `scroll`/`auto` map to
/// `Visible` (scrollbars are out of scope; see `properties::overflow_of`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
}

/// `text-overflow` (E16-M4). Initial `Clip`; NOT inherited. `Ellipsis` truncates
/// the line at layout time (only when the line can't wrap and overflow is
/// clipped); paint is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOverflow {
    Clip,
    Ellipsis,
}

/// `hyphens` (E22-M3). Initial `Manual`; inherited. `Auto` behaves like
/// `Manual` (no dictionary): soft hyphens (U+00AD) are honored as break
/// opportunities, but words are never auto-hyphenated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hyphens {
    None,
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// Also covers `hidden`.
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
    // groove/ridge/inset/outset still fold to `Solid` (in `style_keyword`).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
}

/// `text-justify`. Initial `Auto`; INHERITED (E22-M2). `Auto`/`InterWord` both
/// distribute slack via inter-word gaps; `None` disables justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextJustify {
    Auto,
    InterWord,
    None,
}

/// `direction`. Initial `Ltr`; INHERITED. Sets the inline base direction (E6-M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// `unicode-bidi` (minimal subset, E6-M3). Initial `Normal`; NOT inherited.
/// Only `BidiOverride` meaningfully changes behaviour in M3 (forces the run's
/// direction = `direction`, ignoring character types). `Embed`/`Isolate` behave
/// like `Normal` for a single run (documented limit §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicodeBidi {
    Normal,
    Embed,
    BidiOverride,
    Isolate,
}

/// `writing-mode` (E18-M3). Initial `HorizontalTb`; INHERITED. `sideways-rl`/
/// `sideways-lr` are not modeled (dropped at parse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritingMode {
    HorizontalTb,
    VerticalRl,
    VerticalLr,
}

impl WritingMode {
    /// True for the two vertical modes (block axis runs horizontally).
    pub fn is_vertical(self) -> bool {
        matches!(self, WritingMode::VerticalRl | WritingMode::VerticalLr)
    }
}

/// A `clip-path` basic shape (E32-M1). Lengths/percentages resolve against the
/// box at paint time; percentages are relative to the border-box size.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipShape {
    /// `inset(top right bottom left)` (margins inward from each edge).
    Inset {
        top: LengthPct,
        right: LengthPct,
        bottom: LengthPct,
        left: LengthPct,
    },
    /// `circle(r at cx cy)`; `r` default closest-side.
    Circle {
        r: ClipRadius,
        cx: LengthPct,
        cy: LengthPct,
    },
    /// `ellipse(rx ry at cx cy)`.
    Ellipse {
        rx: ClipRadius,
        ry: ClipRadius,
        cx: LengthPct,
        cy: LengthPct,
    },
    /// `polygon(x1 y1, x2 y2, …)`.
    Polygon(Vec<(LengthPct, LengthPct)>),
}

/// A `circle`/`ellipse` radius (E32-M1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipRadius {
    Length(LengthPct),
    ClosestSide,
    FarthestSide,
}

/// `content-visibility` (E31-M1). `Auto` is treated as `Visible` (no
/// viewport-intersection model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentVisibility {
    #[default]
    Visible,
    Hidden,
    Auto,
}

/// `container-type` (E25-M1): whether the element establishes a query container,
/// and on which axes its size can be queried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerType {
    /// Not a query container (`normal`).
    #[default]
    Normal,
    /// `inline-size`: a query container sized on the inline axis only.
    InlineSize,
    /// `size`: a query container sized on both axes.
    Size,
}

/// `text-orientation` (E18-M3). Initial `Mixed`; INHERITED. In this MVP `Mixed`
/// is treated like `Sideways` (rotated runs); `Upright` stacks single chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOrientation {
    Mixed,
    Upright,
    Sideways,
}

/// `text-transform`. Initial `None`; INHERITED (E6-M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

/// `white-space`. Initial `Normal`; INHERITED (E6-M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    Nowrap,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    /// Whitespace runs (incl. newlines, except a preserved `\n`) collapse to one
    /// space. True for normal / nowrap / pre-line.
    pub fn collapses(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        )
    }
    /// Segment breaks (`\n`) are preserved as forced line breaks. True for
    /// pre / pre-wrap / pre-line / break-spaces.
    pub fn preserves_newlines(self) -> bool {
        matches!(
            self,
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine | WhiteSpace::BreakSpaces
        )
    }
    /// The line may wrap at soft break opportunities (spaces). True for
    /// normal / pre-wrap / pre-line / break-spaces.
    pub fn wraps(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal
                | WhiteSpace::PreWrap
                | WhiteSpace::PreLine
                | WhiteSpace::BreakSpaces
        )
    }
}

/// `word-break`. Initial `Normal`; INHERITED (E22-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordBreak {
    Normal,
    BreakAll,
    KeepAll,
}

/// `overflow-wrap` (alias `word-wrap`). Initial `Normal`; INHERITED (E22-M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowWrap {
    Normal,
    BreakWord,
    Anywhere,
}

/// `tab-size`. Initial `Number(8.0)`; INHERITED (E22-M1). `Number(n)` is `n`
/// advance widths of `'0'`; `Px(p)` is an absolute length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabSize {
    Number(f32),
    Px(f32),
}

/// `font-weight`, normalized to a numeric weight. `normal`→400, `bold`→700.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontWeight(pub u16);

/// `font-style`. Initial `Normal`; inherited. `oblique <angle>` folds to
/// `Oblique` (the angle is not modeled). Matched like italic at font selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

/// `text-decoration-line`. A bitset so underline+overline can combine.
/// `NONE` is the empty set. Stored as a small `u8` wrapper (no external dep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextDecorationLine(u8);

impl TextDecorationLine {
    pub const NONE: TextDecorationLine = TextDecorationLine(0);
    pub const UNDERLINE: TextDecorationLine = TextDecorationLine(1);
    pub const OVERLINE: TextDecorationLine = TextDecorationLine(2);
    pub const LINE_THROUGH: TextDecorationLine = TextDecorationLine(4);

    pub fn contains(self, f: TextDecorationLine) -> bool {
        self.0 & f.0 != 0
    }
    pub fn insert(&mut self, f: TextDecorationLine) {
        self.0 |= f.0;
    }
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyleType {
    Disc,
    Circle,
    Square,
    Decimal,
    None,
}

/// Only `Outside` is supported in M1 but the enum is modelled for forward use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStylePosition {
    Outside,
}

/// One background image source (E16-M2). A `url(...)` raster/SVG image, or a
/// CSS `linear-gradient(...)`/`radial-gradient(...)`/`conic-gradient(...)`.
/// (`none`/unknown sources don't produce a layer.)
#[derive(Debug, Clone, PartialEq)]
pub enum BgImage {
    Url(String),
    Gradient(LinearGradient),
    /// `radial-gradient(...)` (E16-M3).
    Radial(RadialGradient),
    /// `conic-gradient(...)` (E16-M3).
    Conic(ConicGradient),
}

/// `background-size` (E16-M2). `Explicit` carries one axis spec per axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    Explicit(BgSizeAxis, BgSizeAxis),
}

/// One axis of an explicit `background-size` (E16-M2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSizeAxis {
    Auto,
    Px(f32),
    Percent(f32),
}

/// `background-repeat` (E16-M2). The two-keyword form folds to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgRepeat {
    Repeat,
    NoRepeat,
    RepeatX,
    RepeatY,
}

/// One comma-separated background layer (E16-M2). Layers paint back-to-front
/// (last layer is bottom-most), above the single `background_color`.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundLayer {
    pub image: BgImage,
    pub size: BgSize,
    pub position: (LengthPct, LengthPct),
    pub repeat: BgRepeat,
}

/// One `mask-image` source (E21-M3). A `url(...)` raster/SVG image, or a CSS
/// `linear-gradient(...)`/`radial-gradient(...)`. (`none`/unknown sources clear
/// the mask.) `conic` is not modelled (rare for masks).
#[derive(Debug, Clone, PartialEq)]
pub enum MaskImage {
    Url(String),
    Gradient(LinearGradient),
    Radial(RadialGradient),
}

/// `mask-mode` (E21-M3). `Alpha` uses the source's alpha channel as coverage;
/// `Luminance` uses its luminance (× alpha). Initial `Alpha`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskMode {
    Alpha,
    Luminance,
}

/// Geometry box for `mask-origin`/`mask-clip` (E32-M2). `mask-clip` also allows
/// `NoClip`. Initial (for mask) `BorderBox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskGeometryBox {
    BorderBox,
    PaddingBox,
    ContentBox,
    NoClip,
}

/// A computed `mask` (E21-M3) — the single-layer MVP. Reuses `BgSize`/`BgRepeat`
/// for sizing/tiling of `url` sources (gradient masks box-fill, ignoring
/// size/repeat). `origin`/`clip` (E32-M2) pick the geometry box the position is
/// resolved against / the painted area is clipped to. NOT inherited; initial
/// `None`. Forces an offscreen layer.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskSpec {
    pub image: MaskImage,
    pub mode: MaskMode,
    pub size: BgSize,
    pub position: (LengthPct, LengthPct),
    pub repeat: BgRepeat,
    pub origin: MaskGeometryBox,
    pub clip: MaskGeometryBox,
}

/// A parsed `linear-gradient(...)` — the M5 subset. `angle_deg` is in CSS
/// degrees (0deg = to top, 90deg = to right, growing clockwise). `stops` has
/// ≥ 2 entries; `pos` is a 0..1 fraction along the gradient line, or `None`
/// (auto — spread evenly by the painter). (E2-M5 §1.1)
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub angle_deg: f32,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub color: Rgba,
    /// 0..1 along the line; `None` = auto-spaced.
    pub pos: Option<f32>,
}

/// A parsed `radial-gradient(...)` — the M3 MVP: an `ellipse`-as-circle sized
/// `farthest-corner` from the box center, ignoring any shape/size/position
/// prefix (E16-M3). `stops` has ≥ 2 entries (same `pos` semantics as linear).
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub stops: Vec<GradientStop>,
}

/// A parsed `conic-gradient(...)` — the M3 MVP: `from <angle>` (default 0deg =
/// up, growing clockwise), `at ...` ignored (E16-M3). `stops` has ≥ 2 entries;
/// `pos` is a 0..1 turn-fraction (only `%`/auto parsed).
#[derive(Debug, Clone, PartialEq)]
pub struct ConicGradient {
    pub from_deg: f32,
    pub stops: Vec<GradientStop>,
}

/// `box-shadow` — the M5 subset: a single outset shadow. (E2-M5 §3.1)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    /// ≥ 0.
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
}

/// `text-shadow` — the M3 subset: a single shadow (offset + blur + color), no
/// spread. INHERITED; initial absent (E16-M3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    /// ≥ 0.
    pub blur: f32,
    pub color: Rgba,
}

/// `outline` (E16-M3). NOT inherited; initial `{width:0, style:None, color:black,
/// offset:0}`. The stroke grows OUTWARD from the border box (offset by `offset`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outline {
    pub width: f32,
    pub style: BorderStyle,
    pub color: Rgba,
    pub offset: f32,
}

/// A `<length-percentage>` that must survive to paint time (a `%` resolves
/// against the box size, unknown in `style`). Used by `transform`'s translate
/// and by `transform-origin`. (`em`/`rem` already folded to px at parse.) (E5-M3)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthPct {
    Px(f32),
    /// `50%` → `Percent(50.0)`.
    Percent(f32),
}

/// One parsed 2D transform function (E5-M3). Angles are normalized to RADIANS
/// at parse time; scales are unitless f32; translate keeps px/% (the `%`
/// resolves against the border-box at paint). `matrix` stores a,b,c,d,e,f.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformFn {
    /// x, y.
    Translate(LengthPct, LengthPct),
    /// sx, sy.
    Scale(f32, f32),
    /// radians, clockwise.
    Rotate(f32),
    /// ax, ay in radians.
    Skew(f32, f32),
    /// a, b, c, d, e, f.
    Matrix([f32; 6]),
}

/// One parsed `filter` function (E21-M1). Lengths are folded to px and angles to
/// radians at parse time. Amounts are unitless fractions (`%` → /100).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterFn {
    /// Gaussian blur standard deviation in px (≥ 0).
    Blur(f32),
    /// `brightness(b)` — linear scale (no clamp; may exceed 1).
    Brightness(f32),
    /// `contrast(k)` — pivot-0.5 scale (no clamp).
    Contrast(f32),
    /// `grayscale(x)` — lerp toward luminance, x in 0..1.
    Grayscale(f32),
    /// `sepia(x)` — lerp toward the sepia matrix, x in 0..1.
    Sepia(f32),
    /// `invert(x)` — lerp toward the inverse, x in 0..1.
    Invert(f32),
    /// `saturate(s)` — saturation scale (no clamp).
    Saturate(f32),
    /// `hue-rotate(θ)` — hue rotation in radians.
    HueRotate(f32),
    /// `opacity(o)` — multiply alpha, o in 0..1.
    Opacity(f32),
    /// `drop-shadow(dx dy blur color)` — offsets/blur in px, blur ≥ 0.
    DropShadow {
        dx: f32,
        dy: f32,
        blur: f32,
        color: Rgba,
    },
}

/// CSS blend mode (E21-M2). Drives `mix-blend-mode` (composite a box's layer with
/// the backdrop) and `background-blend-mode` (blend background layers with each
/// other). Initial `Normal` (= source-over, no blending). NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// `isolation` (E32-M3). `Isolate` forces a new stacking context / offscreen
/// group so a descendant's `mix-blend-mode` blends only within this subtree,
/// not against the backdrop outside it. Initial `Auto`. NOT inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Auto,
    Isolate,
}

/// `content` (E7-M2). On a `::before`/`::after` pseudo it determines whether a
/// generated box is created and its text. `attr()` is resolved at style time, so
/// `Text` already holds the final string. NOT inherited; initial `Normal`.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// `normal` (and the initial value): no generated box on a pseudo.
    Normal,
    /// `none`: no generated box.
    None,
    /// A resolved text string (may be empty: `content:""` → an empty box).
    Text(String),
}

/// `line-height`. Resolved to px against the element's own font-size in M4.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// ~1.2 × font-size; M4 picks the factor.
    Normal,
    /// Unitless multiplier of font-size.
    Number(f32),
    /// Absolute length (em/rem already folded to px).
    Px(f32),
}

/// `animation-timing-function` (E17-M1). Named presets are folded to their
/// cubic-bezier control points at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    /// Control points `(x1, y1, x2, y2)` of a cubic Bézier in `[0,1]²` (x).
    CubicBezier(f32, f32, f32, f32),
    /// `steps(n, jump-term)`.
    Steps(u32, JumpTerm),
}

/// The `steps()` jump term (E17-M1). Only `start`/`end` are modeled; the other
/// terms fold to `End`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpTerm {
    Start,
    End,
}

/// `animation-direction` (E17-M1). Initial `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

/// `animation-fill-mode` (E17-M1). Initial `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimFillMode {
    None,
    Forwards,
    Backwards,
    Both,
}

/// A resolved CSS animation (E17-M1): the longhands folded together. `name` ""
/// means "no animation" (the default).
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub name: String,
    pub duration_s: f32,
    pub timing: Easing,
    pub delay_s: f32,
    /// `f32::INFINITY` for `infinite`.
    pub iteration_count: f32,
    pub direction: AnimDirection,
    pub fill_mode: AnimFillMode,
}

/// Which property a [`Transition`] watches (E17-M3).
#[derive(Debug, Clone, PartialEq)]
pub enum TransitionProp {
    /// `transition-property: all` — every transitionable property.
    All,
    /// A single property name (lowercased ident).
    Name(String),
}

/// A resolved CSS transition (E17-M3): the longhands for one property folded
/// together. Sampled one-shot from a `from` style to the current `to` style.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub property: TransitionProp,
    pub duration_s: f32,
    pub timing: Easing,
    pub delay_s: f32,
}

impl Default for Animation {
    fn default() -> Animation {
        Animation {
            name: String::new(),
            duration_s: 0.0,
            timing: Easing::CubicBezier(0.25, 0.1, 0.25, 1.0), // `ease`
            delay_s: 0.0,
            iteration_count: 1.0,
            direction: AnimDirection::Normal,
            fill_mode: AnimFillMode::None,
        }
    }
}

impl Easing {
    /// Evaluate the easing's output progress at input progress `t` in `[0,1]`.
    /// Endpoints short-circuit (t<=0→0, t>=1→1) for stability.
    pub fn eval(self, t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        match self {
            Easing::Linear => t,
            Easing::CubicBezier(x1, y1, x2, y2) => {
                let s = solve_bezier_x(x1, x2, t);
                bezier_axis(y1, y2, s)
            }
            Easing::Steps(n, term) => {
                let n = n.max(1) as f32;
                let v = match term {
                    JumpTerm::End => (t * n).floor() / n,
                    JumpTerm::Start => (t * n).ceil() / n,
                };
                v.clamp(0.0, 1.0)
            }
        }
    }
}

/// One axis of a cubic Bézier with fixed endpoints `0` and `1`: B(s) =
/// 3(1-s)²s·p1 + 3(1-s)s²·p2 + s³.
fn bezier_axis(p1: f32, p2: f32, s: f32) -> f32 {
    let mt = 1.0 - s;
    3.0 * mt * mt * s * p1 + 3.0 * mt * s * s * p2 + s * s * s
}

/// Solve `x(s) = t` for `s` in `[0,1]`: Newton's method seeded at `s=t`
/// (≤8 iters), falling back to bisection (~20 iters, tol 1e-6).
fn solve_bezier_x(x1: f32, x2: f32, t: f32) -> f32 {
    let mut s = t;
    for _ in 0..8 {
        let x = bezier_axis(x1, x2, s) - t;
        if x.abs() < 1e-6 {
            return s;
        }
        // dx/ds
        let mt = 1.0 - s;
        let d = 3.0 * mt * mt * x1 + 6.0 * mt * s * (x2 - x1) + 3.0 * s * s * (1.0 - x2);
        if d.abs() < 1e-6 {
            break;
        }
        s -= x / d;
    }
    // Bisection fallback.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut s = t;
    for _ in 0..20 {
        let x = bezier_axis(x1, x2, s);
        if (x - t).abs() < 1e-6 {
            return s;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    s
}

/// Resolved, typed values for the layout-sufficient property subset (§1.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // box generation
    pub display: Display,

    // box dimensions
    pub width: Length,
    pub height: Length,

    // `aspect-ratio` (E18-M1) as a width/height ratio. NOT inherited; initial
    // `auto` ⇒ `None`. The two-value `auto <ratio>` fallback is not modeled.
    pub aspect_ratio: Option<f32>,

    // box-sizing + min/max constraints (E13-M1). NOT inherited. `min_*` Auto ⇒ 0
    // (no lower bound); `max_*` Auto ⇒ no upper bound (+∞).
    pub box_sizing: BoxSizing,
    pub min_width: Length,
    pub min_height: Length,
    pub max_width: Length,
    pub max_height: Length,

    // margin (TRBL)
    pub margin_top: Length,
    pub margin_right: Length,
    pub margin_bottom: Length,
    pub margin_left: Length,

    // padding (TRBL)
    pub padding_top: Length,
    pub padding_right: Length,
    pub padding_bottom: Length,
    pub padding_left: Length,

    // border (TRBL widths + one shared style + one shared color for M3)
    pub border_top_width: f32,
    pub border_right_width: f32,
    pub border_bottom_width: f32,
    pub border_left_width: f32,
    pub border_style: BorderStyle,
    pub border_color: Rgba,

    // color / background (E16-M2)
    pub color: Rgba,
    /// The single solid background color (paints at the bottom of the stack).
    /// Initial `transparent`; NOT inherited.
    pub background_color: Rgba,
    /// `background-image` layers, source order (index 0 = top-most). Empty in
    /// the common no-image case. NOT inherited.
    pub background_layers: Vec<BackgroundLayer>,

    // visual effects (E2-M5)
    /// Corner radii in px: TL, TR, BR, BL. All-zero = sharp corners.
    pub border_radius: [f32; 4],
    pub box_shadow: Option<BoxShadow>,
    /// `text-shadow` (E16-M3). INHERITED; initial `None`.
    pub text_shadow: Option<TextShadow>,
    /// `outline` (E16-M3). NOT inherited; initial zero-width `None` style.
    pub outline: Outline,
    /// 0..1; 1.0 = fully opaque (no offscreen layer).
    pub opacity: f32,

    // transforms (E5-M3) — paint-time only, NOT inherited.
    /// Empty = `none` (no transform, fast path).
    pub transform: Vec<TransformFn>,
    /// The pivot. Initial `(Percent(50), Percent(50))` = center.
    pub transform_origin: (LengthPct, LengthPct),
    /// `filter` (E21-M1) — paint-time only, NOT inherited. Empty = `none` (fast
    /// path, no offscreen layer). Functions apply in source order.
    pub filter: Vec<FilterFn>,
    /// `mix-blend-mode` (E21-M2) — how the box's layer composites with the
    /// backdrop. Initial `Normal` (source-over, no offscreen layer). NOT inherited.
    pub mix_blend_mode: BlendMode,
    /// `isolation` (E32-M3). `Isolate` forces a new stacking context / offscreen
    /// group so a descendant's `mix-blend-mode` blends only within this subtree,
    /// not against the backdrop outside it. Initial `Auto`. NOT inherited.
    pub isolation: Isolation,
    /// `background-blend-mode` (E21-M2) — one mode per background layer (source
    /// order), blending the layers with each other + the bg color. Initial empty
    /// (no blending). NOT inherited.
    pub background_blend_mode: Vec<BlendMode>,
    /// `mask-image` (E21-M3) — paint-time only, NOT inherited. `None` = no mask
    /// (fast path, no offscreen layer). When `Some`, forces an offscreen layer.
    pub mask: Option<MaskSpec>,
    /// `backdrop-filter` (E21-M3) — paint-time only, NOT inherited. Empty = none
    /// (no backdrop snapshot). Functions apply in source order to the backdrop.
    pub backdrop_filter: Vec<FilterFn>,

    // replaced-content fitting (E15-M1).
    /// `object-fit`; initial `Fill`, NOT inherited.
    pub object_fit: ObjectFit,
    /// `object-position`; initial `(Percent(50), Percent(50))` = center, NOT
    /// inherited. Resolved against the free space at fit time.
    pub object_position: (LengthPct, LengthPct),
    /// `image-rendering`; initial `Auto`, INHERITED. Drives the blit sampler.
    pub image_rendering: ImageRendering,

    // text / font
    pub font_size: f32,
    pub font_weight: FontWeight,
    pub font_style: FontStyle,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    /// `text-indent`; initial `Px(0)`, INHERITED (E22-M2). Applies to the first
    /// line; `%` resolves against the containing block's content width.
    pub text_indent: LengthPct,
    /// `text-justify`; initial `Auto`, INHERITED (E22-M2).
    pub text_justify: TextJustify,
    pub font_family: Vec<String>,

    // writing mode (E18-M3) — INHERITED. Default `HorizontalTb` keeps every
    // gated layout/paint branch on its existing (horizontal) else-path.
    pub writing_mode: WritingMode,
    pub text_orientation: TextOrientation,

    // container queries (E25-M1) — NOT inherited. `container_type` establishes a
    // query container; `container_name` lets `@container <name>` target it.
    pub container_type: ContainerType,
    pub container_name: Option<String>,

    /// clip-path (E32-M1) — NOT inherited.
    pub clip_path: Option<ClipShape>,

    // containment (E31-M1) — NOT inherited.
    pub content_visibility: ContentVisibility,
    pub contain_size: bool,
    pub contain_layout: bool,
    pub contain_paint: bool,

    // bidi / spaced / transformed text (E6-M3)
    pub direction: Direction,
    pub unicode_bidi: UnicodeBidi,
    /// extra px after each char; inherited, initial 0.
    pub letter_spacing: f32,
    /// extra px at each U+0020 space; inherited, initial 0.
    pub word_spacing: f32,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    pub tab_size: TabSize,
    /// `hyphens` (E22-M3). Inherited; initial `Manual`.
    pub hyphens: Hyphens,

    // text decoration (M1)
    pub text_decoration_line: TextDecorationLine,

    // list (M1)
    pub list_style_type: ListStyleType,
    pub list_style_position: ListStylePosition,

    // out-of-flow / positioning (M2)
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    /// `overflow` (E13-M4). NOT inherited; initial `Visible`.
    pub overflow: Overflow,
    /// `text-overflow` (E16-M4). NOT inherited; initial `Clip`.
    pub text_overflow: TextOverflow,
    /// `-webkit-line-clamp` (E22-M3). NOT inherited; initial `None`. `Some(n)`
    /// limits the block to `n` lines with a trailing ellipsis on the last.
    pub line_clamp: Option<u32>,
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,

    // flex container (M3)
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,

    // flex item (M3)
    pub align_self: AlignSelf,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,

    // gap (M3) — applies to flex and grid containers
    pub row_gap: Length,
    pub column_gap: Length,

    // multi-column (E18-M2) — NOT inherited. Reuse `column_gap` for the gap.
    /// `column-count`; initial `None` (auto).
    pub column_count: Option<u32>,
    /// `column-width`; initial `None` (auto).
    pub column_width: Option<Length>,
    /// `column-rule-width`; initial `0`.
    pub column_rule_width: f32,
    /// `column-rule-style`; initial `None` (reuse the border enum).
    pub column_rule_style: BorderStyle,
    /// `column-rule-color`; initial black.
    pub column_rule_color: Rgba,

    // grid container (E5-M1)
    pub grid_template_columns: Vec<TrackSize>,
    pub grid_template_rows: Vec<TrackSize>,
    /// `grid-template-rows: masonry` (E31-M2): items pack into the columns by
    /// shortest-column-first instead of forming explicit rows.
    pub grid_masonry_rows: bool,
    /// `grid-template-columns: subgrid` (E31-M3): this grid adopts its parent
    /// grid's column tracks (spanned by its placement) instead of its own.
    pub subgrid_columns: bool,

    // grid item (E5-M1)
    pub grid_column: GridLine,
    pub grid_row: GridLine,

    // grid alignment + areas (E5-M2) — reuse the flex enums.
    /// inline-axis container default; grid initial `Stretch`.
    pub justify_items: AlignItems,
    /// inline-axis per-item; `Auto` → `justify_items`.
    pub justify_self: AlignSelf,
    /// row-track distribution; initial `FlexStart` (= grid `start`).
    pub align_content: JustifyContent,
    /// rows of cell names; `"."` = empty cell. Empty = none.
    pub grid_template_areas: Vec<Vec<String>>,
    /// `grid-area: <name>` (else `None`). Lowercased ident.
    pub grid_area_name: Option<String>,

    // generated content (E7-M2) — NOT inherited; only consumed on ::before/::after.
    pub content: Content,

    // animation (E17-M1) — NOT inherited; initial `None` (no animation). Only
    // populated when an `animation-*` property is set, so non-animated pages keep
    // a `None` here (byte-identical StyledTree).
    pub animation: Option<Animation>,

    // transitions (E17-M3) — NOT inherited; initial empty (no transition). Only
    // populated when a `transition-*` property is set, so non-transition pages
    // keep an empty Vec here (byte-identical StyledTree).
    pub transitions: Vec<Transition>,

    // CSS counters (E16-M1) — NOT inherited. `(name, value)` pairs in source
    // order; applied to the live counter stack during the style walk.
    pub counter_reset: Vec<(String, i32)>,
    pub counter_increment: Vec<(String, i32)>,

    // table (E7-M3) — INHERITED.
    /// Horizontal + vertical spacing between/around cell borders, in px. Only
    /// meaningful in the `Separate` model. Field initial `(0,0)`; UA sheet sets
    /// tables to `2px`.
    pub border_spacing: (f32, f32),
    /// `Separate` (M3) or `Collapse` (deferred → treated as separate).
    pub border_collapse: BorderCollapse,

    // custom properties (E13-M2) — INHERITED. `--name` → its raw component
    // values. Shared via `Rc` so inheritance is a cheap pointer clone; an empty
    // map (the common case) keeps non-`var()` pages byte-identical.
    pub(crate) custom_props:
        std::rc::Rc<std::collections::HashMap<String, Vec<starfish_css::Component>>>,
}

const TRANSPARENT: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};
const BLACK: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};

impl ComputedStyle {
    /// The all-initial style. Doubles as the synthetic parent of the root.
    pub fn initial() -> ComputedStyle {
        ComputedStyle {
            display: Display::Inline,
            width: Length::Auto,
            height: Length::Auto,
            aspect_ratio: None,
            box_sizing: BoxSizing::ContentBox,
            min_width: Length::Auto,
            min_height: Length::Auto,
            max_width: Length::Auto,
            max_height: Length::Auto,
            margin_top: Length::Px(0.0),
            margin_right: Length::Px(0.0),
            margin_bottom: Length::Px(0.0),
            margin_left: Length::Px(0.0),
            padding_top: Length::Px(0.0),
            padding_right: Length::Px(0.0),
            padding_bottom: Length::Px(0.0),
            padding_left: Length::Px(0.0),
            border_top_width: 0.0,
            border_right_width: 0.0,
            border_bottom_width: 0.0,
            border_left_width: 0.0,
            border_style: BorderStyle::None,
            border_color: BLACK, // currentColor = initial color
            color: BLACK,
            background_color: TRANSPARENT,
            background_layers: Vec::new(),
            border_radius: [0.0; 4],
            box_shadow: None,
            text_shadow: None,
            outline: Outline {
                width: 0.0,
                style: BorderStyle::None,
                color: BLACK,
                offset: 0.0,
            },
            opacity: 1.0,
            transform: Vec::new(),
            transform_origin: (LengthPct::Percent(50.0), LengthPct::Percent(50.0)),
            filter: Vec::new(),
            mix_blend_mode: BlendMode::Normal,
            isolation: Isolation::Auto,
            background_blend_mode: Vec::new(),
            mask: None,
            backdrop_filter: Vec::new(),
            object_fit: ObjectFit::Fill,
            object_position: (LengthPct::Percent(50.0), LengthPct::Percent(50.0)),
            image_rendering: ImageRendering::Auto,
            font_size: 16.0,
            font_weight: FontWeight(400),
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            text_indent: LengthPct::Px(0.0),
            text_justify: TextJustify::Auto,
            font_family: Vec::new(),
            writing_mode: WritingMode::HorizontalTb,
            text_orientation: TextOrientation::Mixed,
            direction: Direction::Ltr,
            unicode_bidi: UnicodeBidi::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
            tab_size: TabSize::Number(8.0),
            hyphens: Hyphens::Manual,
            text_decoration_line: TextDecorationLine::NONE,
            list_style_type: ListStyleType::Disc, // CSS initial is `disc`
            list_style_position: ListStylePosition::Outside,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            overflow: Overflow::Visible,
            text_overflow: TextOverflow::Clip,
            line_clamp: None,
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Nowrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: AlignSelf::Auto,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            row_gap: Length::Px(0.0),
            column_gap: Length::Px(0.0),
            column_count: None,
            column_width: None,
            column_rule_width: 0.0,
            column_rule_style: BorderStyle::None,
            column_rule_color: BLACK,
            grid_template_columns: Vec::new(),
            grid_template_rows: Vec::new(),
            grid_masonry_rows: false,
            subgrid_columns: false,
            grid_column: GridLine::AUTO,
            grid_row: GridLine::AUTO,
            justify_items: AlignItems::Stretch,
            justify_self: AlignSelf::Auto,
            align_content: JustifyContent::FlexStart,
            grid_template_areas: Vec::new(),
            grid_area_name: None,
            content: Content::Normal,
            animation: None,
            transitions: Vec::new(),
            counter_reset: Vec::new(),
            counter_increment: Vec::new(),
            border_spacing: (0.0, 0.0),
            border_collapse: BorderCollapse::Separate,
            custom_props: std::rc::Rc::new(std::collections::HashMap::new()),
            container_type: ContainerType::Normal,
            container_name: None,
            clip_path: None,
            content_visibility: ContentVisibility::Visible,
            contain_size: false,
            contain_layout: false,
            contain_paint: false,
        }
    }

    /// Produce a fresh child style: inherited properties copied from `self`,
    /// everything else reset to initial. The cascade then overwrites onto this.
    pub(crate) fn inherit_from(&self) -> ComputedStyle {
        let mut child = ComputedStyle::initial();
        // Inherited set (§1.3): color, font_size, font_weight, line_height,
        // text_align, font_family.
        child.color = self.color;
        child.font_size = self.font_size;
        child.font_weight = self.font_weight;
        child.font_style = self.font_style;
        child.line_height = self.line_height;
        child.text_align = self.text_align;
        child.text_indent = self.text_indent;
        child.text_justify = self.text_justify;
        child.font_family = self.font_family.clone();
        // E18-M3 writing-mode + text-orientation are inherited.
        child.writing_mode = self.writing_mode;
        child.text_orientation = self.text_orientation;
        // E6-M3 inherited text props (unicode_bidi is NOT inherited).
        child.direction = self.direction;
        child.letter_spacing = self.letter_spacing;
        child.word_spacing = self.word_spacing;
        child.text_transform = self.text_transform;
        child.white_space = self.white_space;
        // E22-M1 line-breaking controls are inherited.
        child.word_break = self.word_break;
        child.overflow_wrap = self.overflow_wrap;
        child.tab_size = self.tab_size;
        // E22-M3 hyphens is inherited (line-clamp is NOT).
        child.hyphens = self.hyphens;
        // E16-M3 text-shadow is inherited (outline is NOT).
        child.text_shadow = self.text_shadow;
        // list-style-* are inherited; text-decoration-line is NOT (§1.3).
        child.list_style_type = self.list_style_type;
        child.list_style_position = self.list_style_position;
        // E7-M3 table props are inherited.
        child.border_spacing = self.border_spacing;
        child.border_collapse = self.border_collapse;
        // E13-M2 custom properties are inherited (cheap Rc clone).
        child.custom_props = self.custom_props.clone();
        // E15-M1 image-rendering is inherited; object-fit/position are NOT.
        child.image_rendering = self.image_rendering;
        child
    }
}
