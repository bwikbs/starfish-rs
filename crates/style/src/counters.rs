//! CSS counters (E16-M1): a scoped counter stack tracked through the style
//! tree, plus `counter()`/`counters()` formatting for `content`.
//!
//! `counter-reset` pushes a new value onto each named counter's stack (entering
//! a fresh scope); `counter-increment` adds to the innermost value.
//! `style_node` reverts each element's resets after descending its subtree (so
//! a counter scope is bounded to the element + its descendants), while leaving
//! increments in place so later siblings keep accumulating.

use std::collections::HashMap;

/// A live counter set: each counter name maps to a stack of values, innermost
/// (current scope) last. Empty stack ⇒ the counter is not yet instantiated.
#[derive(Default)]
pub(crate) struct CounterState {
    map: HashMap<String, Vec<i32>>,
}

impl CounterState {
    /// Apply `counter-reset`: push each name's reset value as a new innermost
    /// scope. Returns the names pushed, so the caller can pop them after the
    /// subtree is processed (`undo_reset`).
    pub(crate) fn apply_reset(&mut self, resets: &[(String, i32)]) -> Vec<String> {
        let mut pushed = Vec::with_capacity(resets.len());
        for (name, value) in resets {
            self.map.entry(name.clone()).or_default().push(*value);
            pushed.push(name.clone());
        }
        pushed
    }

    /// Apply `counter-increment`: add `delta` to the innermost value, or, if the
    /// counter does not exist yet, instantiate it at `delta` (per spec, an
    /// increment on a non-existent counter acts as `counter-reset: name 0`
    /// followed by the increment).
    pub(crate) fn apply_increment(&mut self, incs: &[(String, i32)]) {
        for (name, delta) in incs {
            let stack = self.map.entry(name.clone()).or_default();
            match stack.last_mut() {
                Some(top) => *top += delta,
                None => stack.push(*delta),
            }
        }
    }

    /// Revert the resets recorded by a matching `apply_reset` (pop one value off
    /// each named stack). Increments are intentionally NOT undone.
    pub(crate) fn undo_reset(&mut self, names: Vec<String>) {
        for name in names {
            if let Some(stack) = self.map.get_mut(&name) {
                stack.pop();
            }
        }
    }

    /// The innermost value of `name`, or 0 if the counter does not exist.
    pub(crate) fn value(&self, name: &str) -> i32 {
        self.map
            .get(name)
            .and_then(|s| s.last())
            .copied()
            .unwrap_or(0)
    }

