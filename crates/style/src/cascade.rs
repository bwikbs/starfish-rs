//! The cascade (§4): collect matching declarations, order by precedence, apply.

use std::collections::HashMap;
use std::rc::Rc;

use starfish_css::{
    Compound, Declaration, PseudoClass, PseudoElement, Rule, Selector, SelectorPart, Specificity,
};
use starfish_dom::{Document, NodeId};

use crate::computed::{ComputedStyle, Content};
use crate::matching::matches;
use crate::properties::{apply_declaration, resolve_content, EmContext};

// Test-only counter: how many times the per-element match loop
// (`compute_matches`) actually ran. With caching ON, many elements share a
// result and never bump this — proving fewer full matches (E11-M2).
#[cfg(test)]
thread_local! {
    pub(crate) static CASCADE_MATCH_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    UserAgent,
    Author,
}

struct MatchedDecl<'a> {
    origin: Origin,
    /// Declaration came from the element's inline `style=""` attribute. Inline
    /// style outranks any author selector (normal declarations), so it sorts
    /// above non-inline author declarations regardless of specificity.
    inline: bool,
    /// Cascade-layer rank (E24-M2): `UNLAYERED` outside any `@layer`, else the
    /// layer's position in its sheet's declaration order. Among normal
    /// declarations larger wins; among `!important` the order inverts.
    layer: u32,
    specificity: Specificity,
    source_order: usize,
    declaration: &'a Declaration,
}

/// Sort key contribution of the layer rank (E24-M2): for normal declarations
/// later-declared layers (larger rank) win, and unlayered (MAX) beats all; for
/// `!important` the order INVERTS — earlier layers win, and important unlayered
/// (MAX → 0) loses to any important layered declaration.
fn layer_key(m: &MatchedDecl) -> u32 {
    if m.declaration.important {
        u32::MAX - m.layer
    } else {
        m.layer
    }
}

/// Origin × importance rank, low→high precedence (§4.3 step 1):
/// UA-normal < Author-normal < Author-important < UA-important.
fn origin_rank(origin: Origin, important: bool) -> u8 {
    match (origin, important) {
        (Origin::UserAgent, false) => 0,
        (Origin::Author, false) => 1,
        (Origin::Author, true) => 2,
        (Origin::UserAgent, true) => 3,
    }
}

/// Whether `pc` is a structural / positional pseudo-class whose match depends on
/// the element's position among siblings or in the tree — which the cache key
/// (own features only) cannot capture. `:not(simple)` is safe iff its inner
/// compound is itself position-independent (no structural pseudo inside).
fn pseudo_is_position_dependent(pc: &PseudoClass) -> bool {
    match pc {
        PseudoClass::FirstChild
        | PseudoClass::LastChild
        | PseudoClass::OnlyChild
        | PseudoClass::NthChild(_)
        | PseudoClass::NthOfType(_)
        | PseudoClass::Root
        | PseudoClass::Empty => true,
        // `:not(...)` wrapping a structural pseudo is also position-dependent.
        PseudoClass::Not(inner) => inner.pseudos.iter().any(pseudo_is_position_dependent),
        // `:has()` is ALWAYS position-dependent: its match reads the element's
        // subtree/siblings, which the own-feature cache key cannot capture.
        PseudoClass::Has(_) => true,
        // `:is()`/`:where()` are position-dependent iff any of their argument
        // selectors are (a combinator or a structural/`:has` pseudo inside).
        // `:is(.a, .b)` (plain compounds) stays position-INDEPENDENT (cache ON).
        PseudoClass::Is(list) | PseudoClass::Where(list) => {
            list.iter().any(selector_is_position_dependent)
        }
        // E14-M3 form-state pseudos read only the element's own attributes (the
        // cache key captures those via `collect_attr_names`), so position-
        // INDEPENDENT — the cache stays ON.
        PseudoClass::Checked
        | PseudoClass::Disabled
        | PseudoClass::Enabled
        | PseudoClass::Required
        | PseudoClass::ReadOnly
        | PseudoClass::ReadWrite
        // tag/id/class/attr and `:hover`-style NeverMatch are own-feature only.
        | PseudoClass::NeverMatch => false,
    }
}

