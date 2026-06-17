//! Stylesheet data model: the flat owned tree produced by the parser.

/// A parsed stylesheet: a flat list of qualified rules in source order, plus
/// the captured `@font-face` rules (E6-M2). `rules` keeps its M2 semantics so
/// the cascade is unaffected; `font_faces` is consumed only by font loading.
#[derive(Debug)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// Captured `@font-face` rules in source order. Other at-rules are skipped.
    pub font_faces: Vec<FontFaceRule>,
    /// Captured `@media` blocks in source order (E13-M3). Each records the
    /// top-level rule index it appeared at, so the cascade can interleave its
    /// rules at the correct source-order position when the query matches.
    pub media_blocks: Vec<MediaBlock>,
    /// Captured `@keyframes` rules in source order (E17-M1). Consumed only by the
    /// animation pass; the cascade is unaffected. Empty on non-animated pages.
    pub keyframes: Vec<KeyframesRule>,
    /// Captured `@supports` blocks in source order (E24-M2).
    pub supports_blocks: Vec<SupportsBlock>,
    /// Captured named `@layer` blocks in source order (E24-M2). Anonymous,
    /// multi-name and dotted (nested) layer blocks are skipped (deferred).
    pub layer_blocks: Vec<LayerBlock>,
    /// Layer names in declaration order (first appearance wins), from both
    /// `@layer a, b;` ordering statements and `@layer name { … }` blocks.
    /// Later-declared layers have higher cascade priority.
    pub layer_order: Vec<String>,
    /// Captured `@container` blocks in source order (E25-M1). Evaluated per
    /// element against its nearest query container, so not flattened into the
    /// viewport-wide active rules like `@media`.
    pub container_blocks: Vec<ContainerBlock>,
    // E62-M1: captured `@scope (<root>) { … }` blocks in source order. Each rule
    // inside matches only when the element matches the inner selector AND is a
    // descendant-or-self of an element matching `root`. Empty on pages without
    // `@scope`, keeping the cascade byte-identical.
    pub scope_blocks: Vec<ScopeBlock>,
    /// Captured `@property` registrations in source order (E30-M1). The last
    /// registration for a given name wins.
    pub property_rules: Vec<PropertyRule>,
    // E42-M3: captured `@counter-style` rules in source order. The last rule
    // for a given name wins. Empty on pages without `@counter-style`.
    pub counter_style_rules: Vec<CounterStyleRule>,
}

/// A parsed `@supports` prelude condition (E24-M2). Anything not modelled
/// (functions like `selector()`, mixed `and`/`or` without parens, garbage)
/// becomes `Unknown`, which never matches.
#[derive(Debug)]
pub enum SupportsCondition {
    /// `( property : value )` — supported iff the engine accepts it.
    Decl(Declaration),
    Not(Box<SupportsCondition>),
    And(Vec<SupportsCondition>),
    Or(Vec<SupportsCondition>),
    Unknown,
}

/// A captured `@supports` block: its condition, the rules inside, the
/// top-level-rule source index at which it opened, and the at-block ordinal
/// (one counter across all captured at-blocks; merge tiebreaker).
#[derive(Debug)]
pub struct SupportsBlock {
    pub condition: SupportsCondition,
    pub rules: Vec<Rule>,
    pub source_index: usize,
    pub at_ordinal: usize,
}

/// A captured named `@layer name { … }` block (E24-M2).
#[derive(Debug)]
pub struct LayerBlock {
    pub name: String,
    pub rules: Vec<Rule>,
    pub source_index: usize,
    pub at_ordinal: usize,
}

/// A captured `@keyframes` rule (E17-M1): its name and its ordered keyframes. A
/// multi-selector block (`0%,50%{…}`) is expanded to one [`Keyframe`] per offset.
#[derive(Debug)]
pub struct KeyframesRule {
    pub name: String,
    pub keyframes: Vec<Keyframe>,
}

/// One keyframe of a `@keyframes` rule: a normalized offset in `0.0..=1.0`
/// (`from`→0, `to`→1, `50%`→0.5) and its declaration block.
#[derive(Debug)]
pub struct Keyframe {
    pub offset: f32,
    pub declarations: Vec<Declaration>,
}

/// A parsed `@media` prelude: a comma-separated list of conditions (OR). An
/// empty `conditions` never matches.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQuery {
    pub conditions: Vec<MediaCondition>,
}

/// One condition of a media query: an optional leading `not`, a media type, and
/// zero or more `and`-joined features.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaCondition {
    pub negated: bool,
    pub media_type: MediaType,
    pub features: Vec<MediaFeature>,
}

