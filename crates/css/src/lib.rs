//! starfish-css — a lenient, hand-rolled CSS tokenizer + parser that turns a
//! CSS source string into an in-memory stylesheet model (a list of rules, each
//! a selector list plus a declaration block).
//!
//! M2 only **parses**: no cascade, no computed values, no selector matching.
//! The one forward-looking thing computed is each selector's [`Specificity`].
//! Parsing is infallible/lenient — malformed input recovers, never panics.

mod color;
pub mod model;
pub mod parser;
pub mod selector;
pub mod tokenizer;

pub use model::{Component, Declaration, Rgba, Rule, Stylesheet, Value};
pub use selector::{Combinator, Compound, Selector, SelectorPart, Specificity};

/// Parse a CSS source string into a [`Stylesheet`]. Infallible: at-rules are
/// skipped, bad rules/declarations are dropped, and the well-formed rest is
/// returned.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    parser::parse(css)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- helpers ---

    /// Format a selector back to a canonical-ish string for readable asserts.
    fn fmt_selector(s: &Selector) -> String {
        let mut out = String::new();
        for part in &s.parts {
            match part {
                SelectorPart::Compound(c) => {
                    if c.universal {
                        out.push('*');
                    }
                    if let Some(tag) = &c.tag {
                        out.push_str(tag);
                    }
                    for id in &c.ids {
                        out.push('#');
                        out.push_str(id);
                    }
                    for cls in &c.classes {
                        out.push('.');
                        out.push_str(cls);
                    }
                }
                SelectorPart::Combinator(Combinator::Descendant) => out.push(' '),
                SelectorPart::Combinator(Combinator::Child) => out.push_str(" > "),
            }
        }
        out
    }

    fn spec(s: &Selector) -> (u32, u32, u32) {
        (s.specificity.a, s.specificity.b, s.specificity.c)
    }

    /// Parse a single-rule stylesheet and return that rule's selectors.
    fn selectors_of(css: &str) -> Vec<Selector> {
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 1, "expected exactly one rule for {css:?}");
        // move selectors out
        sheet.rules.into_iter().next().unwrap().selectors
    }

    // --- §7.2 selector parsing + specificity ---

    #[test]
    fn sel_universal() {
        let sels = selectors_of("* { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 0, 0));
        assert_eq!(fmt_selector(&sels[0]), "*");
    }

    #[test]
    fn sel_tag() {
        let sels = selectors_of("div { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 0, 1));
    }

    #[test]
    fn sel_class() {
        let sels = selectors_of(".item { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 1, 0));
    }

    #[test]
    fn sel_id() {
        let sels = selectors_of("#main { x: 1 }");
        assert_eq!(spec(&sels[0]), (1, 0, 0));
    }

    #[test]
    fn sel_compound() {
        let sels = selectors_of("div.item#main { x: 1 }");
        assert_eq!(spec(&sels[0]), (1, 1, 1));
        assert_eq!(fmt_selector(&sels[0]), "div#main.item");
    }

    #[test]
    fn sel_descendant() {
        let sels = selectors_of("ul li { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 0, 2));
        assert_eq!(fmt_selector(&sels[0]), "ul li");
    }

    #[test]
    fn sel_descendant_compound() {
        let sels = selectors_of("ul li.active#first { x: 1 }");
        assert_eq!(spec(&sels[0]), (1, 1, 2));
    }

    #[test]
    fn sel_list_comma() {
        let sels = selectors_of("a, b.c { x: 1 }");
        assert_eq!(sels.len(), 2);
        assert_eq!(spec(&sels[0]), (0, 0, 1));
        assert_eq!(spec(&sels[1]), (0, 1, 1));
    }

    #[test]
    fn sel_child_combinator_stored() {
        let sels = selectors_of("div > p { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 0, 2));
        // child combinator stored
        assert!(sels[0]
            .parts
            .iter()
            .any(|p| matches!(p, SelectorPart::Combinator(Combinator::Child))));
        assert_eq!(fmt_selector(&sels[0]), "div > p");
    }

    #[test]
    fn sel_multiple_classes() {
        let sels = selectors_of(".a.b { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 2, 0));
    }

    #[test]
    fn sel_pseudo_invalid_drops_rule() {
        let sheet = parse_stylesheet("a:hover { color: red }");
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn sel_attr_invalid_drops_rule() {
        let sheet = parse_stylesheet("input[type=text] { color: red }");
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn sel_one_bad_in_list_drops_whole_rule() {
        // `b:hover` invalid → whole list (and rule) dropped.
        let sheet = parse_stylesheet("a, b:hover { color: red }");
        assert!(sheet.rules.is_empty());
    }

    // --- §7.3 declarations / values ---

    fn one_rule(css: &str) -> Rule {
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 1, "expected one rule for {css:?}");
        sheet.rules.into_iter().next().unwrap()
    }

    #[test]
    fn decl_basic_named_color() {
        let r = one_rule("p { color: red; }");
        assert_eq!(r.declarations.len(), 1);
        let d = &r.declarations[0];
        assert_eq!(d.name, "color");
        assert_eq!(d.value.raw, "red");
        assert!(!d.important);
        assert_eq!(
            d.value.components,
            vec![Component::Color(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })]
        );
    }

    #[test]
    fn decl_no_trailing_semicolon() {
        let r = one_rule("p { color: red }");
        assert_eq!(r.declarations[0].value.raw, "red");
    }

    #[test]
    fn decl_empty_block() {
        let r = one_rule("p {}");
        assert!(r.declarations.is_empty());
    }

    #[test]
    fn decl_multi_selector_one_decl() {
        let r = one_rule("h1, h2 { margin: 0; }");
        assert_eq!(r.selectors.len(), 2);
        assert_eq!(r.declarations.len(), 1);
        assert_eq!(r.declarations[0].value.components, vec![Component::Number(0.0)]);
    }

    #[test]
    fn decl_dimension_and_percentage() {
        let r = one_rule(".box { width: 50%; padding: 10px 20px; }");
        assert_eq!(r.declarations.len(), 2);
        assert_eq!(
            r.declarations[0].value.components,
            vec![Component::Dimension {
                value: 50.0,
                unit: "%".into()
            }]
        );
        assert_eq!(r.declarations[1].value.raw, "10px 20px");
        assert_eq!(
            r.declarations[1].value.components,
            vec![
                Component::Dimension {
                    value: 10.0,
                    unit: "px".into()
                },
                Component::Dimension {
                    value: 20.0,
                    unit: "px".into()
                },
            ]
        );
    }

    #[test]
    fn decl_hex_colors() {
        let long = one_rule("a { color: #ff0000 }");
        let short = one_rule("a { color: #f00 }");
        let red = Component::Color(Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        assert_eq!(long.declarations[0].value.components, vec![red]);
        let red2 = Component::Color(Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        assert_eq!(short.declarations[0].value.components, vec![red2]);
    }

    #[test]
    fn decl_rgb_function_color() {
        let r = one_rule("b { color: rgb(0, 128, 255); }");
        assert_eq!(
            r.declarations[0].value.components,
            vec![Component::Color(Rgba {
                r: 0,
                g: 128,
                b: 255,
                a: 255
            })]
        );
    }

    #[test]
    fn decl_important() {
        let r = one_rule("p { color: red !important; }");
        assert!(r.declarations[0].important);
        assert_eq!(r.declarations[0].value.raw, "red");
    }

    #[test]
    fn decl_important_odd_spacing() {
        let r = one_rule("p { color: red ! important; }");
        assert!(r.declarations[0].important);
        assert_eq!(r.declarations[0].value.raw, "red");
    }

    #[test]
    fn decl_bad_declaration_recovery() {
        // first decl missing colon → dropped; second survives.
        let r = one_rule("p { color red; font-size: 12px; }");
        assert_eq!(r.declarations.len(), 1);
        assert_eq!(r.declarations[0].name, "font-size");
    }

    #[test]
    fn at_rule_block_skipped() {
        let sheet =
            parse_stylesheet("@media screen { p { color: red } } div { color: blue }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "div");
    }

    #[test]
    fn at_rule_statement_skipped() {
        let sheet = parse_stylesheet("@import \"x.css\"; p { color: red }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "p");
    }

    #[test]
    fn comment_in_value_stripped() {
        let r = one_rule("p { color: /* x */ blue; }");
        assert_eq!(r.declarations[0].value.raw, "blue");
    }

    #[test]
    fn empty_inputs() {
        assert!(parse_stylesheet("").rules.is_empty());
        assert!(parse_stylesheet("   \n\t ").rules.is_empty());
        assert!(parse_stylesheet("/* only a comment */").rules.is_empty());
    }

    #[test]
    fn multi_rule_nested_compound() {
        let sheet = parse_stylesheet(
            "nav ul li a { text-decoration: none } .btn { display: block }",
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(spec(&sheet.rules[0].selectors[0]), (0, 0, 4));
        assert_eq!(spec(&sheet.rules[1].selectors[0]), (0, 1, 0));
    }

    #[test]
    fn function_kept_verbatim() {
        let r = one_rule("div { background: url(foo.png) }");
        match &r.declarations[0].value.components[0] {
            Component::Function { name, raw_args } => {
                assert_eq!(name, "url");
                assert_eq!(raw_args, "foo.png");
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn string_escaped_multibyte_no_panic() {
        // `\é` escape before a multibyte char must not panic mid-char.
        let r = one_rule("p{content:\"\\é\"}");
        assert_eq!(r.declarations[0].value.components, vec![Component::Str("é".into())]);
    }

    #[test]
    fn raw_multibyte_value_no_panic() {
        // Multibyte char in a raw (delim) value position must not panic.
        let r = one_rule("p{x:£}");
        assert_eq!(r.declarations[0].value.raw, "£");
    }

    #[test]
    fn semicolon_inside_function_not_truncated() {
        let sheet = parse_stylesheet("p{grid:foo(a;b);color:red}");
        assert_eq!(sheet.rules.len(), 1);
        let decls = &sheet.rules[0].declarations;
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "grid");
        assert!(
            decls[0].value.raw.contains("foo(a;b)"),
            "grid raw was {:?}",
            decls[0].value.raw
        );
        assert_eq!(decls[1].name, "color");
    }

    #[test]
    fn nested_function_balanced() {
        let r = one_rule("p{color:rgb(rgb(0,0,0))}");
        assert_eq!(r.declarations[0].value.components.len(), 1);
        match &r.declarations[0].value.components[0] {
            Component::Function { name, raw_args } => {
                assert_eq!(name, "rgb");
                assert_eq!(raw_args, "rgb(0,0,0)");
            }
            other => panic!("expected single function, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_function_no_panic() {
        // Unterminated function must not swallow the block's `}` or panic.
        let sheet = parse_stylesheet("p{x:url(abc }");
        // No panic is the primary assertion; recovery yields at most one rule.
        assert!(sheet.rules.len() <= 1);
    }

    #[test]
    fn smoke_small_stylesheet() {
        let css = "
            /* header */
            body { margin: 0; background: #fff; }
            h1, h2 { color: navy; }
            #main .content p { font-size: 14px; line-height: 1.5; }
            a:hover { color: red } /* invalid → dropped */
            @media print { body { color: black } }
        ";
        let sheet = parse_stylesheet(css);
        // body, (h1,h2), (#main .content p)  → 3 rules; a:hover dropped, @media dropped.
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "body");
        assert_eq!(sheet.rules[1].selectors.len(), 2);
        assert_eq!(spec(&sheet.rules[2].selectors[0]), (1, 1, 1));
        // background #fff resolved
        assert_eq!(
            sheet.rules[0].declarations[1].value.components,
            vec![Component::Color(Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            })]
        );
    }
}
