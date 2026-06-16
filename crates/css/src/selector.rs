//! Selector model: pure data (strings + counts), no DOM coupling.

/// One complex selector — a single entry of a comma-separated selector list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    /// Compound selectors and the combinators between them, interleaved in
    /// source order. M3's matcher walks this right-to-left.
    pub parts: Vec<SelectorPart>,
    pub specificity: Specificity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorPart {
    Compound(Compound),
    Combinator(Combinator),
}

/// A relative selector — a `:has()` argument: an optional leading combinator
/// relating the anchor element to the inner complex selector (E16-M1). An
/// absent combinator means Descendant (`:has(.x)` ≡ `:has(>>.x)` informally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeSelector {
    pub combinator: Combinator,
    pub selector: Selector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace, e.g. `div p`.
    Descendant,
    /// `>` direct child.
    Child,
    /// `+` adjacent sibling.
    NextSibling,
    /// `~` general (subsequent) sibling.
    SubsequentSibling,
}

/// A pseudo-element on a compound's subject (E7-M2). `::first-line` etc. are not
/// modeled — they invalidate the rule at parse time.
// E33-M3: `Slotted` carries an inner compound-selector list (`::slotted(span)`),
// so `PseudoElement` is no longer `Copy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    /// `::marker` (E35-M1): styles a list item's marker box.
    Marker,
    /// `::placeholder` (E35-M2): styles a form control's placeholder text.
    Placeholder,
    /// `::slotted(<compound-selector-list>)` (E33-M3): matches a distributed
    /// light child of the shadow host that matches one of the inner compounds.
    Slotted(Vec<Selector>),
}

/// A compound selector: all simple selectors applying to the same element,
/// e.g. `div.item#main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compound {
    /// Type selector: `Some("div")`, or `None` when only `*`/class/id.
    pub tag: Option<String>,
    /// `*` present. Mutually exclusive with a tag in source.
    pub universal: bool,
    pub ids: Vec<String>,
    pub classes: Vec<String>,
    /// Attribute selectors `[…]`.
    pub attrs: Vec<AttrSelector>,
    /// Structural / never-matching pseudo-classes.
    pub pseudos: Vec<PseudoClass>,
    /// `::before` / `::after` on this compound (E7-M2). At most one; only ever
    /// set on the rightmost compound (the parser rejects it elsewhere).
    pub pseudo_element: Option<PseudoElement>,
}

