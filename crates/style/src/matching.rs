//! Selector matching against the DOM (§3). Right-to-left with backtracking.

use starfish_css::{
    AttrOp, AttrSelector, Combinator, Compound, Nth, PseudoClass, RelativeSelector, Selector,
    SelectorPart,
};
use starfish_dom::{Document, NodeId, NodeKind};

/// Whether `element` (guaranteed an Element node) matches `selector`.
pub fn matches(doc: &Document, element: NodeId, selector: &Selector) -> bool {
    // Index of the rightmost (subject) compound.
    match last_compound_index(&selector.parts) {
        Some(i) => match_from(doc, element, &selector.parts, i),
        None => false,
    }
}

fn last_compound_index(parts: &[SelectorPart]) -> Option<usize> {
    parts
        .iter()
        .rposition(|p| matches!(p, SelectorPart::Compound(_)))
}

fn compound_at(parts: &[SelectorPart], i: usize) -> &Compound {
    match &parts[i] {
        SelectorPart::Compound(c) => c,
        SelectorPart::Combinator(_) => unreachable!("expected compound at index {i}"),
    }
}

fn combinator_at(parts: &[SelectorPart], i: usize) -> &Combinator {
    match &parts[i] {
        SelectorPart::Combinator(c) => c,
        SelectorPart::Compound(_) => unreachable!("expected combinator at index {i}"),
    }
}

/// Match `parts[..=i]` (with `parts[i]` a Compound) ending at `node`.
fn match_from(doc: &Document, node: NodeId, parts: &[SelectorPart], i: usize) -> bool {
    if !compound_matches(doc, node, compound_at(parts, i)) {
        return false;
    }
    if i == 0 {
        return true;
    }
    // parts[i-1] is always a Combinator; parts[i-2] the next Compound.
    let comb = combinator_at(parts, i - 1);
    let left = i - 2;
    match comb {
        Combinator::Child => doc
            .parent(node)
            .filter(|p| doc.tag_name(*p).is_some())
            .is_some_and(|p| match_from(doc, p, parts, left)),
        Combinator::Descendant => {
            let mut anc = doc.parent(node);
            while let Some(a) = anc {
                if doc.tag_name(a).is_some() && match_from(doc, a, parts, left) {
                    return true;
                }
                anc = doc.parent(a);
            }
            false
        }
        Combinator::NextSibling => doc
            .prev_element_sibling(node)
            .is_some_and(|s| match_from(doc, s, parts, left)),
        Combinator::SubsequentSibling => {
            let mut s = doc.prev_element_sibling(node);
            while let Some(p) = s {
                if match_from(doc, p, parts, left) {
                    return true;
                }
                s = doc.prev_element_sibling(p);
            }
            false
        }
    }
}

/// All simple selectors in `c` must hold (AND).
fn compound_matches(doc: &Document, element: NodeId, c: &Compound) -> bool {
    // tag (both already lowercased). `universal`/`None` impose no constraint.
    if let Some(t) = &c.tag {
        if doc.tag_name(element) != Some(t.as_str()) {
            return false;
        }
    }
    // ids: each requested id must equal the single id attribute.
    if !c.ids.is_empty() {
        match doc.get_attribute(element, "id") {
            Some(id) => {
                if c.ids.iter().any(|want| want != id) {
                    return false;
                }
            }
            None => return false,
        }
    }
    // classes: every requested class present in the space-split set.
    if !c.classes.is_empty() {
        let attr = doc.get_attribute(element, "class").unwrap_or("");
        for want in &c.classes {
            if !attr.split_ascii_whitespace().any(|cls| cls == want) {
                return false;
            }
        }
    }
    // attribute selectors (AND).
    for a in &c.attrs {
        if !attr_matches(doc, element, a) {
            return false;
        }
    }
    // structural / never-match pseudo-classes (AND).
    for p in &c.pseudos {
        if !pseudo_matches(doc, element, p) {
            return false;
        }
    }
    true
}

/// `:lang(want)` (E29-M3): the nearest `lang` attribute up the ancestor chain
/// equals `want` or starts with `want-` (ASCII case-insensitive).
fn lang_matches(doc: &Document, el: NodeId, want: &str) -> bool {
    let mut cur = Some(el);
    while let Some(n) = cur {
        if let Some(l) = doc.get_attribute(n, "lang") {
            if l.eq_ignore_ascii_case(want)
                || l.len() > want.len()
                    && l[..want.len()].eq_ignore_ascii_case(want)
                    && l.as_bytes()[want.len()] == b'-'
            {
                return true;
            }
            return false; // a closer lang overrides; stop at the first one.
        }
        cur = doc.parent(n);
    }
    false
}

