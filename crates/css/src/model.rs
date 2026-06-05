//! Stylesheet data model: the flat owned tree produced by the parser.

/// A parsed stylesheet: a flat list of qualified rules in source order.
#[derive(Debug)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// One qualified rule: a comma-separated selector list plus a `{ … }` block.
#[derive(Debug)]
pub struct Rule {
    pub selectors: Vec<crate::selector::Selector>,
    pub declarations: Vec<Declaration>,
}

/// A single `name: value` (possibly `!important`) declaration.
#[derive(Debug)]
pub struct Declaration {
    /// Property name, lowercased, e.g. `"color"`.
    pub name: String,
    pub value: Value,
    /// Trailing `!important`.
    pub important: bool,
}

/// A declaration value: the verbatim text plus a best-effort typed split.
#[derive(Debug)]
pub struct Value {
    /// The value verbatim — trimmed, comments stripped, `!important` removed.
    /// The source of truth.
    pub raw: String,
    /// Best-effort typed interpretation of `raw`. `Component::Raw` for anything
    /// not specialized. Never lossy: `raw` is always present.
    pub components: Vec<Component>,
}

/// A generic CSS component value.
#[derive(Debug, PartialEq)]
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