/// `[name op value flag]`. `value` is `None` only for `Exists` (`[name]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrSelector {
    /// Lowercased (HTML attrs are ASCII-case-insensitive).
    pub name: String,
    pub op: AttrOp,
    /// Raw value (NOT lowercased; `case_insensitive` governs compares).
    pub value: Option<String>,
    /// The trailing `i`/`I` flag.
    pub case_insensitive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp {
    /// `[a]`
    Exists,
    /// `[a=v]`
    Equals,
    /// `[a~=v]` whitespace-separated word match
    Includes,
    /// `[a|=v]` `v` or `v-…`
    DashMatch,
    /// `[a^=v]`
    Prefix,
    /// `[a$=v]`
    Suffix,
    /// `[a*=v]`
    Substring,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoClass {
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(Nth),
    NthOfType(Nth),
    // E29-M1: type-indexed + last structural pseudos.
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthLastChild(Nth),
    NthLastOfType(Nth),
    /// `:nth-child(An+B of S)` / `:nth-last-child(… of S)` (E29-M2): index only
    /// among siblings matching `of`. `from_end` for the last-* form.
    NthChildOf {
        nth: Nth,
        of: Vec<Selector>,
        from_end: bool,
    },
    Root,
    Empty,
    // E29-M3: link + UI pseudos.
    /// `:any-link`/`:link` — an `<a>`/`<area>` with an `href`.
    AnyLink,
    /// `:default` — a pre-checked checkbox/radio or a selected `<option>`.
    Default,
    /// `:placeholder-shown` — an empty `<input>`/`<textarea>` with `placeholder`.
    PlaceholderShown,
    /// `:scope` — the scoping root (the document element here).
    Scope,
    /// `:lang(xx)` — `lang` of the element/ancestor matches `xx` or `xx-*`.
    Lang(String),
    /// `:not(<single simple/compound selector>)`.
    Not(Box<Compound>),
    /// `:checked` — a checked checkbox/radio or a selected `<option>` (E14-M3).
    Checked,
    /// `:disabled` — a disabled form control / option / fieldset (E14-M3).
    Disabled,
    /// `:enabled` — a non-disabled form control / option (E14-M3).
    Enabled,
    /// `:required` — a form control with the `required` attribute (E14-M3).
    Required,
    /// `:read-only` — a text-editable control with `readonly` (E14-M3).
    ReadOnly,
    /// `:read-write` — a text-editable control without `readonly` (E14-M3).
    ReadWrite,
    /// `:is(<complex-selector-list>)` — matches if any listed selector matches;
    /// specificity is the max of the list. Forgiving parse (E16-M1).
    Is(Vec<Selector>),
    /// `:where(<complex-selector-list>)` — like `:is`, but zero specificity.
    Where(Vec<Selector>),
    /// `:has(<relative-selector-list>)` — matches if any relative selector,
    /// anchored at this element, matches a descendant/sibling.
    Has(Vec<RelativeSelector>),
    /// `:host` / `:host(<selector-list>)` (E33-M3): matches the shadow host from
    /// inside the shadow tree's scoped sheet. `None` → bare `:host`; `Some(list)`
    /// → matches only when the host matches one of the listed selectors. Matched
    /// specially in the cascade (host scope), never via the normal matcher.
    Host(Option<Vec<Selector>>),
    /// A recognized-but-never-matching pseudo (`:hover`, `:focus`, unknown
    /// `:foo`). Parsed so the rule survives, but never matches.
    NeverMatch,
}

/// `An+B`, e.g. `2n+1` → {a:2, b:1}; `odd` → {a:2, b:1}; `even` → {a:2, b:0};
/// `3` → {a:0, b:3}; `-n+3` → {a:-1, b:3}.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nth {
    pub a: i32,
    pub b: i32,
}

impl Compound {
    pub(crate) fn new() -> Self {
        Compound {
            tag: None,
            universal: false,
            ids: Vec::new(),
            classes: Vec::new(),
            attrs: Vec::new(),
            pseudos: Vec::new(),
            pseudo_element: None,
        }
    }

    /// Whether anything was actually written into this compound.
    pub(crate) fn is_empty(&self) -> bool {
        self.tag.is_none()
            && !self.universal
            && self.ids.is_empty()
            && self.classes.is_empty()
            && self.attrs.is_empty()
            && self.pseudos.is_empty()
            && self.pseudo_element.is_none()
    }
}

/// CSS specificity `(a, b, c)`. Derives `Ord` so M3 can compare
/// lexicographically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity {
    /// `#id` count.
    pub a: u32,
    /// `.class` count (+ attr + pseudo-class — none in M2).
    pub b: u32,
    /// type/tag count (+ pseudo-element — none in M2).
    pub c: u32,
}

impl Selector {
    /// The pseudo-element on the subject (rightmost) compound, if any (E7-M2).
    // E33-M3: returns a reference now that `PseudoElement` is no longer `Copy`.
    pub fn pseudo_element(&self) -> Option<&PseudoElement> {
        self.parts.iter().rev().find_map(|p| match p {
            SelectorPart::Compound(c) => Some(c.pseudo_element.as_ref()),
            SelectorPart::Combinator(_) => None,
        })?
    }

