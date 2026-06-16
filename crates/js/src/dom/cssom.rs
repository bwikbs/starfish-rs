//! E52-M1 read-only CSSOM: `document.styleSheets` → `CSSStyleSheet` objects,
//! each with `cssRules` (alias `rules`) → `CSSStyleRule`s exposing
//! `selectorText`, `cssText`, and `style.cssText`.
//!
//! Everything is built eagerly as plain `JsObject`s from the `author_sheets`
//! snapshot in [`DomState`] (mirroring `customElements`/`storage`). The sheets
//! are frozen during script execution and these objects are read-only, so a
//! fresh-each-call construction needs no live binding.
//!
//! Selectors keep no verbatim source text in the model, so `selectorText` is a
//! best-effort reconstruction: faithful for the common tag/class/id/attribute
//! forms (`p`, `.x`, `#main`, `div.item`, `a[href]`, `div > p`); structural and
//! functional pseudo-classes are emitted as a `:name` placeholder. Declarations
//! serialize as `name: value` from the verbatim `Value::raw`.
//!
//! Non-goals (deferred to later E52 work / roadmap): write side
//! (`insertRule`/`deleteRule`), `@media`/`@keyframes`/at-rule objects, and
//! `CSSRule` type constants.

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsObject, JsValue};
use starfish_css::model::{Declaration, Rule, Stylesheet};
use starfish_css::selector::{Combinator, Compound, Selector, SelectorPart};

/// Best-effort serialization of one complex selector (a single comma-list entry).
fn serialize_selector(sel: &Selector) -> String {
    let mut out = String::new();
    for part in &sel.parts {
        match part {
            SelectorPart::Compound(c) => out.push_str(&serialize_compound(c)),
            SelectorPart::Combinator(comb) => out.push_str(match comb {
                Combinator::Descendant => " ",
                Combinator::Child => " > ",
                Combinator::NextSibling => " + ",
                Combinator::SubsequentSibling => " ~ ",
            }),
        }
    }
    out
}

/// Serialize a compound selector (`div.item#main[href]`), common forms only.
fn serialize_compound(c: &Compound) -> String {
    let mut out = String::new();
    if let Some(tag) = &c.tag {
        out.push_str(tag);
    } else if c.universal {
        out.push('*');
    }
    for id in &c.ids {
        out.push('#');
        out.push_str(id);
    }
    for class in &c.classes {
        out.push('.');
        out.push_str(class);
    }
    for attr in &c.attrs {
        out.push('[');
        out.push_str(&attr.name);
        out.push(']');
    }
    out
}

/// Serialize a declaration block: `"color: red; font-size: 12px;"`.
fn serialize_declarations(decls: &[Declaration]) -> String {
    let mut out = String::new();
    for d in decls {
        out.push_str(&d.name);
        out.push_str(": ");
        out.push_str(&d.value.raw);
        if d.important {
            out.push_str(" !important");
        }
        out.push(';');
        out.push(' ');
    }
    let trimmed = out.trim_end();
    trimmed.to_string()
}

/// Build the `CSSStyleRule.style` object (read-only `cssText`).
fn build_style(decls: &[Declaration], ctx: &mut Context) -> JsObject {
    let css = serialize_declarations(decls);
    let mut init = ObjectInitializer::new(ctx);
    init.property(
        js_string!("cssText"),
        js_string!(css),
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.build()
}

/// Build one `CSSStyleRule`: `selectorText`, `cssText`, `style`.
fn build_rule(rule: &Rule, ctx: &mut Context) -> JsObject {
    let selector_text = rule
        .selectors
        .iter()
        .map(serialize_selector)
        .collect::<Vec<_>>()
        .join(", ");
    let decls = serialize_declarations(&rule.declarations);
    let css_text = format!("{selector_text} {{ {decls} }}");
    let style = build_style(&rule.declarations, ctx);

    let mut init = ObjectInitializer::new(ctx);
    init.property(
        js_string!("selectorText"),
        js_string!(selector_text),
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.property(
        js_string!("cssText"),
        js_string!(css_text),
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.property(
        js_string!("style"),
        JsValue::from(style),
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.build()
}

/// Build one `CSSStyleSheet`: `cssRules` (alias `rules`) array of `CSSStyleRule`.
fn build_sheet(sheet: &Stylesheet, ctx: &mut Context) -> JsObject {
    let rules: Vec<JsValue> = sheet
        .rules
        .iter()
        .map(|r| JsValue::from(build_rule(r, ctx)))
        .collect();
    let arr = boa_engine::object::builtins::JsArray::from_iter(rules, ctx);
    let arr_val = JsValue::from(arr);

    let mut init = ObjectInitializer::new(ctx);
    init.property(
        js_string!("cssRules"),
        arr_val.clone(),
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.property(
        js_string!("rules"),
        arr_val,
        Attribute::READONLY | Attribute::ENUMERABLE,
    );
    init.build()
}

/// Build the `document.styleSheets` value: a `JsArray` of `CSSStyleSheet`
/// objects, one per author sheet. Constructed fresh from the snapshot.
pub(crate) fn build_style_sheets(sheets: &[Stylesheet], ctx: &mut Context) -> JsValue {
    let objs: Vec<JsValue> = sheets
        .iter()
        .map(|s| JsValue::from(build_sheet(s, ctx)))
        .collect();
    JsValue::from(boa_engine::object::builtins::JsArray::from_iter(objs, ctx))
}