/// 1-based index of `el` among its siblings matching `of` (E29-M2). Counts from
/// the end when `from_end`. Returns 0 if `el` isn't among them (never matches).
fn nth_of_index(doc: &Document, el: NodeId, of: &[Selector], from_end: bool) -> i32 {
    let sibs: Vec<NodeId> = match doc.parent(el) {
        Some(p) => doc
            .children(p)
            .into_iter()
            .filter(|c| doc.tag_name(*c).is_some())
            .collect(),
        None => vec![el],
    };
    let matching: Vec<NodeId> = sibs
        .into_iter()
        .filter(|s| of.iter().any(|sel| matches(doc, *s, sel)))
        .collect();
    let Some(pos) = matching.iter().position(|s| *s == el) else {
        return 0;
    };
    if from_end {
        (matching.len() - pos) as i32
    } else {
        (pos + 1) as i32
    }
}

/// Whether `el` satisfies the attribute selector `a`.
fn attr_matches(doc: &Document, el: NodeId, a: &AttrSelector) -> bool {
    let Some(have) = doc.get_attribute(el, &a.name) else {
        return false;
    };
    let ci = a.case_insensitive;
    match a.op {
        AttrOp::Exists => true,
        AttrOp::Equals => eqv(have, want(a), ci),
        AttrOp::Prefix => {
            let w = want(a);
            !w.is_empty() && starts(have, w, ci)
        }
        AttrOp::Suffix => {
            let w = want(a);
            !w.is_empty() && ends(have, w, ci)
        }
        AttrOp::Substring => {
            let w = want(a);
            !w.is_empty() && contains(have, w, ci)
        }
        AttrOp::Includes => {
            // `~=`: a whitespace-separated word equals `w`.
            let w = want(a);
            !w.is_empty()
                && !w.contains(char::is_whitespace)
                && have.split_ascii_whitespace().any(|t| eqv(t, w, ci))
        }
        AttrOp::DashMatch => {
            // `|=`: equal to `w`, or starts with `w-`.
            let w = want(a);
            eqv(have, w, ci) || starts(have, &format!("{w}-"), ci)
        }
    }
}

/// The selector value (operators other than Exists always carry one).
fn want(a: &AttrSelector) -> &str {
    a.value.as_deref().unwrap_or("")
}

fn eqv(have: &str, w: &str, ci: bool) -> bool {
    if ci {
        have.eq_ignore_ascii_case(w)
    } else {
        have == w
    }
}

fn starts(have: &str, w: &str, ci: bool) -> bool {
    if ci {
        have.len() >= w.len() && have[..w.len()].eq_ignore_ascii_case(w)
    } else {
        have.starts_with(w)
    }
}

fn ends(have: &str, w: &str, ci: bool) -> bool {
    if ci {
        have.len() >= w.len() && have[have.len() - w.len()..].eq_ignore_ascii_case(w)
    } else {
        have.ends_with(w)
    }
}

fn contains(have: &str, w: &str, ci: bool) -> bool {
    if ci {
        have.to_ascii_lowercase().contains(&w.to_ascii_lowercase())
    } else {
        have.contains(w)
    }
}

/// The lowercased `type` of an `<input>` (defaulting to `text`), else `None`.
/// `starfish_style` can't depend on `starfish_layout`, so the small form-control
/// classification (E14-M3 state pseudos) is replicated inline here.
fn input_type(doc: &Document, el: NodeId) -> Option<String> {
    (doc.tag_name(el) == Some("input")).then(|| {
        doc.get_attribute(el, "type")
            .unwrap_or("text")
            .to_ascii_lowercase()
    })
}

/// Whether `el` is a form control (`input`/`textarea`/`select`/`button`).
fn is_form_control(doc: &Document, el: NodeId) -> bool {
    matches!(
        doc.tag_name(el),
        Some("input" | "textarea" | "select" | "button")
    )
}

/// Whether `el` is a text-editable control: a `<textarea>`, or an `<input>` of a
/// text-entry type (text/search/email/url/tel/number/password/empty).
fn is_text_editable(doc: &Document, el: NodeId) -> bool {
    if doc.tag_name(el) == Some("textarea") {
        return true;
    }
    matches!(
        input_type(doc, el).as_deref(),
        Some("text" | "search" | "email" | "url" | "tel" | "number" | "password" | "")
    )
}

// E39-M3: constraint-validation helpers. The `style` crate can't depend on
// `layout`, so the candidate/tag/type checks are replicated inline (mirroring
// the form-state pseudos above).

/// Whether `el` is a constraint-validation candidate: a non-hidden/non-button
/// `<input>`, a `<textarea>`, or a `<select>` (E39-M3).
fn is_validation_candidate(doc: &Document, el: NodeId) -> bool {
    match doc.tag_name(el) {
        Some("textarea" | "select") => true,
        Some("input") => !matches!(
            input_type(doc, el).as_deref(),
            Some("hidden" | "button" | "submit" | "reset" | "image")
        ),
        _ => false,
    }
}

