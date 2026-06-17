//! The cascade (§4): collect matching declarations, order by precedence, apply.

use std::collections::HashMap;
use std::rc::Rc;

use starfish_css::{
    Compound, ContainerBlock, Declaration, PseudoClass, PseudoElement, Rule, ScopeBlock, Selector,
    SelectorPart, Specificity,
};
use starfish_dom::{Document, NodeId};

use crate::computed::{ComputedStyle, Content};
use crate::matching::matches;
use crate::properties::{apply_declaration, resolve_content, substitute_attr_decl, EmContext};

/// The query-container context for one element's cascade (E25-M1): the global
/// list of `@container` blocks plus this element's nearest container size/name.
/// `blocks` empty (or no scope) ⇒ no container rules apply — the byte-identical
/// path for pages without `@container`.
#[derive(Clone, Copy)]
pub(crate) struct ContainerEnv<'a> {
    pub blocks: &'a [(Origin, &'a ContainerBlock)],
    pub inline: f32,
    pub block: f32,
    pub name: Option<&'a str>,
}

impl ContainerEnv<'_> {
    /// An empty environment (no container blocks): used by pseudo cascades and
    /// the container-unaware first pass.
    pub(crate) fn none() -> Self {
        ContainerEnv {
            blocks: &[],
            inline: 0.0,
            block: 0.0,
            name: None,
        }
    }
}

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
        | PseudoClass::FirstOfType
        | PseudoClass::LastOfType
        | PseudoClass::OnlyOfType
        | PseudoClass::NthLastChild(_)
        | PseudoClass::NthLastOfType(_)
        | PseudoClass::NthChildOf { .. }
        | PseudoClass::Root
        | PseudoClass::Empty
        // E29-M3: ancestor-/attribute-dependent → keep the cache off (safe).
        | PseudoClass::AnyLink
        | PseudoClass::Default
        | PseudoClass::PlaceholderShown
        | PseudoClass::Scope
        | PseudoClass::Lang(_) => true,
        // E36-M3: `:popover-open` reads a Document-side flag, not an attribute,
        // so `collect_attr_names` cannot key it. It IS own-feature only (depends
        // only on `el`'s own open state), so it stays position-INDEPENDENT (cache
        // ON); the `ElementKey` captures the open flag instead (see `key_for`) so
        // open/closed `[popover]` elements never alias.
        PseudoClass::PopoverOpen => false,
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
        // E39-M3 validity pseudos read only the element's own attributes
        // (value/required/min/max/type/pattern), captured via collect_attr_names,
        // so position-INDEPENDENT — the cache stays ON.
        | PseudoClass::Valid
        | PseudoClass::Invalid
        | PseudoClass::InRange
        | PseudoClass::OutOfRange
        | PseudoClass::Optional
        // E33-M3: `:host` never matches via the ordinary matcher (always false),
        // so it is position-INDEPENDENT for caching purposes.
        | PseudoClass::Host(_)
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
            // E39-M3 validity pseudos read value/required/min/max/type/pattern; the
            // key must capture them so two controls differing only in e.g.
            // `required`/`value` don't alias in the CascadeCache.
            PseudoClass::Valid
            | PseudoClass::Invalid
            | PseudoClass::InRange
            | PseudoClass::OutOfRange
            | PseudoClass::Optional => {
                names.push("value".into());
                names.push("required".into());
                names.push("min".into());
                names.push("max".into());
                names.push("type".into());
                names.push("pattern".into());
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
    /// E36-M3: the element's `:popover-open` state. Always `false` unless a sheet
    /// actually references `:popover-open` (else it stays out of the picture so
    /// non-popover pages key identically to before). Captured here — not via
    /// `attrs` — because the open flag lives on the `Document`, not an attribute.
    popover_open: bool,
}

/// E36-M3: whether any sheet selector references `:popover-open` (recursing into
/// `:not`/`:is`/`:where`/`:has`). When false, the cache key omits the open flag
/// so non-popover pages key exactly as before.
fn sheets_use_popover_open(sheets: &[(Origin, Vec<(&Rule, u32)>)]) -> bool {
    fn compound_has(c: &Compound) -> bool {
        c.pseudos.iter().any(pseudo_has)
    }
    fn pseudo_has(p: &PseudoClass) -> bool {
        match p {
            PseudoClass::PopoverOpen => true,
            PseudoClass::Not(inner) => compound_has(inner),
            PseudoClass::Is(list) | PseudoClass::Where(list) => {
                list.iter().any(selector_has)
            }
            PseudoClass::Has(list) => list.iter().any(|r| selector_has(&r.selector)),
            _ => false,
        }
    }
    fn selector_has(sel: &Selector) -> bool {
        sel.parts.iter().any(|p| match p {
            SelectorPart::Compound(c) => compound_has(c),
            SelectorPart::Combinator(_) => false,
        })
    }
    sheets
        .iter()
        .any(|(_, rules)| rules.iter().any(|(r, _)| r.selectors.iter().any(selector_has)))
}

