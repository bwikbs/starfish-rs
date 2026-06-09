//! `ComputedStyle` and its typed value enums. See M3 design §1.

pub use starfish_css::Rgba;

/// Computed length for box-model sizing/spacing. `em`/`rem` are resolved to
/// `Px` at compute time, so only these three variants survive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    /// `50%` → `Percent(50.0)`; resolved against the containing block in M4.
    Percent(f32),
    Auto,
    /// `calc()` reduced to its linear form `px + percent% * cb` (E13-M2). Only
    /// produced when both a px and a percent part are present; pure-px / pure-%
    /// calc() normalizes back to `Px`/`Percent` (so non-calc pages are unchanged).
    Calc { px: f32, percent: f32 },
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
        matches!(self, FlexDirection::RowReverse | FlexDirection::ColumnReverse)
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
}

impl WhiteSpace {
    /// Whitespace runs (incl. newlines, except a preserved `\n`) collapse to one
    /// space. True for normal / nowrap / pre-line.
    pub fn collapses(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine)
    }
    /// Segment breaks (`\n`) are preserved as forced line breaks. True for
    /// pre / pre-wrap / pre-line.
    pub fn preserves_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
    /// The line may wrap at soft break opportunities (spaces). True for
    /// normal / pre-wrap / pre-line.
    pub fn wraps(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
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

/// `background` (color or single linear-gradient image). Replaces the old
/// `background_color: Rgba` field. Initial = `Color(transparent)`. (E2-M5 §1.1)
#[derive(Debug, Clone, PartialEq)]
pub enum Background {
    Color(Rgba),
    Gradient(LinearGradient),
}

impl Background {
    /// The solid color, or `None` for a gradient.
    pub fn solid(&self) -> Option<Rgba> {
        match self {
            Background::Color(c) => Some(*c),
            _ => None,
        }
    }
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

/// Resolved, typed values for the layout-sufficient property subset (§1.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // box generation
    pub display: Display,

    // box dimensions
    pub width: Length,
    pub height: Length,

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

    // color / background
    pub color: Rgba,
    pub background: Background,

    // visual effects (E2-M5)
    /// Corner radii in px: TL, TR, BR, BL. All-zero = sharp corners.
    pub border_radius: [f32; 4],
    pub box_shadow: Option<BoxShadow>,
    /// 0..1; 1.0 = fully opaque (no offscreen layer).
    pub opacity: f32,

    // transforms (E5-M3) — paint-time only, NOT inherited.
    /// Empty = `none` (no transform, fast path).
    pub transform: Vec<TransformFn>,
    /// The pivot. Initial `(Percent(50), Percent(50))` = center.
    pub transform_origin: (LengthPct, LengthPct),

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
    pub font_family: Vec<String>,

    // bidi / spaced / transformed text (E6-M3)
    pub direction: Direction,
    pub unicode_bidi: UnicodeBidi,
    /// extra px after each char; inherited, initial 0.
    pub letter_spacing: f32,
    /// extra px at each U+0020 space; inherited, initial 0.
    pub word_spacing: f32,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,

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

    // grid container (E5-M1)
    pub grid_template_columns: Vec<TrackSize>,
    pub grid_template_rows: Vec<TrackSize>,

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
    pub(crate) custom_props: std::rc::Rc<std::collections::HashMap<String, Vec<starfish_css::Component>>>,
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
            background: Background::Color(TRANSPARENT),
            border_radius: [0.0; 4],
            box_shadow: None,
            opacity: 1.0,
            transform: Vec::new(),
            transform_origin: (LengthPct::Percent(50.0), LengthPct::Percent(50.0)),
            object_fit: ObjectFit::Fill,
            object_position: (LengthPct::Percent(50.0), LengthPct::Percent(50.0)),
            image_rendering: ImageRendering::Auto,
            font_size: 16.0,
            font_weight: FontWeight(400),
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            font_family: Vec::new(),
            direction: Direction::Ltr,
            unicode_bidi: UnicodeBidi::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            text_decoration_line: TextDecorationLine::NONE,
            list_style_type: ListStyleType::Disc, // CSS initial is `disc`
            list_style_position: ListStylePosition::Outside,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            overflow: Overflow::Visible,
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
            grid_template_columns: Vec::new(),
            grid_template_rows: Vec::new(),
            grid_column: GridLine::AUTO,
            grid_row: GridLine::AUTO,
            justify_items: AlignItems::Stretch,
            justify_self: AlignSelf::Auto,
            align_content: JustifyContent::FlexStart,
            grid_template_areas: Vec::new(),
            grid_area_name: None,
            content: Content::Normal,
            counter_reset: Vec::new(),
            counter_increment: Vec::new(),
            border_spacing: (0.0, 0.0),
            border_collapse: BorderCollapse::Separate,
            custom_props: std::rc::Rc::new(std::collections::HashMap::new()),
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
        child.font_family = self.font_family.clone();
        // E6-M3 inherited text props (unicode_bidi is NOT inherited).
        child.direction = self.direction;
        child.letter_spacing = self.letter_spacing;
        child.word_spacing = self.word_spacing;
        child.text_transform = self.text_transform;
        child.white_space = self.white_space;
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