/// The control's current value as a string (E39-M3). For `<input>`/`<select>`
/// it is the `value` attribute (default empty); for `<textarea>` it is the
/// `value` attribute if present, else its text content.
fn control_value(doc: &Document, el: NodeId) -> String {
    if doc.tag_name(el) == Some("textarea") {
        if let Some(v) = doc.get_attribute(el, "value") {
            return v.to_string();
        }
        let mut s = String::new();
        for c in doc.children(el) {
            if let NodeKind::Text(t) = doc.kind(c) {
                s.push_str(t);
            }
        }
        return s;
    }
    doc.get_attribute(el, "value").unwrap_or("").to_string()
}

/// Basic `type=email` validity: a single `@` with non-empty local + domain.
fn email_ok(v: &str) -> bool {
    match v.split_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.is_empty() && !domain.contains('@'),
        None => false,
    }
}

/// Basic `type=url` validity: contains `://`, or starts with a scheme (`a-z`
/// letters followed by `:`).
fn url_ok(v: &str) -> bool {
    if v.contains("://") {
        return true;
    }
    match v.split_once(':') {
        Some((scheme, _)) => {
            !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// The `(min, max)` numeric range limits, if any are present and parse (E39-M3).
fn range_limits(doc: &Document, el: NodeId) -> (Option<f64>, Option<f64>) {
    let parse = |name: &str| {
        doc.get_attribute(el, name)
            .and_then(|s| s.trim().parse::<f64>().ok())
    };
    (parse("min"), parse("max"))
}

/// Whether a validation candidate is currently INVALID (E39-M3): empty while
/// `required`; or malformed `type=email`/`url`; or a numeric value outside
/// `min`/`max`.
fn is_invalid_control(doc: &Document, el: NodeId) -> bool {
    let value = control_value(doc, el);
    // `required` + empty.
    if doc.get_attribute(el, "required").is_some() && value.trim().is_empty() {
        return true;
    }
    // Type-specific format checks (only when non-empty).
    if !value.is_empty() {
        match input_type(doc, el).as_deref() {
            Some("email") if !email_ok(&value) => return true,
            Some("url") if !url_ok(&value) => return true,
            _ => {}
        }
        // Range overflow/underflow.
        if let Ok(n) = value.trim().parse::<f64>() {
            let (min, max) = range_limits(doc, el);
            if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                return true;
            }
        }
    }
    false
}

/// Whether `el` has a range constraint and a numeric value (E39-M3). Returns the
/// numeric value plus its in-range flag, or `None` when no range applies.
fn range_state(doc: &Document, el: NodeId) -> Option<bool> {
    let (min, max) = range_limits(doc, el);
    if min.is_none() && max.is_none() {
        return None;
    }
    let n = control_value(doc, el).trim().parse::<f64>().ok()?;
    let in_range = !(min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m));
    Some(in_range)
}

// E58-M3: per-flag constraint-validation result, computed from the E39-M3
// helpers above so the JS Constraint Validation API and the CSS `:valid`/
// `:invalid` pseudo-classes share one source of truth. `valid` excludes
// `customError` (the JS layer ORs in its own custom-message flag).
/// The constraint-validation flags for a control (E58-M3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub pattern_mismatch: bool,
    /// True when `el` is a validation candidate (so the JS `validity` getter and
    /// `willValidate` can short-circuit non-candidates).
    pub candidate: bool,
}

impl Validity {
    /// Whether the control is valid (no built-in error flag set). Does NOT
    /// account for a JS custom error (the caller ORs that in).
    pub fn is_valid(&self) -> bool {
        !(self.value_missing
            || self.type_mismatch
            || self.range_underflow
            || self.range_overflow
            || self.pattern_mismatch)
    }
}

/// Whether `pattern` definitely does NOT match `value`, for the simple literal
/// patterns supported in this MVP (E58-M3). Patterns containing regex
/// metacharacters are treated as a match (no false positives) since there is no
/// regex engine in the style crate. An empty value never mismatches.
fn pattern_mismatch(pattern: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let has_meta = pattern
        .chars()
        .any(|c| "\\^$.|?*+()[]{}".contains(c));
    !has_meta && pattern != value
}