/// Per-`style_tree`-call memo: maps an `ElementKey` to the shared per-rule
/// match result (each rule's max matching specificity, or `None`). Stack-local
/// to one `style_tree` call, so it stays correct across getComputedStyle's
/// repeated calls and across multiple documents.
pub(crate) struct CascadeCache {
    enabled: bool,
    /// Attr NAMES referenced by any attribute selector, sorted+deduped.
    attr_names: Vec<String>,
    /// E36-M3: whether any sheet references `:popover-open` (gates the key flag).
    uses_popover_open: bool,
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
            uses_popover_open: sheets_use_popover_open(sheets),
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
            popover_open: self.uses_popover_open && doc.is_popover_open(element),
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
// E33-M3: `host_rules`/`slotted_rules` are extra Author rules injected on top
// (like `@container`), used to apply a shadow scope's `:host`/`::slotted` rules
// onto a host / distributed light child. Both empty on the default (non-shadow)
// path → no effect → byte-identical.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cascade(
    doc: &Document,
    element: NodeId,
    sheets: &[(Origin, Vec<(&Rule, u32)>)],
    ctx: EmContext<'_>,
    style: &mut ComputedStyle,
    cache: &mut CascadeCache,
    containers: ContainerEnv,
    host_rules: &[&Rule],
    slotted_rules: &[&Rule],
    // E62-M1: `@scope` blocks (UA + author) in precedence order. Empty on pages
    // without `@scope` → the loop below is skipped (byte-identical path).
    scope_blocks: &[(Origin, &ScopeBlock)],
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

    // @container blocks (E25-M1): for each block whose name (if any) matches the
    // element's nearest query container and whose size condition holds against
    // that container, match its inner rules against this element and append
    // their declarations. Appended after source-order rules (and inline), so a
    // matching container rule wins ties — the intended effect. Skipped entirely
    // when there are no blocks (byte-identical path).
    for (origin, cb) in containers.blocks {
        let name_ok = cb
            .name
            .as_deref()
            .is_none_or(|n| containers.name == Some(n));
        if !name_ok
            || !crate::container::container_matches(
                &cb.condition,
                containers.inline,
                containers.block,
            )
        {
            continue;
        }
        for rule in &cb.rules {
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if sel.pseudo_element().is_none() && matches(doc, element, sel) {
                    best =
                        Some(best.map_or(sel.specificity, |b: Specificity| b.max(sel.specificity)));
                }
            }
            let Some(spec) = best else { continue };
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: *origin,
                    inline: false,
                    layer: crate::UNLAYERED,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // E62-M1: `@scope (<root>) { … }` blocks. For each block, a rule applies to
    // this element iff the element matches the inner selector AND is in scope
    // (a descendant-or-self of an element matching the scope root). The scope
    // root adds NO specificity (MVP): scoping is a match filter. Appended after
    // source-order rules so a matching scoped rule wins ties at equal
    // specificity (it appeared later in source order). Skipped entirely when
    // there are no blocks (byte-identical path).
    for (origin, sb) in scope_blocks {
        if !element_in_scope(doc, element, &sb.root, &sb.limit) {
            continue;
        }
        for rule in &sb.rules {
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if sel.pseudo_element().is_none()
                    && scope_rule_matches(doc, element, sel, &sb.root)
                {
                    best =
                        Some(best.map_or(sel.specificity, |b: Specificity| b.max(sel.specificity)));
                }
            }
            let Some(spec) = best else { continue };
            for decl in &rule.declarations {
                matched.push(MatchedDecl {
                    origin: *origin,
                    inline: false,
                    layer: crate::UNLAYERED,
                    specificity: spec,
                    source_order,
                    declaration: decl,
                });
                source_order += 1;
            }
        }
    }

