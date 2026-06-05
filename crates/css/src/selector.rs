//! Selector model: pure data (strings + counts), no DOM coupling.

/// One complex selector — a single entry of a comma-separated selector list.
#[derive(Debug)]
pub struct Selector {
    /// Compound selectors and the combinators between them, interleaved in
    /// source order. M3's matcher walks this right-to-left.
    pub parts: Vec<SelectorPart>,
    pub specificity: Specificity,
}

#[derive(Debug)]
pub enum SelectorPart {
    Compound(Compound),
    Combinator(Combinator),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace, e.g. `div p`.
    Descendant,
    /// `>` (stubbed — parsed and stored; M3 may treat like descendant).
    Child,
}

/// A compound selector: all simple selectors applying to the same element,
/// e.g. `div.item#main`.
#[derive(Debug)]
pub struct Compound {
    /// Type selector: `Some("div")`, or `None` when only `*`/class/id.
    pub tag: Option<String>,
    /// `*` present. Mutually exclusive with a tag in source.
    pub universal: bool,
    pub ids: Vec<String>,
    pub classes: Vec<String>,
}

impl Compound {
    fn new() -> Self {
        Compound {
            tag: None,
            universal: false,
            ids: Vec::new(),
            classes: Vec::new(),
        }
    }

    /// Whether anything was actually written into this compound.
    fn is_empty(&self) -> bool {
        self.tag.is_none() && !self.universal && self.ids.is_empty() && self.classes.is_empty()
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
    fn from_parts(parts: Vec<SelectorPart>) -> Self {
        let mut spec = Specificity { a: 0, b: 0, c: 0 };
        for p in &parts {
            if let SelectorPart::Compound(c) = p {
                spec.a += c.ids.len() as u32;
                spec.b += c.classes.len() as u32;
                spec.c += c.tag.is_some() as u32;
            }
        }
        Selector {
            parts,
            specificity: spec,
        }
    }
}

pub(crate) use builder::SelectorBuilder;

/// Selector-building state machine used by the parser. Kept here so the
/// `Compound`/`Selector` internals (`new`, `is_empty`, `from_parts`) stay
/// private to this module.
mod builder {
    use super::{Combinator, Compound, Selector, SelectorPart};

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

        /// Called before adding any simple selector: materialize a pending
        /// combinator (flushing the previous compound) so the new simple
        /// selector starts a fresh compound.
        fn before_simple(&mut self) {
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

        /// Buffer a combinator. Whitespace → Descendant; `>` → Child, which
        /// overrides an adjacent pending Descendant.
        pub(crate) fn push_combinator(&mut self, comb: Combinator) {
            match (&self.pending, &comb) {
                // whitespace then `>` (or `>` then whitespace): keep Child.
                (Some(Combinator::Child), Combinator::Descendant) => {}
                (Some(Combinator::Descendant), Combinator::Child) => {
                    self.pending = Some(Combinator::Child);
                }
                // `> >` etc. → invalid.
                (Some(Combinator::Child), Combinator::Child) => self.invalid = true,
                _ => self.pending = Some(comb),
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
            // A dangling explicit combinator (e.g. `div >`).
            if matches!(self.pending, Some(Combinator::Child)) && self.current.is_empty() {
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