/// The per-flag constraint validity of `el` (E58-M3). Computed entirely from the
/// E39-M3 candidate/value/range/format helpers so it matches `:valid`/`:invalid`.
/// Non-candidates report all flags false (and `candidate=false`).
pub fn validity(doc: &Document, el: NodeId) -> Validity {
    let mut v = Validity::default();
    if !is_validation_candidate(doc, el) {
        return v;
    }
    v.candidate = true;
    let value = control_value(doc, el);
    if doc.get_attribute(el, "required").is_some() && value.trim().is_empty() {
        v.value_missing = true;
    }
    if !value.is_empty() {
        match input_type(doc, el).as_deref() {
            Some("email") if !email_ok(&value) => v.type_mismatch = true,
            Some("url") if !url_ok(&value) => v.type_mismatch = true,
            _ => {}
        }
        if let Ok(n) = value.trim().parse::<f64>() {
            let (min, max) = range_limits(doc, el);
            if min.is_some_and(|m| n < m) {
                v.range_underflow = true;
            }
            if max.is_some_and(|m| n > m) {
                v.range_overflow = true;
            }
        }
        if let Some(p) = doc.get_attribute(el, "pattern") {
            if pattern_mismatch(p, &value) {
                v.pattern_mismatch = true;
            }
        }
    }
    v
}

/// Whether `el` satisfies the pseudo-class `p`.
fn pseudo_matches(doc: &Document, el: NodeId, p: &PseudoClass) -> bool {
    let tag = doc.tag_name(el);
    let has = |name: &str| doc.get_attribute(el, name).is_some();
    match p {
        PseudoClass::FirstChild => doc.element_index(el) == 1,
        PseudoClass::LastChild => doc.element_index_from_end(el) == 1,
        PseudoClass::OnlyChild => doc.element_sibling_count(el) == 1,
        PseudoClass::NthChild(nth) => nth_matches(*nth, doc.element_index(el) as i32),
        PseudoClass::NthOfType(nth) => nth_matches(*nth, doc.element_type_index(el) as i32),
        // E29-M1: type-indexed + last structural pseudos.
        PseudoClass::FirstOfType => doc.element_type_index(el) == 1,
        PseudoClass::LastOfType => doc.element_type_index_from_end(el) == 1,
        PseudoClass::OnlyOfType => doc.element_type_count(el) == 1,
        PseudoClass::NthLastChild(nth) => nth_matches(*nth, doc.element_index_from_end(el) as i32),
        PseudoClass::NthLastOfType(nth) => {
            nth_matches(*nth, doc.element_type_index_from_end(el) as i32)
        }
        // E29-M2: `:nth-child(An+B of S)` — index among S-matching siblings.
        PseudoClass::NthChildOf { nth, of, from_end } => {
            of.iter().any(|s| matches(doc, el, s))
                && nth_matches(*nth, nth_of_index(doc, el, of, *from_end))
        }
        PseudoClass::Root => doc.is_root_element(el),
        PseudoClass::Empty => doc.is_empty_element(el),
        // E29-M3: link + UI pseudos.
        PseudoClass::AnyLink => matches!(tag, Some("a" | "area")) && has("href"),
        PseudoClass::Default => {
            matches!(input_type(doc, el).as_deref(), Some("checkbox" | "radio")) && has("checked")
                || (tag == Some("option") && has("selected"))
        }
        PseudoClass::PlaceholderShown => {
            matches!(tag, Some("input" | "textarea"))
                && has("placeholder")
                && doc.get_attribute(el, "value").unwrap_or("").is_empty()
                && doc.is_empty_element(el)
        }
        PseudoClass::Scope => doc.is_root_element(el),
        PseudoClass::Lang(want) => lang_matches(doc, el, want),
        PseudoClass::Not(inner) => !compound_matches(doc, el, inner),
        // E14-M3 form-state pseudo-classes (own-attribute based).
        PseudoClass::Checked => {
            matches!(input_type(doc, el).as_deref(), Some("checkbox" | "radio")) && has("checked")
                || (tag == Some("option") && has("selected"))
        }
        PseudoClass::Disabled => {
            (is_form_control(doc, el) || matches!(tag, Some("option" | "optgroup" | "fieldset")))
                && has("disabled")
        }
        PseudoClass::Enabled => {
            (is_form_control(doc, el) || matches!(tag, Some("option" | "optgroup")))
                && !has("disabled")
        }
        PseudoClass::Required => is_form_control(doc, el) && has("required"),
        PseudoClass::ReadOnly => is_text_editable(doc, el) && has("readonly"),
        PseudoClass::ReadWrite => is_text_editable(doc, el) && !has("readonly"),
        // E39-M3: constraint-validation pseudo-classes (own-attribute based).
        PseudoClass::Valid => is_validation_candidate(doc, el) && !is_invalid_control(doc, el),
        PseudoClass::Invalid => is_validation_candidate(doc, el) && is_invalid_control(doc, el),
        PseudoClass::InRange => {
            is_validation_candidate(doc, el) && range_state(doc, el) == Some(true)
        }
        PseudoClass::OutOfRange => {
            is_validation_candidate(doc, el) && range_state(doc, el) == Some(false)
        }
        PseudoClass::Optional => is_validation_candidate(doc, el) && !has("required"),
        // E36-M3: open flag lives on the Document, set by show/togglePopover.
        PseudoClass::PopoverOpen => doc.is_popover_open(el),
        // `:is()`/`:where()` match if any listed selector matches `el` (E16-M1).
        PseudoClass::Is(list) | PseudoClass::Where(list) => {
            list.iter().any(|s| matches(doc, el, s))
        }
        // `:has()` matches if any relative selector, anchored at `el`, matches.
        PseudoClass::Has(list) => list.iter().any(|r| has_matches(doc, el, r)),
        // E33-M3: `:host` never matches via the ordinary matcher — it is matched
        // only against the shadow host in the dedicated host-rule path.
        PseudoClass::Host(_) => false,
        PseudoClass::NeverMatch => false,
    }
}

