//! starfish-css — a lenient, hand-rolled CSS tokenizer + parser that turns a
//! CSS source string into an in-memory stylesheet model (a list of rules, each
//! a selector list plus a declaration block).
//!
//! M2 only **parses**: no cascade, no computed values, no selector matching.
//! The one forward-looking thing computed is each selector's [`Specificity`].
//! Parsing is infallible/lenient — malformed input recovers, never panics.

mod color;
pub mod model;

pub use color::parse_color;
pub mod parser;
pub use parser::parse_media_query;
pub mod selector;
pub mod tokenizer;

pub use model::{
    Component, Declaration, FontFaceRule, FontFaceStyle, FontSrc, MediaBlock, MediaCondition,
    MediaFeature, MediaQuery, MediaType, Orientation, Rgba, Rule, Stylesheet, Value,
};
pub use selector::{
    AttrOp, AttrSelector, Combinator, Compound, Nth, PseudoClass, PseudoElement, RelativeSelector,
    Selector, SelectorPart, Specificity,
};

/// Parse a CSS source string into a [`Stylesheet`]. Infallible: at-rules are
/// skipped, bad rules/declarations are dropped, and the well-formed rest is
/// returned.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    parser::parse(css)
}

/// Parse a CSS value string into a list of [`Component`]s, reusing the same
/// classification used for declaration values. Used to re-parse `var()`
/// fallback strings. Whitespace-trimmed; no `!important` handling.
pub fn parse_component_values(s: &str) -> Vec<Component> {
    parser::parse_component_values(s)
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
                SelectorPart::Combinator(Combinator::NextSibling) => out.push_str(" + "),
                SelectorPart::Combinator(Combinator::SubsequentSibling) => out.push_str(" ~ "),
            }
        }
        out
    }

    /// The single compound of a one-compound selector.
    fn compound_of(s: &Selector) -> &Compound {
        s.parts
            .iter()
            .find_map(|p| match p {
                SelectorPart::Compound(c) => Some(c),
                _ => None,
            })
            .expect("a compound")
    }

    fn spec(s: &Selector) -> (u32, u32, u32) {
        (s.specificity.a, s.specificity.b, s.specificity.c)
    }

    /// Parse a single-rule stylesheet and return that rule's selectors.
    fn selectors_of(css: &str) -> Vec<Selector> {
        let sheet = parse_stylesheet(css);
        assert_eq!(
            sheet.rules.len(),
            1,
            "expected exactly one rule for {css:?}"
        );
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
    fn sel_pseudo_unknown_kept_as_never_match() {
        // E7-M1: `:hover` now parses as a NeverMatch rule; the sheet survives.
        use selector::PseudoClass;
        let sels = selectors_of("a:hover { color: red }");
        let c = compound_of(&sels[0]);
        assert_eq!(c.pseudos, vec![PseudoClass::NeverMatch]);
        // pseudo-class still counts toward specificity (class-level).
        assert_eq!(spec(&sels[0]), (0, 1, 1));
    }

    #[test]
    fn sel_attr_now_parses() {
        // E7-M1: `[type=text]` is a real attribute selector now.
        use selector::AttrOp;
        let sels = selectors_of("input[type=text] { color: red }");
        let c = compound_of(&sels[0]);
        assert_eq!(c.attrs.len(), 1);
        assert_eq!(c.attrs[0].name, "type");
        assert_eq!(c.attrs[0].op, AttrOp::Equals);
        assert_eq!(c.attrs[0].value.as_deref(), Some("text"));
        assert_eq!(spec(&sels[0]), (0, 1, 1));
    }

    #[test]
    fn sel_pseudo_in_list_kept() {
        // E7-M1: `b:hover` is now a NeverMatch selector, so the 2-selector rule
        // survives instead of dropping.
        let sels = selectors_of("a, b:hover { color: red }");
        assert_eq!(sels.len(), 2);
        assert_eq!(fmt_selector(&sels[0]), "a");
    }

    // --- E7-M1: attribute selectors ---

    #[test]
    fn attr_exists() {
        use selector::AttrOp;
        let sels = selectors_of("[data-x] { x: 1 }");
        let a = &compound_of(&sels[0]).attrs[0];
        assert_eq!(a.op, AttrOp::Exists);
        assert_eq!(a.value, None);
        assert_eq!(spec(&sels[0]), (0, 1, 0));
    }

    #[test]
    fn attr_operators() {
        use selector::AttrOp::*;
        for (css, op) in [
            ("[class~=foo]", Includes),
            ("[lang|=en]", DashMatch),
            ("[href^=\"https\"]", Prefix),
            ("[src$=\".png\"]", Suffix),
            ("[title*=x]", Substring),
            ("[type=text]", Equals),
        ] {
            let sels = selectors_of(&format!("{css} {{ x: 1 }}"));
            assert_eq!(compound_of(&sels[0]).attrs[0].op, op, "{css}");
        }
    }

    #[test]
    fn attr_case_insensitive_flag() {
        let sels = selectors_of("[a=b i] { x: 1 }");
        let a = &compound_of(&sels[0]).attrs[0];
        assert!(a.case_insensitive);
        assert_eq!(a.value.as_deref(), Some("b"));
    }

    #[test]
    fn attr_name_lowercased() {
        let sels = selectors_of("[Data-X] { x: 1 }");
        assert_eq!(compound_of(&sels[0]).attrs[0].name, "data-x");
    }

    // --- E7-M1: structural pseudo-classes ---

    #[test]
    fn pseudo_structural_bare() {
        use selector::PseudoClass::*;
        for (css, want) in [
            ("li:first-child", FirstChild),
            ("li:last-child", LastChild),
            ("li:only-child", OnlyChild),
            ("p:root", Root),
            ("p:empty", Empty),
        ] {
            let sels = selectors_of(&format!("{css} {{ x: 1 }}"));
            assert_eq!(compound_of(&sels[0]).pseudos[0], want, "{css}");
            assert_eq!(spec(&sels[0]), (0, 1, 1), "{css}");
        }
    }

    #[test]
    fn pseudo_nth_forms() {
        use selector::{Nth, PseudoClass};
        let cases = [
            ("li:nth-child(2)", Nth { a: 0, b: 2 }),
            ("li:nth-child(odd)", Nth { a: 2, b: 1 }),
            ("li:nth-child(even)", Nth { a: 2, b: 0 }),
            ("li:nth-child(2n)", Nth { a: 2, b: 0 }),
            ("li:nth-child(2n+1)", Nth { a: 2, b: 1 }),
            ("li:nth-child(2n-1)", Nth { a: 2, b: -1 }),
            ("li:nth-child(-n+3)", Nth { a: -1, b: 3 }),
            ("li:nth-child(n)", Nth { a: 1, b: 0 }),
        ];
        for (css, want) in cases {
            let sels = selectors_of(&format!("{css} {{ x: 1 }}"));
            assert_eq!(
                compound_of(&sels[0]).pseudos[0],
                PseudoClass::NthChild(want),
                "{css}"
            );
        }
    }

    #[test]
    fn pseudo_nth_of_type() {
        use selector::{Nth, PseudoClass};
        let sels = selectors_of("p:nth-of-type(2) { x: 1 }");
        assert_eq!(
            compound_of(&sels[0]).pseudos[0],
            PseudoClass::NthOfType(Nth { a: 0, b: 2 })
        );
    }

    #[test]
    fn pseudo_nth_bad_drops_rule() {
        assert!(parse_stylesheet("li:nth-child(foo) { x: 1 }")
            .rules
            .is_empty());
        assert!(parse_stylesheet("li:nth-child(2.5) { x: 1 }")
            .rules
            .is_empty());
    }

    #[test]
    fn pseudo_not_specificity() {
        // :not adds its argument's specificity; the :not itself adds nothing.
        let sels = selectors_of("div:not(.x) { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 1, 1)); // tag c=1 + .x b=1
        let sels2 = selectors_of(":not(#id) { x: 1 }");
        assert_eq!(spec(&sels2[0]), (1, 0, 0));
        let sels3 = selectors_of(":not(div) { x: 1 }");
        assert_eq!(spec(&sels3[0]), (0, 0, 1));
    }

    #[test]
    fn pseudo_not_inner() {
        use selector::PseudoClass;
        let sels = selectors_of("div:not(.x) { x: 1 }");
        match &compound_of(&sels[0]).pseudos[0] {
            PseudoClass::Not(inner) => assert_eq!(inner.classes, vec!["x".to_string()]),
            other => panic!("expected Not, got {other:?}"),
        }
    }

    #[test]
    fn pseudo_functional_unknown_drops_rule() {
        // E16-M1: `:is`/`:has`/`:where` now parse (tested elsewhere). Only still-
        // unsupported functional pseudos (e.g. `:nth-last-child`, `:lang`) drop.
        for css in [":nth-last-child(1)", ":lang(x)"] {
            assert!(
                parse_stylesheet(&format!("{css} {{ x: 1 }}"))
                    .rules
                    .is_empty(),
                "{css} should drop"
            );
        }
    }

    #[test]
    fn pseudo_is_specificity_max() {
        // :is(#a, .b) → max((1,0,0),(0,1,0)) = (1,0,0).
        let sels = selectors_of(":is(#a, .b) { x: 1 }");
        assert_eq!(spec(&sels[0]), (1, 0, 0));
        // :is(#a, .b) on a tag adds onto the tag.
        let sels2 = selectors_of("p:is(#a, .b) { x: 1 }");
        assert_eq!(spec(&sels2[0]), (1, 0, 1));
    }

    #[test]
    fn pseudo_where_zero_specificity() {
        let sels = selectors_of(":where(#a) { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 0, 0));
        let sels2 = selectors_of("p:where(#a) { x: 1 }");
        assert_eq!(spec(&sels2[0]), (0, 0, 1)); // only the tag counts.
    }

    #[test]
    fn pseudo_has_specificity() {
        // :has() adds the max specificity among its relative selectors.
        let sels = selectors_of("div:has(.x) { x: 1 }");
        assert_eq!(spec(&sels[0]), (0, 1, 1)); // tag c=1 + .x b=1.
        let sels2 = selectors_of("div:has(#y) { x: 1 }");
        assert_eq!(spec(&sels2[0]), (1, 0, 1));
    }

    #[test]
    fn pseudo_is_parses_variants() {
        // these now parse (no longer dropped).
        for css in [
            ":is(p)",
            ":has(a)",
            ":where(.x)",
            ":is(:not(a), b)",
            "div:has(> p)",
        ] {
            assert!(
                !parse_stylesheet(&format!("{css} {{ x: 1 }}"))
                    .rules
                    .is_empty(),
                "{css} should parse"
            );
        }
    }

    // --- E7-M2: pseudo-elements ---

    #[test]
    fn pseudo_element_before_after() {
        use selector::PseudoElement;
        let sels = selectors_of("div::before { content: \"x\" }");
        assert_eq!(
            compound_of(&sels[0]).pseudo_element,
            Some(PseudoElement::Before)
        );
        assert_eq!(sels[0].pseudo_element(), Some(PseudoElement::Before));
        // tag c=1 + pseudo-element c=1 = (0,0,2).
        assert_eq!(spec(&sels[0]), (0, 0, 2));

        let sels2 = selectors_of("a::after { content: \"x\" }");
        assert_eq!(
            compound_of(&sels2[0]).pseudo_element,
            Some(PseudoElement::After)
        );
        assert_eq!(spec(&sels2[0]), (0, 0, 2));
    }

    #[test]
    fn pseudo_element_legacy_single_colon() {
        use selector::PseudoElement;
        let sels = selectors_of("div:before { content: \"x\" }");
        assert_eq!(
            compound_of(&sels[0]).pseudo_element,
            Some(PseudoElement::Before)
        );
        let sels2 = selectors_of("a:after { content: \"x\" }");
        assert_eq!(
            compound_of(&sels2[0]).pseudo_element,
            Some(PseudoElement::After)
        );
    }

    #[test]
    fn pseudo_element_specificity_variants() {
        use selector::PseudoElement;
        // .x::before → class b=1 + pseudo-element c=1.
        let sels = selectors_of(".x::before { content: \"x\" }");
        assert_eq!(spec(&sels[0]), (0, 1, 1));
        assert_eq!(sels[0].pseudo_element(), Some(PseudoElement::Before));
        // bare ::before → (0,0,1).
        let sels2 = selectors_of("::before { content: \"x\" }");
        assert_eq!(spec(&sels2[0]), (0, 0, 1));
    }

    #[test]
    fn pseudo_element_unsupported_drops_rule() {
        for css in [
            "p::first-line",
            "p::first-letter",
            "p::marker",
            "p::selection",
            "p::unknown",
        ] {
            assert!(
                parse_stylesheet(&format!("{css} {{ x: 1 }}"))
                    .rules
                    .is_empty(),
                "{css} should drop"
            );
        }
    }

    #[test]
    fn pseudo_element_trailing_simple_or_combinator_drops() {
        // A simple selector, combinator, or second pseudo-element after a
        // pseudo-element terminates the selector → rule dropped.
        for css in ["div::before.x", "div::before .x", "div::before::after"] {
            assert!(
                parse_stylesheet(&format!("{css} {{ x: 1 }}"))
                    .rules
                    .is_empty(),
                "{css} should drop"
            );
        }
    }

    // --- E7-M1: sibling combinators ---

    #[test]
    fn sibling_combinators() {
        let sels = selectors_of("h1 + p { x: 1 }");
        assert!(sels[0]
            .parts
            .iter()
            .any(|p| matches!(p, SelectorPart::Combinator(Combinator::NextSibling))));
        assert_eq!(spec(&sels[0]), (0, 0, 2));
        let sels2 = selectors_of("h1 ~ p { x: 1 }");
        assert!(sels2[0]
            .parts
            .iter()
            .any(|p| matches!(p, SelectorPart::Combinator(Combinator::SubsequentSibling))));
    }

    #[test]
    fn combinator_trailing_and_double_invalid() {
        assert!(parse_stylesheet("div + { x: 1 }").rules.is_empty());
        assert!(parse_stylesheet("a + + b { x: 1 }").rules.is_empty());
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
        assert_eq!(
            r.declarations[0].value.components,
            vec![Component::Number(0.0)]
        );
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
    fn decl_hsl_function_color() {
        let only_color = |css: &str| -> Rgba {
            let r = one_rule(css);
            match &r.declarations[0].value.components[..] {
                [Component::Color(c)] => *c,
                other => panic!("expected one color, got {other:?}"),
            }
        };
        assert_eq!(
            only_color("a { color: hsl(0, 100%, 50%); }"),
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
        assert_eq!(
            only_color("a { color: hsl(120, 100%, 50%); }"),
            Rgba {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }
        );
        assert_eq!(
            only_color("a { color: hsl(240, 100%, 50%); }"),
            Rgba {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            }
        );
        let hsla = only_color("a { color: hsla(0, 100%, 50%, 0.5); }");
        assert_eq!((hsla.r, hsla.g, hsla.b), (255, 0, 0));
        assert!(
            (hsla.a as i32 - 128).abs() <= 1,
            "alpha ≈128, got {}",
            hsla.a
        );
    }

    #[test]
    fn decl_hex_alpha_colors() {
        let only_color = |css: &str| -> Rgba {
            let r = one_rule(css);
            match &r.declarations[0].value.components[..] {
                [Component::Color(c)] => *c,
                other => panic!("expected one color, got {other:?}"),
            }
        };
        // #rrggbbaa and #rgba already parse via parse_hex.
        assert_eq!(only_color("a { color: #ff000080; }").a, 128);
        assert_eq!(only_color("a { color: #f008; }").a, 0x88);
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
    fn at_rule_block_captured_as_media() {
        // E13-M3: @media is now CAPTURED into media_blocks (not dropped). The
        // top-level `rules` still hold only the non-media `div` rule.
        let sheet = parse_stylesheet("@media screen { p { color: red } } div { color: blue }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "div");
        assert_eq!(sheet.media_blocks.len(), 1);
        let mb = &sheet.media_blocks[0];
        assert_eq!(mb.source_index, 0); // opened before any top-level rule
        assert_eq!(mb.query.conditions.len(), 1);
        assert_eq!(mb.query.conditions[0].media_type, MediaType::Screen);
        assert_eq!(mb.rules.len(), 1);
        assert_eq!(fmt_selector(&mb.rules[0].selectors[0]), "p");
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
        let sheet =
            parse_stylesheet("nav ul li a { text-decoration: none } .btn { display: block }");
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
        assert_eq!(
            r.declarations[0].value.components,
            vec![Component::Str("é".into())]
        );
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
            a:hover { color: red } /* E7-M1: kept as NeverMatch */
            @media print { body { color: black } }
        ";
        let sheet = parse_stylesheet(css);
        // body, (h1,h2), (#main .content p), a:hover → 4 top-level rules; the
        // @media print block is captured separately (E13-M3).
        assert_eq!(sheet.rules.len(), 4);
        assert_eq!(sheet.media_blocks.len(), 1);
        assert_eq!(sheet.media_blocks[0].source_index, 4); // after all 4 rules
        assert_eq!(
            sheet.media_blocks[0].query.conditions[0].media_type,
            MediaType::Print
        );
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

    // --- E6-M2: @font-face capture ---

    use model::{FontFaceStyle, FontSrc};

    #[test]
    fn font_face_basic_url() {
        let sheet = parse_stylesheet("@font-face { font-family: \"MyFont\"; src: url(\"a.ttf\") }");
        assert!(sheet.rules.is_empty(), "no qualified rules");
        assert_eq!(sheet.font_faces.len(), 1);
        let ff = &sheet.font_faces[0];
        assert_eq!(ff.family, "MyFont");
        assert_eq!(
            ff.src,
            vec![FontSrc::Url {
                url: "a.ttf".into(),
                format: None
            }]
        );
        assert_eq!(ff.weight, None);
        assert_eq!(ff.style, None);
    }

    #[test]
    fn font_face_format_hint() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: MyFont; src: url(\"a.ttf\") format(\"truetype\") }",
        );
        let ff = &sheet.font_faces[0];
        assert_eq!(
            ff.src,
            vec![FontSrc::Url {
                url: "a.ttf".into(),
                format: Some("truetype".into())
            }]
        );
    }

    #[test]
    fn font_face_multiple_src_entries() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: MyFont; \
             src: local(\"Arial\"), url(\"a.woff2\") format(\"woff2\") }",
        );
        let ff = &sheet.font_faces[0];
        assert_eq!(
            ff.src,
            vec![
                FontSrc::Local("Arial".into()),
                FontSrc::Url {
                    url: "a.woff2".into(),
                    format: Some("woff2".into())
                },
            ]
        );
    }

    #[test]
    fn font_face_weight_and_style() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: MyFont; src: url(\"a.ttf\"); \
             font-weight: 700; font-style: italic }",
        );
        let ff = &sheet.font_faces[0];
        assert_eq!(ff.weight, Some(700));
        assert_eq!(ff.style, Some(FontFaceStyle::Italic));
    }

    #[test]
    fn font_face_weight_keyword_bold() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: MyFont; src: url(\"a.ttf\"); font-weight: bold }",
        );
        assert_eq!(sheet.font_faces[0].weight, Some(700));
    }

    #[test]
    fn font_face_missing_family_dropped() {
        let sheet = parse_stylesheet("@font-face { src: url(\"a.ttf\") }");
        assert!(sheet.font_faces.is_empty(), "no family → dropped");
    }

    #[test]
    fn font_face_missing_src_dropped() {
        let sheet = parse_stylesheet("@font-face { font-family: \"MyFont\" }");
        assert!(sheet.font_faces.is_empty(), "no src → dropped");
    }

    #[test]
    fn font_face_alongside_rules_and_other_at_rules() {
        let sheet = parse_stylesheet(
            "@media screen { p { color: red } } \
             @font-face { font-family: \"MyFont\"; src: url(\"a.ttf\") } \
             @import \"x.css\"; \
             div { color: blue }",
        );
        // @import still skipped; div captured as a normal rule; @media captured
        // into media_blocks (E13-M3).
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "div");
        assert_eq!(sheet.media_blocks.len(), 1);
        assert_eq!(sheet.media_blocks[0].source_index, 0);
        // the @font-face captured separately.
        assert_eq!(sheet.font_faces.len(), 1);
        assert_eq!(sheet.font_faces[0].family, "MyFont");
    }

    // --- E13-M2: value re-parsing + custom-property case ---

    #[test]
    fn parse_component_values_color() {
        let comps = parse_component_values("red");
        assert_eq!(
            comps,
            vec![Component::Color(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })]
        );
    }

    #[test]
    fn parse_component_values_empty() {
        assert!(parse_component_values("").is_empty());
    }

    #[test]
    fn custom_prop_name_case_preserved() {
        let lower = one_rule("p { --c: red }");
        assert_eq!(lower.declarations[0].name, "--c");
        let upper = one_rule("p { --C: red }");
        assert_eq!(upper.declarations[0].name, "--C");
    }

    #[test]
    fn font_face_blockless_recovers() {
        // A blockless @font-face statement must resync and not eat the next rule.
        let sheet = parse_stylesheet("@font-face other; div { color: blue }");
        assert!(sheet.font_faces.is_empty());
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(fmt_selector(&sheet.rules[0].selectors[0]), "div");
    }

    // --- E13-M3: @media capture ---

    use model::{MediaFeature, Orientation};

    #[test]
    fn media_max_width_captured() {
        let sheet = parse_stylesheet("@media (max-width:500px){p{color:red}}");
        assert!(sheet.rules.is_empty());
        assert_eq!(sheet.media_blocks.len(), 1);
        let mb = &sheet.media_blocks[0];
        assert_eq!(mb.query.conditions.len(), 1);
        let c = &mb.query.conditions[0];
        assert!(!c.negated);
        assert_eq!(c.media_type, MediaType::All);
        assert_eq!(c.features, vec![MediaFeature::MaxWidth(500.0)]);
        assert_eq!(mb.rules.len(), 1);
        assert_eq!(fmt_selector(&mb.rules[0].selectors[0]), "p");
    }

    #[test]
    fn media_screen_and_min_width() {
        let sheet = parse_stylesheet("@media screen and (min-width:400px){a{x:1}}");
        let c = &sheet.media_blocks[0].query.conditions[0];
        assert_eq!(c.media_type, MediaType::Screen);
        assert_eq!(c.features, vec![MediaFeature::MinWidth(400.0)]);
    }

    #[test]
    fn media_comma_is_or() {
        let sheet = parse_stylesheet("@media (max-width:500px), (min-height:300px){p{x:1}}");
        let q = &sheet.media_blocks[0].query;
        assert_eq!(q.conditions.len(), 2);
        assert_eq!(
            q.conditions[0].features,
            vec![MediaFeature::MaxWidth(500.0)]
        );
        assert_eq!(
            q.conditions[1].features,
            vec![MediaFeature::MinHeight(300.0)]
        );
    }

    #[test]
    fn media_not_screen() {
        let sheet = parse_stylesheet("@media not screen{p{x:1}}");
        let c = &sheet.media_blocks[0].query.conditions[0];
        assert!(c.negated);
        assert_eq!(c.media_type, MediaType::Screen);
        assert!(c.features.is_empty());
    }

    #[test]
    fn media_orientation_portrait() {
        let sheet = parse_stylesheet("@media (orientation:portrait){p{x:1}}");
        let c = &sheet.media_blocks[0].query.conditions[0];
        assert_eq!(
            c.features,
            vec![MediaFeature::Orientation(Orientation::Portrait)]
        );
    }

    #[test]
    fn media_malformed_no_panic() {
        // Malformed prelude must not panic; the block's rules are still captured.
        let sheet = parse_stylesheet("@media ((( {p{}}");
        assert_eq!(sheet.media_blocks.len(), 1);
        // unterminated paren → an Unknown feature (never matches).
        let c = &sheet.media_blocks[0].query.conditions[0];
        assert!(c.features.contains(&MediaFeature::Unknown));
    }

    #[test]
    fn media_unknown_feature() {
        let sheet = parse_stylesheet("@media (min-resolution:2dppx){p{x:1}}");
        let c = &sheet.media_blocks[0].query.conditions[0];
        assert_eq!(c.features, vec![MediaFeature::Unknown]);
    }
}