    /// The full nesting stack of `name` (outermost→innermost), empty if absent.
    pub(crate) fn stack(&self, name: &str) -> &[i32] {
        self.map.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// A `counter()`/`counters()` style argument.
#[derive(Clone, Copy)]
pub(crate) enum CounterStyleKind {
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerAlpha,
    UpperAlpha,
}

impl CounterStyleKind {
    /// Map a `<counter-style>` keyword to a kind, defaulting to `Decimal` for
    /// unknown / unsupported styles.
    fn from_keyword(k: &str) -> CounterStyleKind {
        match k.trim().to_ascii_lowercase().as_str() {
            "lower-roman" => CounterStyleKind::LowerRoman,
            "upper-roman" => CounterStyleKind::UpperRoman,
            "lower-alpha" | "lower-latin" => CounterStyleKind::LowerAlpha,
            "upper-alpha" | "upper-latin" => CounterStyleKind::UpperAlpha,
            _ => CounterStyleKind::Decimal,
        }
    }
}

/// Format a counter value `v` with `style`.
pub(crate) fn format_counter(v: i32, style: CounterStyleKind) -> String {
    match style {
        CounterStyleKind::Decimal => v.to_string(),
        CounterStyleKind::LowerRoman => to_roman(v, false),
        CounterStyleKind::UpperRoman => to_roman(v, true),
        CounterStyleKind::LowerAlpha => to_alpha(v, false),
        CounterStyleKind::UpperAlpha => to_alpha(v, true),
    }
}

/// Roman numerals for 1..=3999; outside that range falls back to decimal.
fn to_roman(v: i32, upper: bool) -> String {
    if !(1..=3999).contains(&v) {
        return v.to_string();
    }
    const TABLE: [(i32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = v;
    let mut out = String::new();
    for (val, sym) in TABLE {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    if upper {
        out.to_ascii_uppercase()
    } else {
        out
    }
}

/// Bijective base-26 alphabetic numbering (1→a, 26→z, 27→aa). Values ≤ 0 fall
/// back to decimal.
fn to_alpha(v: i32, upper: bool) -> String {
    if v <= 0 {
        return v.to_string();
    }
    let mut n = v;
    let mut buf = Vec::new();
    while n > 0 {
        n -= 1;
        buf.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    let out: String = buf.into_iter().rev().collect();
    if upper {
        out.to_ascii_uppercase()
    } else {
        out
    }
}

/// Parse `counter()` args: `name` or `name, <style>`. Returns the counter name
/// and its style (default `Decimal`).
pub(crate) fn parse_counter_args(raw: &str) -> (String, CounterStyleKind) {
    let mut parts = raw.splitn(2, ',');
    let name = parts.next().unwrap_or("").trim().to_string();
    let style = parts
        .next()
        .map(CounterStyleKind::from_keyword)
        .unwrap_or(CounterStyleKind::Decimal);
    (name, style)
}

/// Parse `counters()` args: `name, "sep"[, <style>]`. Returns the name, the
/// separator string (quotes stripped), and the style (default `Decimal`).
pub(crate) fn parse_counters_args(raw: &str) -> (String, String, CounterStyleKind) {
    let mut parts = raw.splitn(3, ',');
    let name = parts.next().unwrap_or("").trim().to_string();
    let sep = parts
        .next()
        .map(|s| strip_quotes(s.trim()))
        .unwrap_or_default();
    let style = parts
        .next()
        .map(CounterStyleKind::from_keyword)
        .unwrap_or(CounterStyleKind::Decimal);
    (name, sep, style)
}

/// Strip a single pair of matching surrounding quotes.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_scope_push_pop() {
        let mut cs = CounterState::default();
        let pushed = cs.apply_reset(&[("item".into(), 0)]);
        cs.apply_increment(&[("item".into(), 1)]);
        assert_eq!(cs.value("item"), 1);
        cs.undo_reset(pushed);
        assert_eq!(cs.value("item"), 0);
    }

    #[test]
    fn increment_without_reset_instantiates() {
        let mut cs = CounterState::default();
        cs.apply_increment(&[("c".into(), 5)]);
        assert_eq!(cs.value("c"), 5);
    }

    #[test]
    fn roman_and_alpha() {
        assert_eq!(format_counter(1, CounterStyleKind::LowerRoman), "i");
        assert_eq!(format_counter(4, CounterStyleKind::LowerRoman), "iv");
        assert_eq!(format_counter(9, CounterStyleKind::UpperRoman), "IX");
        assert_eq!(format_counter(1, CounterStyleKind::LowerAlpha), "a");
        assert_eq!(format_counter(27, CounterStyleKind::LowerAlpha), "aa");
        assert_eq!(format_counter(28, CounterStyleKind::UpperAlpha), "AB");
        // out of range roman → decimal.
        assert_eq!(format_counter(4000, CounterStyleKind::LowerRoman), "4000");
    }

    #[test]
    fn parse_args() {
        let (n, _) = parse_counter_args("item");
        assert_eq!(n, "item");
        let (n2, _) = parse_counter_args("item, lower-roman");
        assert_eq!(n2, "item");
        let (n3, sep, _) = parse_counters_args("item, \".\"");
        assert_eq!((n3.as_str(), sep.as_str()), ("item", "."));
    }
}