    // E33-M3: inject the shadow scope's `:host` / `::slotted` declarations,
    // mirroring the `@container` path above. Appended as Author declarations
    // sorted by their selector's specificity / source order. Both slices are
    // empty on the non-shadow path, so this is skipped (byte-identical).
    for rule in host_rules {
        let mut best: Option<Specificity> = None;
        for sel in &rule.selectors {
            if let Some(spec) = host_selector_spec(doc, element, sel) {
                best = Some(best.map_or(spec, |b: Specificity| b.max(spec)));
            }
        }
        let Some(spec) = best else { continue };
        for decl in &rule.declarations {
            matched.push(MatchedDecl {
                origin: Origin::Author,
                inline: false,
                layer: crate::UNLAYERED,
                specificity: spec,
                source_order,
                declaration: decl,
            });
            source_order += 1;
        }
    }
    for rule in slotted_rules {
        let mut best: Option<Specificity> = None;
        for sel in &rule.selectors {
            if let Some(spec) = slotted_selector_spec(doc, element, sel) {
                best = Some(best.map_or(spec, |b: Specificity| b.max(spec)));
            }
        }
        let Some(spec) = best else { continue };
        for decl in &rule.declarations {
            matched.push(MatchedDecl {
                origin: Origin::Author,
                inline: false,
                layer: crate::UNLAYERED,
                specificity: spec,
                source_order,
                declaration: decl,
            });
            source_order += 1;
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
        // E24-M3: expand `attr()` against this element before applying.
        let sub = substitute_attr_decl(m.declaration, doc, element);
        let decl = sub.as_ref().unwrap_or(m.declaration);
        if apply_declaration(style, decl, ctx, &custom) {
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
    ctx: EmContext<'_>,
    counters: &mut crate::counters::CounterState,
    // E53-M3: boxed return so the large `(ComputedStyle, String)` result lives on
    // the heap, not in the recursive caller's (`style_node`) stack frame.
) -> Option<Box<(ComputedStyle, String)>> {
    let mut matched: Vec<MatchedDecl> = Vec::new();
    let mut source_order = 0usize;

    for (origin, rules) in sheets {
        for (rule, rank) in rules {
            let mut best: Option<Specificity> = None;
            for sel in &rule.selectors {
                if sel.pseudo_element() == Some(&side) && matches(doc, element, sel) {
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
        counter_styles: ctx.counter_styles, // E42-M3
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
        } else {
            let sub = substitute_attr_decl(m.declaration, doc, element);
            let decl = sub.as_ref().unwrap_or(m.declaration);
            if apply_declaration(&mut style, decl, pctx, &custom) {
                border_color_set = true;
            }
        }
    }
    if !border_color_set {
        style.border_color = style.color;
    }

    // E53-M3: a quote `content` keyword resolves to its mark here, using + updating
    // the live quote depth. The pair set is the originating element's `quotes`
    // (the UA default when `auto`/unset). The resolved string replaces the quote
    // variant with `Content::Text` so layout treats it as ordinary text.
    if let Content::OpenQuote | Content::CloseQuote | Content::NoOpenQuote
    | Content::NoCloseQuote = content
    {
        let op = match content {
            Content::OpenQuote => crate::counters::QuoteOp::Open,
            Content::CloseQuote => crate::counters::QuoteOp::Close,
            Content::NoOpenQuote => crate::counters::QuoteOp::NoOpen,
            _ => crate::counters::QuoteOp::NoClose,
        };
        let pairs: &[(String, String)] = match element_style.quotes.as_deref() {
            Some(p) => p.as_slice(),
            None => crate::counters::UA_DEFAULT_QUOTES,
        };
        let mark = counters.apply_quote(op, pairs);
        content = Content::Text(mark);
    }

    // E53-M2: carry the resolved `content` on the pseudo's style so the box tree
    // can tell a `url(...)` image pseudo apart from a text pseudo.
    style.content = content.clone();
    match content {
        Content::Text(s) => Some(Box::new((style, s))),
        // E53-M2: `content: url(...)` → keep the url in the content string so the
        // box tree can build an image replaced box (style.content marks it a Url).
        Content::Url(src) => Some(Box::new((style, src))),
        // E35-M1: `::marker` produces a styled marker box even without an explicit
        // `content` (color/font-only rules); the empty string means "keep the
        // default bullet/ordinal text". For ::before/::after, no `content` →
        // no generated box.
        // E35-M2: `::placeholder` likewise keeps a style-only rule (its text comes
        // from the control's `placeholder` attribute, not `content`).
        // E35-M3: `::first-letter` likewise keeps a style-only rule; the styled
        // letter comes from the block's first text run, not `content`.
        Content::None | Content::Normal
            if side == PseudoElement::Marker
                || side == PseudoElement::Placeholder
                || side == PseudoElement::FirstLetter =>
        {
            Some(Box::new((style, String::new())))
        }
        Content::None | Content::Normal => None,
        // E53-M3: quote variants were already resolved to `Content::Text` above.
        Content::OpenQuote | Content::CloseQuote | Content::NoOpenQuote
        | Content::NoCloseQuote => unreachable!("quote content resolved to Text above"),
    }
}

// E62-M1: `@scope` membership. An element is in scope iff it, or one of its
// ancestors, matches any of the scope-root selectors (descendant-or-self of a
// root). Walks up the parent chain from `element` (inclusive) to the document
// root.
//
// E62-M2: `to (<limit>)` bounds the scope (a "donut"). The limit is exclusive:
// an element matching `<limit>`, and everything below it, is OUT of scope. The
// element is in scope iff there is an ancestor-or-self `R` matching `root` such
// that, on the path `element → R` (excluding `R`), no node (including `element`
// itself) matches `limit`. `limit` empty = no boundary (M1 behavior).
fn element_in_scope(
    doc: &Document,
    element: NodeId,
    root: &[Selector],
    limit: &[Selector],
) -> bool {
    let mut cur = Some(element);
    while let Some(node) = cur {
        if doc.tag_name(node).is_some() {
            // Reaching a root match: in scope, since no limit was crossed on the
            // way up (we'd have returned false already). The root itself is never
            // treated as a limit boundary.
            if root.iter().any(|s| matches(doc, node, s)) {
                return true;
            }
            // A non-root node on the path matching the limit cuts off the scope
            // (the limit element and its descendants are excluded).
            if !limit.is_empty() && limit.iter().any(|s| matches(doc, node, s)) {
                return false;
            }
        }
        cur = doc.parent(node);
    }
    false
}

// E62-M3: match a `@scope` block's inner rule selector against `element`, with
// `:scope` / `&` (both modeled as `PseudoClass::Scope`) resolving to the scope
// root rather than the document root. The element is already known to be in
// scope (the caller checked `element_in_scope`). Three cases:
//   * The selector references no `:scope`/`&` → ordinary match.
//   * The selector IS exactly `:scope` (a single bare-`Scope` compound) → it
//     targets the scope root itself: matches iff `element` matches the scope
//     `root` selector (i.e. it is a scope-root element, and — being in scope —
//     scoped under itself).
//   * The selector's LEADING compound references `:scope`/`&` (e.g. `:scope .x`,
//     `& p`): MVP simplification — the `:scope`/`&` part is treated as
//     satisfied-by-scope (E62-M1 already restricts matching to scoped
//     descendants), so we strip the leading `:scope` compound + its combinator
//     and match the remaining sub-selector against `element`. A `:scope`/`&`
//     that appears anywhere else falls back to the ordinary matcher (where
//     `Scope` matches the document root).
fn scope_rule_matches(
    doc: &Document,
    element: NodeId,
    sel: &Selector,
    root: &[Selector],
) -> bool {
    // Index of the leftmost compound (selectors always start with a compound).
    let Some(SelectorPart::Compound(first)) = sel.parts.first() else {
        return matches(doc, element, sel);
    };
    if !compound_is_bare_scope(first) {
        return matches(doc, element, sel);
    }
    // The leading compound is exactly `:scope` / `&`.
    if sel.parts.len() == 1 {
        // `:scope { … }` — target the scope root itself.
        return root.iter().any(|s| matches(doc, element, s));
    }
    // `:scope <combinator> <rest>` — strip the leading `:scope` + combinator and
    // match the remainder; scope membership already pins it under the root.
    let rest = Selector {
        parts: sel.parts[2..].to_vec(),
        specificity: sel.specificity,
    };
    matches(doc, element, &rest)
}

// E62-M3: whether a compound is exactly a single `:scope` / `&` (no tag, class,
// id, attr, other pseudo, or pseudo-element).
fn compound_is_bare_scope(c: &Compound) -> bool {
    c.tag.is_none()
        && !c.universal
        && c.ids.is_empty()
        && c.classes.is_empty()
        && c.attrs.is_empty()
        && c.pseudo_element.is_none()
        && matches!(c.pseudos.as_slice(), [PseudoClass::Scope])
}

// E33-M3: `:host` candidate match. `sel` must be a single compound whose
// `pseudos` contain `:host`; returns its specificity when it matches `element`
// (the shadow host). `:host` → always matches; `:host(list)` → matches iff
// `element` matches any selector in `list`.
fn host_selector_spec(doc: &Document, element: NodeId, sel: &Selector) -> Option<Specificity> {
    // Single compound only (no combinators).
    let compound = match sel.parts.as_slice() {
        [SelectorPart::Compound(c)] => c,
        _ => return None,
    };
    let host = compound
        .pseudos
        .iter()
        .find_map(|p| match p {
            PseudoClass::Host(opt) => Some(opt),
            _ => None,
        })?;
    match host {
        None => Some(sel.specificity),
        Some(list) => list
            .iter()
            .any(|s| matches(doc, element, s))
            .then_some(sel.specificity),
    }
}

// E33-M3: `::slotted(list)` candidate match. The selector's pseudo-element must
// be `Slotted(list)`; returns its specificity when `element` (a distributed
// light child) matches one of the inner compound selectors. MVP: the inner
// compound list only (no descendant combinator before `::slotted`).
fn slotted_selector_spec(doc: &Document, element: NodeId, sel: &Selector) -> Option<Specificity> {
    let list = match sel.pseudo_element() {
        Some(PseudoElement::Slotted(list)) => list,
        _ => return None,
    };
    list.iter()
        .any(|s| matches(doc, element, s))
        .then_some(sel.specificity)
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

    /// E42-M3: an empty `@counter-style` map for `EmContext` in tests.
    fn no_counter_styles() -> &'static HashMap<String, crate::CounterStyleData> {
        use std::sync::OnceLock;
        static M: OnceLock<HashMap<String, crate::CounterStyleData>> = OnceLock::new();
        M.get_or_init(HashMap::new)
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
            counter_styles: no_counter_styles(),
        };

        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(
            &doc,
            p,
            &sheets,
            ctx,
            &mut style,
            &mut cache,
            ContainerEnv::none(),
            &[],
            &[],
            &[],
        );
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
            counter_styles: no_counter_styles(),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(
            &doc,
            p,
            &sheets,
            ctx,
            &mut style,
            &mut cache,
            ContainerEnv::none(),
            &[],
            &[],
            &[],
        );
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
            counter_styles: no_counter_styles(),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(
            &doc,
            p,
            &sheets,
            ctx,
            &mut style,
            &mut cache,
            ContainerEnv::none(),
            &[],
            &[],
            &[],
        );
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
            counter_styles: no_counter_styles(),
        };
        let mut style = ComputedStyle::initial();
        let mut cache = CascadeCache::new(&sheets);
        cascade(
            &doc,
            p,
            &sheets,
            ctx,
            &mut style,
            &mut cache,
            ContainerEnv::none(),
            &[],
            &[],
            &[],
        );
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

    /// E39-M3: two inputs differing only in `required` (hence `:invalid`) must
    /// NOT alias in the CascadeCache — `input:invalid` colors only the empty
    /// required one, not the optional one.
    #[test]
    fn validity_pseudo_does_not_alias_in_cache() {
        let doc = parse("<input id='req' required><input id='opt'>");
        let find = |id: &str| {
            let mut stack = doc.children(doc.root());
            while let Some(n) = stack.pop() {
                if doc.get_attribute(n, "id") == Some(id) {
                    return n;
                }
                stack.extend(doc.children(n));
            }
            panic!("#{id}");
        };
        let author = parse_stylesheet("input:invalid { color: #ff0000 }");
        let sheets = [(Origin::Author, pairs(&author.rules))];
        let ctx = EmContext {
            parent_font_size: 16.0,
            root_font_size: 16.0,
            viewport: crate::Viewport::from_width(800.0),
            counter_styles: no_counter_styles(),
        };
        let mut cache = CascadeCache::new(&sheets);
        let run = |el, cache: &mut CascadeCache| {
            let mut style = ComputedStyle::initial();
            cascade(
                &doc,
                el,
                &sheets,
                ctx,
                &mut style,
                cache,
                ContainerEnv::none(),
                &[],
                &[],
                &[],
            );
            style.color
        };
        let red = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        // The required-empty input is :invalid → red.
        assert_eq!(run(find("req"), &mut cache), red);
        // The optional input is :valid; despite identical other features it must
        // not pick up the cached red from `req`.
        assert_ne!(run(find("opt"), &mut cache), red);
    }
}