    fn from_parts(parts: Vec<SelectorPart>) -> Self {
        let mut spec = Specificity { a: 0, b: 0, c: 0 };
        for p in &parts {
            if let SelectorPart::Compound(c) = p {
                add_compound_specificity(&mut spec, c);
            }
        }
        Selector {
            parts,
            specificity: spec,
        }
    }
}

/// Sum a compound's specificity contribution (recursive for `:not`).
fn add_compound_specificity(spec: &mut Specificity, c: &Compound) {
    spec.a += c.ids.len() as u32;
    spec.b += c.classes.len() as u32;
    spec.c += c.tag.is_some() as u32;
    // Attribute selectors are class-level.
    spec.b += c.attrs.len() as u32;
    for p in &c.pseudos {
        match p {
            // `:not` itself adds nothing; fold in its argument's specificity.
            PseudoClass::Not(inner) => add_compound_specificity(spec, inner),
            // `:is()` contributes the max specificity among its arguments; the
            // `:is()` itself adds nothing. Empty list → adds nothing.
            PseudoClass::Is(list) => {
                if let Some(max) = list.iter().map(|s| s.specificity).max() {
                    spec.a += max.a;
                    spec.b += max.b;
                    spec.c += max.c;
                }
            }
            // `:where()` always contributes zero specificity.
            PseudoClass::Where(_) => {}
            // `:has()` contributes the max specificity among its arguments.
            PseudoClass::Has(list) => {
                if let Some(max) = list.iter().map(|r| r.selector.specificity).max() {
                    spec.a += max.a;
                    spec.b += max.b;
                    spec.c += max.c;
                }
            }
            // E33-M3: `:host` is a pseudo-class (b += 1); `:host(sel)` adds the
            // max specificity of its inner list on top (mirrors `:is`).
            PseudoClass::Host(opt) => {
                spec.b += 1;
                if let Some(list) = opt {
                    if let Some(max) = list.iter().map(|s| s.specificity).max() {
                        spec.a += max.a;
                        spec.b += max.b;
                        spec.c += max.c;
                    }
                }
            }
            // Every other pseudo-class (incl. NeverMatch) is class-level.
            _ => spec.b += 1,
        }
    }
    // A pseudo-element contributes element-level (0,0,1) (CSS Selectors §16).
    // E33-M3: `::slotted(list)` adds the max specificity of its inner compound
    // list on top of the element-level contribution (mirrors `:is`).
    match &c.pseudo_element {
        Some(PseudoElement::Slotted(list)) => {
            spec.c += 1;
            if let Some(max) = list.iter().map(|s| s.specificity).max() {
                spec.a += max.a;
                spec.b += max.b;
                spec.c += max.c;
            }
        }
        Some(_) => spec.c += 1,
        None => {}
    }
}

pub(crate) use builder::SelectorBuilder;

/// Selector-building state machine used by the parser. Kept here so the
/// `Compound`/`Selector` internals (`new`, `is_empty`, `from_parts`) stay
/// private to this module.
mod builder {
    use super::{
        AttrSelector, Combinator, Compound, PseudoClass, PseudoElement, Selector, SelectorPart,
    };

    /// Accumulates one complex selector. The parser feeds simple selectors and
    /// combinators; `finish` validates and produces a `Selector`, or `None` if
    /// the selector was malformed/empty.
    ///
    /// Combinators are *pending*: whitespace sets a pending `Descendant`, an
    /// explicit `>` sets/upgrades a pending `Child` (absorbing adjacent
    /// whitespace). The pending combinator materializes only when the next
    /// simple selector arrives — so a trailing combinator simply never emits.
    pub(crate) struct SelectorBuilder {
        parts: Vec<SelectorPart>,
        current: Compound,
        /// Combinator buffered after the current compound was flushed, awaiting
        /// the next simple selector.
        pending: Option<Combinator>,
        invalid: bool,
    }

    impl SelectorBuilder {
        pub(crate) fn new() -> Self {
            SelectorBuilder {
                parts: Vec::new(),
                current: Compound::new(),
                pending: None,
                invalid: false,
            }
        }