/// Media type. An unknown type is treated as `Print` (never matches screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaType {
    All,
    Screen,
    Print,
}

/// A single media feature. Anything we don't model (unknown name, non-px unit)
/// becomes `Unknown`, which never matches.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaFeature {
    MinWidth(f32),
    MaxWidth(f32),
    MinHeight(f32),
    MaxHeight(f32),
    Orientation(Orientation),
    /// Range-syntax width/height query (E27-M1): `(width >= 400px)`,
    /// `(400px <= width < 800px)`. Either bound may be absent (open range).
    Range(RangeFeature),
    // User-preference + interaction features (E27-M2).
    PrefersColorScheme(ColorScheme),
    /// `reduce` = `true`, `no-preference` = `false`.
    PrefersReducedMotion(bool),
    PrefersContrast(Contrast),
    Pointer(PointerKind),
    /// `hover` = `true`, `none` = `false`.
    Hover(bool),
    // Dimensional features (E27-M3). Aspect ratio is width/height; resolution is
    // in dppx (device pixels per CSS px).
    MinAspectRatio(f32),
    MaxAspectRatio(f32),
    MinResolution(f32),
    MaxResolution(f32),
    Unknown,
}

/// `prefers-color-scheme` / `color-scheme` value (E27-M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

/// `prefers-contrast` value (E27-M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contrast {
    NoPreference,
    More,
    Less,
}

/// `pointer` value (E27-M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    None,
    Coarse,
    Fine,
}

/// A normalized range-syntax media feature (E27-M1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeFeature {
    pub axis: RangeAxis,
    pub lower: Option<RangeBound>,
    pub upper: Option<RangeBound>,
}

/// One end of a [`RangeFeature`]: a px value and whether the bound is inclusive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeBound {
    pub value: f32,
    pub inclusive: bool,
}

/// The axis a [`RangeFeature`] queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAxis {
    Width,
    Height,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

/// A captured `@media` block: its prelude query, the rules inside the block, and
/// the top-level-rule source index at which it opened.
#[derive(Debug)]
pub struct MediaBlock {
    pub query: MediaQuery,
    pub rules: Vec<Rule>,
    pub source_index: usize,
    /// At-block ordinal (one counter across all captured at-blocks; E24-M2).
    pub at_ordinal: usize,
}

/// A captured `@container` block (E25-M1): an optional container name, a size
/// condition, the inner rules, and the source position (interleaving rules like
/// `@media`). Unlike `@media`, the condition is evaluated per element against
/// its nearest query container, not the viewport.
#[derive(Debug)]
pub struct ContainerBlock {
    pub name: Option<String>,
    pub condition: ContainerCondition,
    pub rules: Vec<Rule>,
    pub source_index: usize,
    pub at_ordinal: usize,
}

// E62-M1: a captured `@scope (<root>) { … }` block. `root` is the scope-root
// selector list from the prelude `( … )`; `rules` are the qualified rules inside.
// `source_index`/`at_ordinal` interleave its rules at the right cascade position
// (same mechanism as `@container`/`@media`).
#[derive(Debug)]
pub struct ScopeBlock {
    pub root: Vec<crate::selector::Selector>,
    pub rules: Vec<Rule>,
    pub source_index: usize,
    pub at_ordinal: usize,
}

/// A `@container` size condition (E25-M1). Mirrors the `@supports` `and`/`or`/
/// `not` shape; the leaf is a single size feature. Anything unrecognized →
/// `Unknown` (never matches).
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerCondition {
    And(Vec<ContainerCondition>),
    Or(Vec<ContainerCondition>),
    Not(Box<ContainerCondition>),
    Size(SizeFeature),
    Unknown,
}

/// One `@container` size feature, e.g. `(min-width: 400px)`. The value is in px
/// (non-px units make the whole feature `Unknown` at parse time).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeFeature {
    pub axis: SizeAxis,
    pub op: SizeOp,
    pub value: f32,
}

/// The queried axis. `Width`/`InlineSize` query the container's inline extent;
/// `Height`/`BlockSize` its block extent (horizontal-tb mapping for the MVP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeAxis {
    Width,
    Height,
    InlineSize,
    BlockSize,
}

/// Comparison direction for a [`SizeFeature`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeOp {
    Min,
    Max,
}