/// Whether the `:has()` relative selector `r`, anchored at `el`, matches some
/// element in `el`'s subtree/siblings per its combinator. The inner complex
/// selector is matched against each candidate independently via `matches`
/// (subject-against-candidate): correct for the common `:has(.x)`/`:has(> .x)`/
/// `:has(+ .x)`/`:has(~ .x)` cases. Documented limitation: a multi-compound
/// relative selector like `:has(.a .b)` is matched as "some descendant matches
/// `.a .b`" rather than re-anchoring at `el`.
fn has_matches(doc: &Document, el: NodeId, r: &RelativeSelector) -> bool {
    match r.combinator {
        Combinator::Child => element_children(doc, el)
            .into_iter()
            .any(|c| matches(doc, c, &r.selector)),
        Combinator::Descendant => has_descendant(doc, el, &r.selector, 64),
        Combinator::NextSibling => doc
            .next_element_sibling(el)
            .is_some_and(|s| matches(doc, s, &r.selector)),
        Combinator::SubsequentSibling => {
            let mut s = doc.next_element_sibling(el);
            while let Some(sib) = s {
                if matches(doc, sib, &r.selector) {
                    return true;
                }
                s = doc.next_element_sibling(sib);
            }
            false
        }
    }
}

/// Element children of `el` (skipping text/comment nodes).
fn element_children(doc: &Document, el: NodeId) -> Vec<NodeId> {
    doc.children(el)
        .into_iter()
        .filter(|&c| doc.tag_name(c).is_some())
        .collect()
}

/// Depth-capped DFS: whether any element in `el`'s subtree matches `sel`.
fn has_descendant(doc: &Document, el: NodeId, sel: &Selector, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    for child in element_children(doc, el) {
        if matches(doc, child, sel) || has_descendant(doc, child, sel, depth - 1) {
            return true;
        }
    }
    false
}