/// Whether a complete (`:is`/`:where` argument) selector is position-dependent:
/// it contains a combinator, or any compound carries a position-dependent
/// pseudo-class. Used to keep the cache OFF only when an `:is()`/`:where()`
/// argument actually needs tree/sibling context (E16-M1).
fn selector_is_position_dependent(sel: &Selector) -> bool {
    sel.parts.iter().any(|p| match p {
        SelectorPart::Combinator(_) => true,
        SelectorPart::Compound(c) => c.pseudos.iter().any(pseudo_is_position_dependent),
    })
}

/// Collect every attribute name a compound's matching depends on — including
/// names referenced INSIDE `:not(...)` — into `names`. The `ElementKey` must
/// record these attrs' values, else two elements differing only in a
/// `:not([attr])`-referenced attribute would share a key and mis-share the rule
/// match. Mirrors the `:not` recursion in `pseudo_is_position_dependent`.
fn collect_attr_names(c: &Compound, names: &mut Vec<String>) {
    for a in &c.attrs {
        names.push(a.name.clone());
    }
    for p in &c.pseudos {
        match p {
            PseudoClass::Not(inner) => collect_attr_names(inner, names),
            // E14-M3 form-state pseudos read the element's own attributes (and the
            // input `type`); the key must capture them so two elements differing
            // only in e.g. `disabled` don't alias. (sort+dedup handles repeats.)
            PseudoClass::Checked => {
                names.push("checked".into());
                names.push("selected".into());
                names.push("type".into());
            }
            PseudoClass::Disabled | PseudoClass::Enabled => {
                names.push("disabled".into());
                names.push("type".into());
            }
            PseudoClass::Required => {
                names.push("required".into());
                names.push("type".into());
            }
            PseudoClass::ReadOnly | PseudoClass::ReadWrite => {
                names.push("readonly".into());
                names.push("type".into());
            }
            // `:is()`/`:where()` keep the cache ON when their arguments are
            // own-feature only, so their referenced attr names must be keyed too
            // (E16-M1). Recurse into each argument compound.
            PseudoClass::Is(list) | PseudoClass::Where(list) => {
                for s in list {
                    for part in &s.parts {
                        if let SelectorPart::Compound(c) = part {
                            collect_attr_names(c, names);
                        }
                    }
                }
            }
            // `:has()` disables the cache anyway, but collecting is harmless.
            PseudoClass::Has(list) => {
                for r in list {
                    for part in &r.selector.parts {
                        if let SelectorPart::Compound(c) = part {
                            collect_attr_names(c, names);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The cache is safe only when every selector across all sheets is a single
/// compound matching purely on the element's own (position-independent)
/// features — tag/id/class/attr. Any combinator (descendant/child/sibling) or
/// structural/positional pseudo-class makes a match position-dependent, so a
/// per-element key can no longer identify the match set: disable caching.
fn sheets_are_position_independent(sheets: &[(Origin, Vec<(&Rule, u32)>)]) -> bool {
    for (_, rules) in sheets {
        for (rule, _) in rules {
            for sel in &rule.selectors {
                for part in &sel.parts {
                    match part {
                        SelectorPart::Combinator(_) => return false,
                        SelectorPart::Compound(c) => {
                            if c.pseudos.iter().any(pseudo_is_position_dependent) {
                                return false;
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

/// Cache key for an element's selector-relevant features. Two elements with an
/// equal key are matched identically by every (position-independent) selector,
/// so they share a `compute_matches` result. `class` is the raw attribute string
/// (the matcher splits it the same way every time), and `attrs` carries this
/// element's value for each attr NAME any sheet selector references.
#[derive(PartialEq, Eq, Hash, Clone)]
struct ElementKey {
    tag: Option<String>,
    id: Option<String>,
    class: Option<String>,
    attrs: Vec<(String, Option<String>)>,
}

/// Per-`style_tree`-call memo: maps an `ElementKey` to the shared per-rule
/// match result (each rule's max matching specificity, or `None`). Stack-local
/// to one `style_tree` call, so it stays correct across getComputedStyle's
/// repeated calls and across multiple documents.
pub(crate) struct CascadeCache {
    enabled: bool,
    /// Attr NAMES referenced by any attribute selector, sorted+deduped.
    attr_names: Vec<String>,
    map: HashMap<ElementKey, Rc<Vec<Option<Specificity>>>>,
}

impl CascadeCache {
    pub(crate) fn new(sheets: &[(Origin, Vec<(&Rule, u32)>)]) -> Self {
        let enabled = sheets_are_position_independent(sheets);
        let mut attr_names: Vec<String> = Vec::new();
        for (_, rules) in sheets {
            for (rule, _) in rules {
                for sel in &rule.selectors {
                    for part in &sel.parts {
                        if let SelectorPart::Compound(c) = part {
                            collect_attr_names(c, &mut attr_names);
                        }
                    }
                }
            }
        }
        attr_names.sort();
        attr_names.dedup();
        CascadeCache {
            enabled,
            attr_names,
            map: HashMap::new(),
        }
    }

    /// Build the key for `element`, reading the same attributes the matcher does.
    fn key_for(&self, doc: &Document, element: NodeId) -> ElementKey {
        ElementKey {
            tag: doc.tag_name(element).map(str::to_string),
            id: doc.get_attribute(element, "id").map(str::to_string),
            class: doc.get_attribute(element, "class").map(str::to_string),
            attrs: self
                .attr_names
                .iter()
                .map(|n| (n.clone(), doc.get_attribute(element, n).map(str::to_string)))
                .collect(),
        }
    }
}

/// The per-element match loop, factored out so its result can be memoized. For
/// each (sheet, rule) in fixed precedence order, returns that rule's max
/// matching specificity, or `None` if no selector matched. Pseudo-element
/// selectors are skipped (cascaded separately by `cascade_pseudo`).
fn compute_matches(
    doc: &Document,
    element: NodeId,
    sheets: &[(Origin, Vec<(&Rule, u32)>)],
) -> Vec<Option<Specificity>> {
    #[cfg(test)]
    CASCADE_MATCH_CALLS.with(|c| c.set(c.get() + 1));

    let mut per_rule = Vec::new();
    for (_, rules) in sheets {
        for (rule, _) in rules {
            // Max specificity among this rule's matching selectors.
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                // A pseudo-element selector (`div::before`) applies to the
                // pseudo, not the element — those declarations are cascaded
                // separately by `cascade_pseudo`. Skip them here so they don't
                // leak onto the originating element.
                if sel.pseudo_element().is_none() && matches(doc, element, sel) {
                    best = Some(match best {
                        Some(b) => b.max(sel.specificity),
                        None => sel.specificity,
                    });
                }
            }
            per_rule.push(best);
        }
    }
    per_rule
}

/// Cascade matching declarations from `sheets` (each tagged with an origin, in
/// precedence-base order) onto `style`, seeded by inheritance/initial.
pub(crate) fn cascade(
    doc: &Document,
    element: NodeId,
    sheets: &[(Origin, Vec<(&Rule, u32)>)],
    ctx: EmContext,
    style: &mut ComputedStyle,
    cache: &mut CascadeCache,
) {
    // Per-rule match set (one entry per rule, in the same fixed (sheet, rule)
    // order `compute_matches` walks). Shared across elements with equal keys
    // when caching is enabled.
    let per_rule = if cache.enabled {
        let key = cache.key_for(doc, element);
        if let Some(hit) = cache.map.get(&key) {
            hit.clone()
        } else {
            let v = Rc::new(compute_matches(doc, element, sheets));
            cache.map.insert(key, v.clone());
            v
        }
    } else {
        Rc::new(compute_matches(doc, element, sheets))
    };

    let mut matched: Vec<MatchedDecl> = Vec::new();
    let mut source_order = 0usize;

    // Rebuild the `MatchedDecl` list from `per_rule` zipped with the rules in
    // the SAME order `compute_matches` used, preserving the per-rule
    // source_order increments so the downstream sort tiebreak is identical.
    let mut rule_idx = 0usize;
    for (origin, rules) in sheets {
        for (rule, rank) in rules {
            let spec = per_rule[rule_idx];
            rule_idx += 1;
            let Some(spec) = spec else { continue };
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: *origin,
                    inline: false,
                    layer: *rank,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // Inline `style=""` attribute: parsed as one Author rule whose declarations
    // outrank every author selector (normal declarations). Held in a local so
    // its declarations can be borrowed for the lifetime of the cascade. `!important`
    // inside the inline style is honored via `decl.important` + `origin_rank`.
    let inline_sheet = doc
        .get_attribute(element, "style")
        .filter(|s| !s.trim().is_empty())
        .map(|s| starfish_css::parse_stylesheet(&format!("*{{{s}}}")));
    if let Some(sheet) = &inline_sheet {
        if let Some(rule) = sheet.rules.first() {
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: Origin::Author,
                    inline: true,
                    layer: crate::UNLAYERED,
                    specificity: Specificity { a: 0, b: 0, c: 0 },
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // Ascending sort: applied first → last wins (stable preserves source order
    // already encoded, but key includes it explicitly). `inline` orders just
    // above non-inline within the same origin/importance rank.
    matched.sort_by_key(|m| {
        (
            origin_rank(m.origin, m.declaration.important),
            m.inline,
            layer_key(m),
            m.specificity,
            m.source_order,
        )
    });

    // Pass 0 (E13-M2): resolve custom properties (`--name`) first, in cascade
    // order, before any `var()` is consumed. Only rebuild the Rc map if at least
    // one custom prop is declared on this element; otherwise the inherited Rc is
    // shared unchanged (the common, byte-identical case).
    let mut custom_map: Option<HashMap<String, Vec<starfish_css::Component>>> = None;
    for m in matched
        .iter()
        .filter(|m| m.declaration.name.starts_with("--"))
    {
        let map = custom_map.get_or_insert_with(|| (*style.custom_props).clone());
        map.insert(
            m.declaration.name.clone(),
            m.declaration.value.components.clone(),
        );
    }
    if let Some(map) = custom_map {
        style.custom_props = Rc::new(map);
    }
    let custom = style.custom_props.clone();

    // Pass 1: apply font-size first so `em` on other lengths resolves against
    // this element's own computed font-size (§5.3). `--*` are excluded (already
    // handled in pass 0).
    let mut border_color_set = false;
    for m in matched.iter().filter(|m| m.declaration.name == "font-size") {
        apply_declaration(style, m.declaration, ctx, &custom);
    }
    for m in matched
        .iter()
        .filter(|m| m.declaration.name != "font-size" && !m.declaration.name.starts_with("--"))
    {
        if apply_declaration(style, m.declaration, ctx, &custom) {
            border_color_set = true;
        }
    }

    // currentColor: if no border color was explicitly given, it follows the
    // element's computed `color` (§1.3).
    if !border_color_set {
        style.border_color = style.color;
    }
}

/// Cascade the `::before`/`::after` pseudo-element of `element` for one `side`
/// (E7-M2). Only rules whose subject selector targets this pseudo-element side
/// (and that match `element`) are considered. The pseudo is a child of the
/// element, so it inherits from `element_style`. Returns `Some((style, text))`
/// iff a box-generating `content` (a `Text`, including `""`) was resolved;
/// `None` for no matching rule, no `content`, or `content:none/normal`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cascade_pseudo(
    doc: &Document,
    element: NodeId,
    side: PseudoElement,
    element_style: &ComputedStyle,
    sheets: &[(Origin, Vec<(&Rule, u32)>)],
    ctx: EmContext,
    counters: &crate::counters::CounterState,
) -> Option<(ComputedStyle, String)> {
    let mut matched: Vec<MatchedDecl> = Vec::new();
    let mut source_order = 0usize;

    for (origin, rules) in sheets {
        for (rule, rank) in rules {
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if sel.pseudo_element() == Some(side) && matches(doc, element, sel) {
                    best = Some(match best {
                        Some(b) => b.max(sel.specificity),
                        None => sel.specificity,
                    });
                }
            }
            let Some(spec) = best else { continue };
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: *origin,
                    inline: false,
                    layer: *rank,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }
    if matched.is_empty() {
        return None; // no ::side rule at all → no box.
    }

    // Base = inherit from the originating element (the pseudo is its child). The
    // pseudo's `em` basis is the element's font-size (the pseudo inherits it).
    let mut style = element_style.inherit_from();
    let pctx = EmContext {
        parent_font_size: element_style.font_size,
        root_font_size: ctx.root_font_size,
        viewport: ctx.viewport,
    };

    matched.sort_by_key(|m| {
        (
            origin_rank(m.origin, m.declaration.important),
            m.inline,
            layer_key(m),
            m.specificity,
            m.source_order,
        )
    });

    // Custom properties declared on the pseudo (E13-M2), resolved before any
    // `var()`. The pseudo inherits the element's custom props via `inherit_from`.
    let mut custom_map: Option<HashMap<String, Vec<starfish_css::Component>>> = None;
    for m in matched
        .iter()
        .filter(|m| m.declaration.name.starts_with("--"))
    {
        let map = custom_map.get_or_insert_with(|| (*style.custom_props).clone());
        map.insert(
            m.declaration.name.clone(),
            m.declaration.value.components.clone(),
        );
    }
    if let Some(map) = custom_map {
        style.custom_props = Rc::new(map);
    }
    let custom = style.custom_props.clone();

    // font-size first (so `em` resolves), then the rest; `content` resolved from
    // the winning (last) `content` declaration.
    let mut content = Content::Normal;
    let mut border_color_set = false;
    for m in matched.iter().filter(|m| m.declaration.name == "font-size") {
        apply_declaration(&mut style, m.declaration, pctx, &custom);
    }
    for m in matched
        .iter()
        .filter(|m| m.declaration.name != "font-size" && !m.declaration.name.starts_with("--"))
    {
        if m.declaration.name == "content" {
            content = resolve_content(doc, element, m.declaration, counters);
        } else if apply_declaration(&mut style, m.declaration, pctx, &custom) {
            border_color_set = true;
        }
    }
    if !border_color_set {
        style.border_color = style.color;
    }

    match content {
        Content::Text(s) => Some((style, s)),
        Content::None | Content::Normal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::Rgba;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;

    /// Tag each rule with the UNLAYERED rank (the tests don't use `@layer`).
    fn pairs(rules: &[Rule]) -> Vec<(&Rule, u32)> {
        rules.iter().map(|r| (r, crate::UNLAYERED)).collect()
    }

    /// UA-important outranks author-important (origin_rank 3 > 2): when both an
    /// UA and an author sheet set the same property `!important`, UA wins.
    #[test]
    fn ua_important_beats_author_important() {
        let doc = parse("<p>x</p>");
        let p = doc
            .children(doc.root())
            .into_iter()
            .find(|n| doc.tag_name(*n) == Some("p"))
            .or_else(|| {
                // Walk to find the <p> regardless of html/body wrapping.
                let mut stack = doc.children(doc.root());
                while let Some(n) = stack.pop() {
                    if doc.tag_name(n) == Some("p") {
                        return Some(n);
                    }
                    stack.extend(doc.children(n));
                }
                None
            })
            .expect("<p>");

        let ua = parse_stylesheet("p { color: #ff0000 !important }");
        let author = parse_stylesheet("p { color: #0000ff !important }");
        let sheets = [
            (Origin::UserAgent, pairs(&ua.rules)),
            (Origin::Author, pairs(&author.rules)),
        ];
        let ctx = EmContext {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport: crate::Viewport::from_width(800.0),
        };

        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(&doc, p, &sheets, ctx, &mut style, &mut cache);
        assert_eq!(
            style.color,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    /// A `::before` rule's declarations apply to the pseudo, NOT the element —
    /// the general cascade must skip pseudo-element selectors.
    #[test]
    fn pseudo_element_rule_does_not_leak_to_element() {
        let doc = parse("<p>x</p>");
        let p = find_p(&doc);
        let author =
            parse_stylesheet("p { color: #000000 } p::before { color: #ff0000; content: \"y\" }");
        let sheets = [(Origin::Author, pairs(&author.rules))];
        let ctx = EmContext {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport: crate::Viewport::from_width(800.0),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(&doc, p, &sheets, ctx, &mut style, &mut cache);
        // The element keeps black; the red is the pseudo's, not the element's.
        assert_eq!(
            style.color,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    fn find_p(doc: &Document) -> NodeId {
        let mut stack = doc.children(doc.root());
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some("p") {
                return n;
            }
            stack.extend(doc.children(n));
        }
        panic!("<p>");
    }

    /// Inline `style=""` declarations beat an author selector rule.
    #[test]
    fn inline_style_beats_author() {
        let doc = parse("<p style='color:#00ff00'>x</p>");
        let p = find_p(&doc);
        let author = parse_stylesheet("p { color: #ff0000 }");
        let sheets = [(Origin::Author, pairs(&author.rules))];
        let ctx = EmContext {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport: crate::Viewport::from_width(800.0),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(&doc, p, &sheets, ctx, &mut style, &mut cache);
        assert_eq!(
            style.color,
            Rgba {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }
        );
    }

    /// An author `!important` still beats a normal inline declaration.
    #[test]
    fn author_important_beats_inline_normal() {
        let doc = parse("<p style='color:#00ff00'>x</p>");
        let p = find_p(&doc);
        let author = parse_stylesheet("p { color: #ff0000 !important }");
        let sheets = [(Origin::Author, pairs(&author.rules))];
        let ctx = EmContext {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport: crate::Viewport::from_width(800.0),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(&doc, p, &sheets, ctx, &mut style, &mut cache);
        assert_eq!(
            style.color,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }
}