/// A captured `@property` registration (E30-M1): a typed custom property with a
/// declared syntax, inheritance flag, and initial value.
#[derive(Debug)]
pub struct PropertyRule {
    /// The custom property name, including the leading `--`.
    pub name: String,
    /// The `syntax` descriptor, unquoted (e.g. `<length>`, `<color>`, `*`).
    pub syntax: String,
    pub inherits: bool,
    /// The `initial-value` parsed into components (empty for `syntax: "*"`).
    pub initial: Vec<Component>,
}

// E42-M3: a captured `@counter-style <name> { … }` rule. Only the descriptors
// used by the list-marker formatter are kept (`range`/`pad`/`negative`/
// `speak-as`/`fallback`/`extends` are deferred).
#[derive(Debug, Clone)]
pub struct CounterStyleRule {
    /// The counter-style name (an ident, e.g. `box`), lowercased.
    pub name: String,
    /// The `system` descriptor, lowercased (`cyclic`/`fixed`/`symbolic`/
    /// `alphabetic`/`numeric`). Defaults to `symbolic` when unspecified.
    pub system: String,
    /// The `symbols` list (quotes stripped). Empty when only `additive-symbols`
    /// is given (not supported here) or none specified.
    pub symbols: Vec<String>,
    /// The `additive-symbols` list as `(weight, symbol)` pairs (captured but the
    /// formatter MVP does not consume it).
    pub additive_symbols: Vec<(i32, String)>,
    /// The `prefix` descriptor (quotes stripped). Default empty.
    pub prefix: String,
    /// The `suffix` descriptor (quotes stripped). `None` ⇒ use the UA default
    /// marker suffix (". ").
    pub suffix: Option<String>,
}

/// A captured `@font-face` rule (E6-M2). Only the descriptors used for loading
/// and matching are kept; unknown descriptors (unicode-range, font-display, …)
/// are ignored.
#[derive(Debug)]
pub struct FontFaceRule {
    /// The author-chosen family name (the `font-family` descriptor), unquoted.
    pub family: String,
    /// The `src` list in source order; the loader tries them front-to-back.
    pub src: Vec<FontSrc>,
    /// `font-weight` descriptor → numeric. `None` ⇒ default 400 at match time.
    pub weight: Option<u16>,
    /// `font-style` descriptor. `None` ⇒ default Normal at match time.
    pub style: Option<FontFaceStyle>,
}

/// One `src` entry of an `@font-face` rule.
#[derive(Debug, PartialEq)]
pub enum FontSrc {
    /// `url("…") format("…")` — the URL string + optional format hint
    /// (lowercased, e.g. "truetype", "woff2"); used to skip unsupported formats.
    Url { url: String, format: Option<String> },
    /// `local("…")` — a system face name to look up in the system DB.
    Local(String),
}

/// `font-style` as it can appear in an `@font-face` descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFaceStyle {
    Normal,
    Italic,
    Oblique,
}

/// One qualified rule: a comma-separated selector list plus a `{ … }` block.
#[derive(Debug)]
pub struct Rule {
    pub selectors: Vec<crate::selector::Selector>,
    pub declarations: Vec<Declaration>,
}

/// A single `name: value` (possibly `!important`) declaration.
#[derive(Debug, Clone)]
pub struct Declaration {
    /// Property name. Lowercased for normal properties (`"color"`); custom
    /// properties (`--*`) keep their original case (they are case-sensitive).
    pub name: String,
    pub value: Value,
    /// Trailing `!important`.
    pub important: bool,
}

/// A declaration value: the verbatim text plus a best-effort typed split.
#[derive(Debug, Clone)]
pub struct Value {
    /// The value verbatim — trimmed, comments stripped, `!important` removed.
    /// The source of truth.
    pub raw: String,
    /// Best-effort typed interpretation of `raw`. `Component::Raw` for anything
    /// not specialized. Never lossy: `raw` is always present.
    pub components: Vec<Component>,
}

/// A generic CSS component value.
#[derive(Debug, Clone, PartialEq)]
pub enum Component {
    /// Bare identifier / keyword: `block`, `auto`, `solid`.
    Keyword(String),
    /// `<number>` with no unit: `1.5`, `0`, `400`.
    Number(f32),
    /// `<length>`/`<percentage>` etc: value + unit. `50%` → unit `"%"`.
    Dimension { value: f32, unit: String },
    /// Color literal, pre-resolved to RGBA.
    Color(Rgba),
    /// A function kept but not specialized: `url(…)`, `calc(…)`.
    Function { name: String, raw_args: String },
    /// A top-level `,` (comma lists like font stacks).
    Comma,
    /// A quoted string: `"Helvetica Neue"`.
    Str(String),
    /// Unclassified token text — M3 can decide.
    Raw(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