        pub(crate) fn invalidate(&mut self) {
            self.invalid = true;
        }

        pub(crate) fn set_tag(&mut self, name: String) {
            self.before_simple();
            // A second tag in one compound is invalid.
            if self.current.tag.is_some() {
                self.invalid = true;
            }
            self.current.tag = Some(name);
        }

        pub(crate) fn set_universal(&mut self) {
            self.before_simple();
            self.current.universal = true;
        }

        pub(crate) fn push_id(&mut self, id: String) {
            self.before_simple();
            self.current.ids.push(id);
        }

        pub(crate) fn push_class(&mut self, class: String) {
            self.before_simple();
            self.current.classes.push(class);
        }

        pub(crate) fn push_attr(&mut self, attr: AttrSelector) {
            self.before_simple();
            self.current.attrs.push(attr);
        }

        pub(crate) fn push_pseudo(&mut self, pseudo: PseudoClass) {
            self.before_simple();
            self.current.pseudos.push(pseudo);
        }

        pub(crate) fn set_pseudo_element(&mut self, pe: PseudoElement) {
            self.before_simple();
            // A second pseudo-element on one compound is invalid. (Any simple
            // selector or combinator appearing AFTER it is caught by the
            // `pseudo_element.is_some()` guard in `before_simple`.)
            if self.current.pseudo_element.is_some() {
                self.invalid = true;
            }
            self.current.pseudo_element = Some(pe);
        }

        /// Called before adding any simple selector: materialize a pending
        /// combinator (flushing the previous compound) so the new simple
        /// selector starts a fresh compound.
        fn before_simple(&mut self) {
            // A pseudo-element terminates the selector: nothing may follow it
            // (no further simple selector, no combinator). E7-M2.
            if self.current.pseudo_element.is_some() {
                self.invalid = true;
            }
            if let Some(comb) = self.pending.take() {
                if self.current.is_empty() {
                    // combinator with no left-hand compound → invalid.
                    self.invalid = true;
                    return;
                }
                self.flush_current();
                self.parts.push(SelectorPart::Combinator(comb));
            }
        }

        /// Buffer a combinator. Whitespace → Descendant; an explicit combinator
        /// (`>`/`+`/`~`) overrides an adjacent pending Descendant. Two explicit
        /// combinators in a row → invalid.
        pub(crate) fn push_combinator(&mut self, comb: Combinator) {
            let new_explicit = !matches!(comb, Combinator::Descendant);
            match &self.pending {
                None => self.pending = Some(comb),
                Some(Combinator::Descendant) => {
                    // explicit overrides a pending Descendant; Descendant after
                    // Descendant is a no-op.
                    if new_explicit {
                        self.pending = Some(comb);
                    }
                }
                // pending is explicit:
                Some(_) => {
                    if new_explicit {
                        // explicit then explicit (`> >`, `+ +`, …) → invalid.
                        self.invalid = true;
                    }
                    // explicit then whitespace: keep the explicit.
                }
            }
        }

        fn flush_current(&mut self) {
            let done = std::mem::replace(&mut self.current, Compound::new());
            self.parts.push(SelectorPart::Compound(done));
        }

        /// Finalize. `None` when the selector is invalid or empty.
        pub(crate) fn finish(mut self) -> Option<Selector> {
            if self.invalid {
                return None;
            }
            // A trailing explicit combinator with no right-hand compound
            // (e.g. `div >`, `div +`) is malformed. The combinator is still
            // pending (never materialized), and `current` holds the left-hand
            // compound — so a non-empty `current` here means the combinator is
            // dangling.
            if matches!(self.pending, Some(ref c) if !matches!(c, Combinator::Descendant)) {
                return None;
            }
            if !self.current.is_empty() {
                self.flush_current();
            }
            if self.parts.is_empty() {
                return None;
            }
            Some(Selector::from_parts(self.parts))
        }
    }
}
