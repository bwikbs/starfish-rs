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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// Also covers `hidden`.
    None,
    /// The only painted line style; dashed/dotted/etc. fold to `Solid`.
    Solid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
}

/// `font-weight`, normalized to a numeric weight. `normal`→400, `bold`→700.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontWeight(pub u16);

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
    pub background_color: Rgba,

    // text / font
    pub font_size: f32,
    pub font_weight: FontWeight,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub font_family: Vec<String>,

    // text decoration (M1)
    pub text_decoration_line: TextDecorationLine,

    // list (M1)
    pub list_style_type: ListStyleType,
    pub list_style_position: ListStylePosition,

    // out-of-flow / positioning (M2)
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
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

    // gap (M3) — applies to flex containers
    pub row_gap: Length,
    pub column_gap: Length,
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
            font_size: 16.0,
            font_weight: FontWeight(400),
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            font_family: Vec::new(),
            text_decoration_line: TextDecorationLine::NONE,
            list_style_type: ListStyleType::Disc, // CSS initial is `disc`
            list_style_position: ListStylePosition::Outside,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
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
        child.line_height = self.line_height;
        child.text_align = self.text_align;
        child.font_family = self.font_family.clone();
        // list-style-* are inherited; text-decoration-line is NOT (§1.3).
        child.list_style_type = self.list_style_type;
        child.list_style_position = self.list_style_position;
        child
    }
}