/// Whether a 1-based index `i` (`i ≥ 1`) matches `An+B`, i.e. `∃ n ≥ 0 :
/// i = a·n + b`.
fn nth_matches(nth: Nth, i: i32) -> bool {
    let Nth { a, b } = nth;
    if a == 0 {
        return i == b;
    }
    let diff = i - b;
    diff % a == 0 && diff / a >= 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;

    /// Parse a single selector via the throwaway-stylesheet trick.
    fn sel(s: &str) -> Selector {
        let sheet = parse_stylesheet(&format!("{s}{{x:1}}"));
        sheet
            .rules
            .into_iter()
            .next()
            .expect("a rule")
            .selectors
            .into_iter()
            .next()
            .expect("a selector")
    }

    fn dfs(doc: &Document) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in doc.children(n).into_iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Element ids (document order) that match `selector`.
    fn matched(doc: &Document, selector: &str) -> Vec<NodeId> {
        let s = sel(selector);
        dfs(doc)
            .into_iter()
            .filter(|&n| doc.tag_name(n).is_some() && matches(doc, n, &s))
            .collect()
    }

    fn find_id(doc: &Document, id: &str) -> NodeId {
        dfs(doc)
            .into_iter()
            .find(|&n| doc.get_attribute(n, "id") == Some(id))
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    fn matches_id(doc: &Document, id: &str, selector: &str) -> bool {
        let s = sel(selector);
        matches(doc, find_id(doc, id), &s)
    }

    // --- attribute operators ---

    #[test]
    fn attr_ops_match() {
        let doc = parse(
            "<input id='a' type='text' class='foo bar' \
             href='https://x' data-x lang='en-US'>",
        );
        assert!(matches_id(&doc, "a", "[type=text]"));
        assert!(!matches_id(&doc, "a", "[type=submit]"));
        assert!(matches_id(&doc, "a", "[class~=foo]"));
        assert!(!matches_id(&doc, "a", "[class~=fo]"));
        assert!(matches_id(&doc, "a", "[href^=\"https\"]"));
        assert!(!matches_id(&doc, "a", "[href^=\"ftp\"]"));
        assert!(matches_id(&doc, "a", "[href$=\"x\"]"));
        assert!(matches_id(&doc, "a", "[href*=\"//\"]"));
        assert!(matches_id(&doc, "a", "[data-x]"));
        assert!(!matches_id(&doc, "a", "[data-y]"));
        assert!(matches_id(&doc, "a", "[lang|=en]"));
        assert!(!matches_id(&doc, "a", "[lang|=fr]"));
    }

    #[test]
    fn attr_case_insensitive() {
        let doc = parse("<input id='a' type='TEXT'>");
        assert!(!matches_id(&doc, "a", "[type=text]"));
        assert!(matches_id(&doc, "a", "[type=text i]"));
    }

    #[test]
    fn attr_empty_value_edge_cases() {
        let doc = parse("<input id='a' type='text'>");
        // ^=/$=/*= with empty value never match.
        assert!(!matches_id(&doc, "a", "[type^=\"\"]"));
        assert!(!matches_id(&doc, "a", "[type$=\"\"]"));
        assert!(!matches_id(&doc, "a", "[type*=\"\"]"));
    }

    // --- structural pseudo-classes ---

    #[test]
    fn first_last_only_child() {
        let doc = parse("<ul><li id='a'>1</li><li id='b'>2</li><li id='c'>3</li></ul>");
        assert!(matches_id(&doc, "a", "li:first-child"));
        assert!(!matches_id(&doc, "b", "li:first-child"));
        assert!(matches_id(&doc, "c", "li:last-child"));
        assert!(!matches_id(&doc, "b", "li:last-child"));
        assert!(!matches_id(&doc, "a", "li:only-child"));
        let only = parse("<ul><li id='x'>1</li></ul>");
        assert!(matches_id(&only, "x", "li:only-child"));
    }

    #[test]
    fn nth_child_striping() {
        let doc = parse(
            "<ul><li id='1'>1</li><li id='2'>2</li><li id='3'>3</li>\
             <li id='4'>4</li></ul>",
        );
        let even = matched(&doc, "li:nth-child(even)");
        assert_eq!(even, vec![find_id(&doc, "2"), find_id(&doc, "4")]);
        let odd = matched(&doc, "li:nth-child(odd)");
        assert_eq!(odd, vec![find_id(&doc, "1"), find_id(&doc, "3")]);
        // (2n) == even.
        assert_eq!(matched(&doc, "li:nth-child(2n)"), even);
        // (2n+1) == odd.
        assert_eq!(matched(&doc, "li:nth-child(2n+1)"), odd);
        // fixed position.
        assert_eq!(matched(&doc, "li:nth-child(3)"), vec![find_id(&doc, "3")]);
        // -n+3 → first three.
        assert_eq!(
            matched(&doc, "li:nth-child(-n+3)"),
            vec![find_id(&doc, "1"), find_id(&doc, "2"), find_id(&doc, "3")]
        );
    }

    #[test]
    fn nth_matches_an_b_cases() {
        // a==0 fixed.
        assert!(nth_matches(Nth { a: 0, b: 2 }, 2));
        assert!(!nth_matches(Nth { a: 0, b: 2 }, 3));
        // 2n (even).
        assert!(nth_matches(Nth { a: 2, b: 0 }, 2));
        assert!(!nth_matches(Nth { a: 2, b: 0 }, 3));
        // -n+3.
        assert!(nth_matches(Nth { a: -1, b: 3 }, 1));
        assert!(nth_matches(Nth { a: -1, b: 3 }, 3));
        assert!(!nth_matches(Nth { a: -1, b: 3 }, 4));
    }

    #[test]
    fn nth_of_type_vs_child() {
        // span, p, p — for the first p: type-index 1, child-index 2.
        let doc = parse("<div><span id='s'>x</span><p id='p1'>1</p><p id='p2'>2</p></div>");
        assert!(matches_id(&doc, "p1", "p:nth-of-type(1)"));
        assert!(!matches_id(&doc, "p1", "p:nth-child(1)"));
        assert!(matches_id(&doc, "p2", "p:nth-of-type(2)"));
    }

    #[test]
    fn empty_pseudo() {
        let doc = parse("<div id='e'></div><div id='ws'>  </div><div id='x'>hi</div>");
        assert!(matches_id(&doc, "e", "div:empty"));
        assert!(matches_id(&doc, "ws", "div:empty"));
        assert!(!matches_id(&doc, "x", "div:empty"));
    }

    #[test]
    fn root_pseudo() {
        let doc = parse("<html><body id='b'>x</body></html>");
        let html = matched(&doc, ":root");
        assert_eq!(html.len(), 1);
        assert_eq!(doc.tag_name(html[0]), Some("html"));
        assert!(!matches_id(&doc, "b", ":root"));
    }

    #[test]
    fn not_pseudo() {
        let doc = parse("<div id='a' class='x'>a</div><div id='b'>b</div><span id='s'>s</span>");
        assert!(!matches_id(&doc, "a", "div:not(.x)"));
        assert!(matches_id(&doc, "b", "div:not(.x)"));
        assert!(matches_id(&doc, "a", ":not(span)"));
        assert!(!matches_id(&doc, "s", ":not(span)"));
    }

    // --- sibling combinators ---

    #[test]
    fn adjacent_sibling() {
        let doc = parse(
            "<div><h1 id='h'>t</h1><p id='p1'>a</p><p id='p2'>b</p>\
             <p id='before'>z</p></div>",
        );
        // h1 + p matches only the p immediately after the h1.
        let m = matched(&doc, "h1 + p");
        assert_eq!(m, vec![find_id(&doc, "p1")]);
        // a p before the h1 never matches.
        let doc2 = parse("<div><p id='pre'>x</p><h1>t</h1></div>");
        assert!(matched(&doc2, "h1 + p").is_empty());
    }

    #[test]
    fn general_sibling_backtracking() {
        let doc = parse(
            "<div><p id='pre'>0</p><h1 id='h'>t</h1><span>x</span>\
             <p id='p1'>a</p><p id='p2'>b</p></div>",
        );
        // h1 ~ p matches every p after the h1 (p1, p2), not the p before it.
        let m = matched(&doc, "h1 ~ p");
        assert_eq!(m, vec![find_id(&doc, "p1"), find_id(&doc, "p2")]);
        assert!(!m.contains(&find_id(&doc, "pre")));
    }

    // --- E14-M3 form-state pseudo-classes ---

    #[test]
    fn checked_pseudo() {
        let doc = parse(
            "<input id='cb' type='checkbox' checked>\
             <input id='cbn' type='checkbox'>\
             <input id='rd' type='radio' checked>\
             <input id='txt' type='text' checked>\
             <select><option id='o1' selected>A<option id='o2'>B</select>",
        );
        assert!(matches_id(&doc, "cb", ":checked"));
        assert!(!matches_id(&doc, "cbn", ":checked"));
        assert!(matches_id(&doc, "rd", ":checked"));
        // A text input is never :checked even with a stray `checked` attr.
        assert!(!matches_id(&doc, "txt", ":checked"));
        assert!(matches_id(&doc, "o1", ":checked"));
        assert!(!matches_id(&doc, "o2", ":checked"));
    }

    #[test]
    fn disabled_enabled_pseudo() {
        let doc = parse(
            "<input id='i' disabled>\
             <input id='e'>\
             <div id='d' disabled>x</div>\
             <select><option id='od' disabled>A<option id='oe'>B</select>",
        );
        assert!(matches_id(&doc, "i", ":disabled"));
        assert!(!matches_id(&doc, "e", ":disabled"));
        // A plain <div disabled> is not a form control → not :disabled.
        assert!(!matches_id(&doc, "d", ":disabled"));
        assert!(matches_id(&doc, "od", ":disabled"));
        // :enabled is the complement for form controls / options.
        assert!(matches_id(&doc, "e", ":enabled"));
        assert!(!matches_id(&doc, "i", ":enabled"));
        assert!(matches_id(&doc, "oe", ":enabled"));
        assert!(!matches_id(&doc, "od", ":enabled"));
        // A <div> is neither :enabled nor :disabled.
        assert!(!matches_id(&doc, "d", ":enabled"));
    }

    #[test]
    fn required_pseudo() {
        let doc = parse("<input id='r' required><input id='n'><div id='d' required>x</div>");
        assert!(matches_id(&doc, "r", ":required"));
        assert!(!matches_id(&doc, "n", ":required"));
        assert!(!matches_id(&doc, "d", ":required"));
    }

    #[test]
    fn read_only_read_write_pseudo() {
        let doc = parse(
            "<input id='ro' readonly>\
             <input id='rw'>\
             <textarea id='ta' readonly></textarea>\
             <input id='cb' type='checkbox' readonly>",
        );
        assert!(matches_id(&doc, "ro", ":read-only"));
        assert!(!matches_id(&doc, "rw", ":read-only"));
        assert!(matches_id(&doc, "ta", ":read-only"));
        assert!(matches_id(&doc, "rw", ":read-write"));
        assert!(!matches_id(&doc, "ro", ":read-write"));
        // A checkbox is not text-editable → neither :read-only nor :read-write.
        assert!(!matches_id(&doc, "cb", ":read-only"));
        assert!(!matches_id(&doc, "cb", ":read-write"));
    }

    // --- E16-M1: :is / :where / :has ---

    #[test]
    fn is_matches_any_in_list() {
        let doc = parse(
            "<div id='a' class='a'>x</div><div id='b' class='b'>y</div>\
             <div id='c' class='c'>z</div>",
        );
        assert!(matches_id(&doc, "a", ":is(.a, .b)"));
        assert!(matches_id(&doc, "b", ":is(.a, .b)"));
        assert!(!matches_id(&doc, "c", ":is(.a, .b)"));
    }

    #[test]
    fn where_matches_like_is() {
        let doc = parse("<p id='p'>x</p><span id='s'>y</span>");
        assert!(matches_id(&doc, "p", ":where(p, span)"));
        assert!(matches_id(&doc, "s", ":where(p, span)"));
    }

    #[test]
    fn has_descendant() {
        let doc = parse(
            "<div id='with'><span><p>x</p></span></div>\
             <div id='without'><span>y</span></div>",
        );
        assert!(matches_id(&doc, "with", ":has(p)"));
        assert!(!matches_id(&doc, "without", ":has(p)"));
    }

    #[test]
    fn has_child_only() {
        let doc = parse(
            "<div id='direct'><p>x</p></div>\
             <div id='nested'><span><p>y</p></span></div>",
        );
        // `:has(> p)` requires a DIRECT child p.
        assert!(matches_id(&doc, "direct", ":has(> p)"));
        assert!(!matches_id(&doc, "nested", ":has(> p)"));
        // descendant form matches both.
        assert!(matches_id(&doc, "nested", ":has(p)"));
    }

    #[test]
    fn has_next_and_subsequent_sibling() {
        let doc = parse(
            "<div><h1 id='h'>t</h1><p>a</p><span>b</span><p>c</p></div>\
             <div><h1 id='lone'>t</h1></div>",
        );
        // `:has(+ p)`: immediately-following sibling is a p.
        assert!(matches_id(&doc, "h", ":has(+ p)"));
        assert!(!matches_id(&doc, "lone", ":has(+ p)"));
        // `:has(~ span)`: some following sibling is a span.
        assert!(matches_id(&doc, "h", ":has(~ span)"));
        assert!(!matches_id(&doc, "lone", ":has(~ span)"));
    }

    // --- E39-M3 constraint-validation pseudos ---

    #[test]
    fn validity_required_empty_vs_filled() {
        let doc = parse(
            "<input id='empty' required>\
             <input id='filled' required value='x'>",
        );
        assert!(matches_id(&doc, "empty", ":invalid"));
        assert!(!matches_id(&doc, "empty", ":valid"));
        assert!(matches_id(&doc, "filled", ":valid"));
        assert!(!matches_id(&doc, "filled", ":invalid"));
    }

    #[test]
    fn validity_email_format() {
        let doc = parse(
            "<input id='bad' type='email' value='bad'>\
             <input id='good' type='email' value='a@b.com'>",
        );
        assert!(matches_id(&doc, "bad", ":invalid"));
        assert!(matches_id(&doc, "good", ":valid"));
    }

    #[test]
    fn validity_number_range() {
        let doc = parse(
            "<input id='over' type='number' min='1' max='10' value='20'>\
             <input id='ok' type='number' min='1' max='10' value='5'>",
        );
        assert!(matches_id(&doc, "over", ":out-of-range"));
        assert!(matches_id(&doc, "over", ":invalid"));
        assert!(matches_id(&doc, "ok", ":in-range"));
        assert!(matches_id(&doc, "ok", ":valid"));
        // No range constraint → neither in-range nor out-of-range.
        let nr = parse("<input id='n' type='number' value='5'>");
        assert!(!matches_id(&nr, "n", ":in-range"));
        assert!(!matches_id(&nr, "n", ":out-of-range"));
    }

    #[test]
    fn validity_optional_vs_required() {
        let doc = parse(
            "<input id='opt'>\
             <input id='req' required>",
        );
        assert!(matches_id(&doc, "opt", ":optional"));
        assert!(!matches_id(&doc, "opt", ":required"));
        assert!(!matches_id(&doc, "req", ":optional"));
        assert!(matches_id(&doc, "req", ":required"));
    }

    #[test]
    fn validity_non_candidate_matches_none() {
        let doc = parse("<div id='d'>x</div><input id='h' type='hidden'>");
        for p in [":valid", ":invalid", ":in-range", ":out-of-range", ":optional"] {
            assert!(!matches_id(&doc, "d", p), "div should not match {p}");
            assert!(!matches_id(&doc, "h", p), "hidden input should not match {p}");
        }
    }

    #[test]
    fn validity_textarea_text_content() {
        // `<textarea>` value is its text content; empty + required → invalid.
        let doc = parse(
            "<textarea id='empty' required></textarea>\
             <textarea id='filled' required>hi</textarea>",
        );
        assert!(matches_id(&doc, "empty", ":invalid"));
        assert!(matches_id(&doc, "filled", ":valid"));
    }
}
