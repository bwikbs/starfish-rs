//! The lenient parser: token stream → [`Stylesheet`]. Never panics; recovers
//! from malformed input the way CSS error handling does.

use crate::color;
use crate::model::{
    Component, Declaration, FontFaceRule, FontFaceStyle, FontSrc, Keyframe, KeyframesRule,
    LayerBlock, MediaBlock, MediaCondition, MediaFeature, MediaQuery, MediaType, Orientation, Rule,
    Stylesheet, SupportsBlock, SupportsCondition, Value,
};
use crate::selector::{
    AttrOp, AttrSelector, Combinator, Compound, Nth, PseudoClass, PseudoElement, RelativeSelector,
    Selector, SelectorBuilder,
};
use crate::tokenizer::{Token, Tokenizer};

/// A token paired with the byte span `[start, end)` it covers in the source.
struct Spanned {
    tok: Token,
    start: usize,
    end: usize,
}

/// Tokenize `s` and classify it into a list of [`Component`]s, reusing the same
/// value-building logic as declaration values. Used to re-parse value strings
/// (e.g. `var()` fallbacks). Whitespace-trimmed; no `!important` handling.
pub(crate) fn parse_component_values(s: &str) -> Vec<Component> {
    let mut tz = Tokenizer::new(s);
    let mut toks: Vec<Spanned> = Vec::new();
    loop {
        let start = tz.pos();
        let tok = tz.next_token();
        let end = tz.pos();
        let eof = tok == Token::Eof;
        toks.push(Spanned { tok, start, end });
        if eof {
            break;
        }
    }
    // Exclude the trailing Eof token (its empty span would yield a `Raw("")`).
    let hi = toks.len().saturating_sub(1);
    let p = Parser {
        css: s,
        toks,
        pos: 0,
    };
    p.classify_components(0, hi)
}

pub(crate) fn parse(css: &str) -> Stylesheet {
    // Tokenize fully up front, recording spans so we can slice `raw` later.
    let mut tz = Tokenizer::new(css);
    let mut toks: Vec<Spanned> = Vec::new();
    loop {
        let start = tz.pos();
        let tok = tz.next_token();
        let end = tz.pos();
        let eof = tok == Token::Eof;
        toks.push(Spanned { tok, start, end });
        if eof {
            break;
        }
    }

    let mut p = Parser { css, toks, pos: 0 };
    let mut rules = Vec::new();
    let mut font_faces = Vec::new();
    let mut media_blocks = Vec::new();
    let mut keyframes = Vec::new();
    let mut supports_blocks = Vec::new();
    let mut layer_blocks = Vec::new();
    let mut layer_order = Vec::new();
    p.parse_rule_list(
        &mut rules,
        &mut font_faces,
        &mut media_blocks,
        &mut keyframes,
        &mut supports_blocks,
        &mut layer_blocks,
        &mut layer_order,
    );
    Stylesheet {
        rules,
        font_faces,
        media_blocks,
        keyframes,
        supports_blocks,
        layer_blocks,
        layer_order,
    }
}

struct Parser<'a> {
    css: &'a str,
    toks: Vec<Spanned>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos].tok
    }

    fn bump(&mut self) -> &Spanned {
        let i = self.pos;
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        &self.toks[i]
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Token::Whitespace) {
            self.bump();
        }
    }

    // --- §4.1 top level ---

    #[allow(clippy::too_many_arguments)]
    fn parse_rule_list(
        &mut self,
        out: &mut Vec<Rule>,
        font_faces: &mut Vec<FontFaceRule>,
        media_blocks: &mut Vec<MediaBlock>,
        keyframes: &mut Vec<KeyframesRule>,
        supports_blocks: &mut Vec<SupportsBlock>,
        layer_blocks: &mut Vec<LayerBlock>,
        layer_order: &mut Vec<String>,
    ) {
        // One ordinal counter across ALL captured at-blocks (E24-M2): the
        // cascade's k-way merge uses it to order blocks sharing a source_index.
        let mut at_ordinal = 0usize;
        loop {
            self.skip_whitespace();
            match self.peek() {
                Token::Eof => return,
                Token::AtKeyword(name) => {
                    if name.eq_ignore_ascii_case("font-face") {
                        if let Some(ff) = self.parse_font_face() {
                            font_faces.push(ff);
                        }
                    } else if name.eq_ignore_ascii_case("media") {
                        // source_index = number of top-level rules parsed so far.
                        if let Some(mb) = self.parse_media(out.len(), at_ordinal) {
                            media_blocks.push(mb);
                            at_ordinal += 1;
                        }
                    } else if name.eq_ignore_ascii_case("keyframes") {
                        if let Some(kf) = self.parse_keyframes() {
                            keyframes.push(kf);
                        }
                    } else if name.eq_ignore_ascii_case("supports") {
                        if let Some(sb) = self.parse_supports(out.len(), at_ordinal) {
                            supports_blocks.push(sb);
                            at_ordinal += 1;
                        }
                    } else if name.eq_ignore_ascii_case("layer") {
                        if let Some(lb) = self.parse_layer(out.len(), at_ordinal, layer_order) {
                            layer_blocks.push(lb);
                            at_ordinal += 1;
                        }
                    } else {
                        self.skip_at_rule();
                    }
                }
                // Stray closing/structural tokens at top level → ignore.
                Token::RightBrace | Token::Semicolon => {
                    self.bump();
                }
                _ => {
                    if let Some(rule) = self.parse_qualified_rule() {
                        out.push(rule);
                    }
                }
            }
        }
    }

    // --- §4.4 at-rules: skip statement (`;`) or block (`{…}`) ---

    fn skip_at_rule(&mut self) {
        self.bump(); // the AtKeyword
        loop {
            match self.peek() {
                Token::Eof => return,
                Token::Semicolon => {
                    self.bump();
                    return;
                }
                Token::LeftBrace => {
                    self.skip_block();
                    return;
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Consume a balanced `{ … }` block (cursor must be on `{`).
    fn skip_block(&mut self) {
        self.bump(); // `{`
        let mut depth = 1;
        while depth > 0 {
            match self.peek() {
                Token::Eof => return,
                Token::LeftBrace => depth += 1,
                Token::RightBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    // --- E6-M2: @font-face capture ---

    /// Parse a `@font-face` rule. The cursor is on the `@font-face` keyword.
    /// Reuses `parse_declaration_block` for the `{…}` body and folds the
    /// descriptors into a `FontFaceRule`. Returns `None` (and resyncs the
    /// stream) for a malformed/blockless rule or one missing family/src.
    fn parse_font_face(&mut self) -> Option<FontFaceRule> {
        self.bump(); // @font-face
        self.skip_whitespace();
        if !matches!(self.peek(), Token::LeftBrace) {
            // statement form / malformed: behave like a skipped at-rule.
            self.recover_at_rule_tail();
            return None;
        }
        let decls = self.parse_declaration_block(); // leaves cursor past `}`
        fold_font_face(&decls)
    }

    /// Consume to (and past) the next top-level `;`, or skip a balanced `{…}`
    /// block, mirroring `skip_at_rule` after the keyword. Used when a
    /// `@font-face` is not followed by a block.
    fn recover_at_rule_tail(&mut self) {
        loop {
            match self.peek() {
                Token::Eof => return,
                Token::Semicolon => {
                    self.bump();
                    return;
                }
                Token::LeftBrace => {
                    self.skip_block();
                    return;
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    // --- E13-M3: @media capture ---

    /// Parse a `@media` block. The cursor is on the `@media` keyword. Collects
    /// the prelude tokens up to the `{`, parses them with the never-panicking
    /// `parse_media_query` mini-parser, then parses the inner qualified rules up
    /// to the matching `}`. Nested at-rules inside the block are skipped.
    /// Returns `None` (recovering) when there is no block before EOF.
    fn parse_media(&mut self, source_index: usize, at_ordinal: usize) -> Option<MediaBlock> {
        self.bump(); // @media
                     // Collect prelude token indices up to the first top-level `{`.
        let prelude_start = self.pos;
        loop {
            match self.peek() {
                Token::Eof => return None, // malformed: no block → recover.
                Token::LeftBrace => break,
                _ => {
                    self.bump();
                }
            }
        }
        let prelude_end = self.pos; // index of the `{`
        let prelude: Vec<Token> = self.toks[prelude_start..prelude_end]
            .iter()
            .map(|s| s.tok.clone())
            .collect();
        let query = parse_media_query_tokens(&prelude);
        let rules = self.parse_block_rules();

        Some(MediaBlock {
            query,
            rules,
            source_index,
            at_ordinal,
        })
    }

    /// Parse the qualified rules inside an at-rule's `{ … }` block (cursor on
    /// the `{`; leaves it past the matching `}`). Nested at-rules inside the
    /// block are out of scope: skipped. Shared by `@media`/`@supports`/`@layer`.
    fn parse_block_rules(&mut self) -> Vec<Rule> {
        self.bump(); // `{`
        let mut rules = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Token::Eof => break, // unbalanced at EOF → close.
                Token::RightBrace => {
                    self.bump();
                    break;
                }
                Token::Semicolon => {
                    self.bump();
                }
                Token::AtKeyword(_) => self.skip_at_rule(),
                _ => {
                    if let Some(rule) = self.parse_qualified_rule() {
                        rules.push(rule);
                    }
                }
            }
        }
        rules
    }

    // --- E24-M2: @supports capture ---

    /// Parse a `@supports` block. The cursor is on the `@supports` keyword.
    /// Collects the prelude tokens up to the `{`, parses them with the
    /// never-panicking condition mini-parser (unrecognized → `Unknown`), then
    /// parses the inner qualified rules. `None` when there is no block.
    fn parse_supports(&mut self, source_index: usize, at_ordinal: usize) -> Option<SupportsBlock> {
        self.bump(); // @supports
        let prelude_start = self.pos;
        loop {
            match self.peek() {
                Token::Eof => return None, // malformed: no block → recover.
                Token::LeftBrace => break,
                _ => {
                    self.bump();
                }
            }
        }
        let prelude_end = self.pos; // index of the `{`
        let condition = self.parse_supports_condition(prelude_start, prelude_end);
        let rules = self.parse_block_rules();
        Some(SupportsBlock {
            condition,
            rules,
            source_index,
            at_ordinal,
        })
    }

    /// Trim whitespace tokens off both ends of `[lo, hi)`.
    fn trim_ws_range(&self, mut lo: usize, mut hi: usize) -> (usize, usize) {
        while lo < hi && matches!(self.toks[lo].tok, Token::Whitespace) {
            lo += 1;
        }
        while hi > lo && matches!(self.toks[hi - 1].tok, Token::Whitespace) {
            hi -= 1;
        }
        (lo, hi)
    }

    /// Parse a `@supports` condition from tokens `[lo, hi)` (recursive descent):
    ///   cond      := `not` in-parens
    ///              | in-parens (`and` in-parens)*
    ///              | in-parens (`or` in-parens)*
    ///   in-parens := `(` cond `)` | `(` ident `:` value `)`
    /// Mixed `and`/`or` without parens, function tokens (`selector(…)`), and any
    /// other unrecognized shape → `Unknown` (never matches). NEVER panics.
    fn parse_supports_condition(&self, lo: usize, hi: usize) -> SupportsCondition {
        let (lo, hi) = self.trim_ws_range(lo, hi);
        if lo >= hi {
            return SupportsCondition::Unknown;
        }
        // `not <in-parens>` — must consume the whole range.
        if let Token::Ident(id) = &self.toks[lo].tok {
            if id.eq_ignore_ascii_case("not") {
                let (j, _) = self.trim_ws_range(lo + 1, hi);
                return match self.parse_supports_in_parens(j, hi) {
                    Some((c, next)) if self.trim_ws_range(next, hi).0 >= hi => {
                        SupportsCondition::Not(Box::new(c))
                    }
                    _ => SupportsCondition::Unknown,
                };
            }
        }
        // <in-parens> joined by a homogeneous run of `and` XOR `or`.
        let mut parts = Vec::new();
        let mut is_and: Option<bool> = None;
        let mut i = lo;
        loop {
            let Some((c, next)) = self.parse_supports_in_parens(i, hi) else {
                return SupportsCondition::Unknown;
            };
            parts.push(c);
            let (j, _) = self.trim_ws_range(next, hi);
            if j >= hi {
                break;
            }
            let this_and = match &self.toks[j].tok {
                Token::Ident(id) if id.eq_ignore_ascii_case("and") => true,
                Token::Ident(id) if id.eq_ignore_ascii_case("or") => false,
                _ => return SupportsCondition::Unknown,
            };
            match is_and {
                None => is_and = Some(this_and),
                // mixed `and`/`or` without parens → Unknown.
                Some(prev) if prev != this_and => return SupportsCondition::Unknown,
                _ => {}
            }
            let (k, _) = self.trim_ws_range(j + 1, hi);
            i = k;
        }
        match (parts.len(), is_and) {
            (1, _) => parts.into_iter().next().unwrap(),
            (_, Some(true)) => SupportsCondition::And(parts),
            _ => SupportsCondition::Or(parts),
        }
    }

    /// Parse one parenthesized `@supports` term starting at token `i`: returns
    /// the condition and the index just past the closing `)`. `None` when `i`
    /// is not an open paren with a matching close (e.g. a `Function(` token).
    /// Unparseable parenthesized content yields `Unknown` (general-enclosed),
    /// NOT `None`, so an `or` sibling can still match.
    fn parse_supports_in_parens(&self, i: usize, hi: usize) -> Option<(SupportsCondition, usize)> {
        if i >= hi || !matches!(self.toks[i].tok, Token::LeftParen) {
            return None;
        }
        let close = self.find_paren_close(i, hi)?;
        let (lo, ihi) = self.trim_ws_range(i + 1, close);
        // `( ident : value )` → Decl.
        if let Some(Token::Ident(name)) = self.toks.get(lo).map(|s| &s.tok) {
            let (j, _) = self.trim_ws_range(lo + 1, ihi);
            if j < ihi && matches!(self.toks[j].tok, Token::Colon) {
                // Custom properties keep case; others lowercased (like decls).
                let name = if name.starts_with("--") {
                    name.clone()
                } else {
                    name.to_ascii_lowercase()
                };
                // build_value strips a trailing `!important` into the flag.
                let (value, important) = self.build_value(j + 1, ihi);
                if value.raw.is_empty() && value.components.is_empty() {
                    // `(display:)` — invalid → general-enclosed, never matches.
                    return Some((SupportsCondition::Unknown, close + 1));
                }
                return Some((
                    SupportsCondition::Decl(Declaration {
                        name,
                        value,
                        important,
                    }),
                    close + 1,
                ));
            }
        }
        // `( cond )` — recurse (garbage inside resolves to Unknown).
        Some((self.parse_supports_condition(lo, ihi), close + 1))
    }

    // --- E24-M2: @layer capture ---

    /// Parse a `@layer` rule. The cursor is on the `@layer` keyword. Statement
    /// form (`@layer a, b;`) registers each name in `layer_order` on first
    /// appearance and returns `None`. Block form with exactly one plain name
    /// (`@layer a { … }`) registers the name and captures a [`LayerBlock`].
    /// Anonymous / multi-name / dotted (nested) blocks are skipped (deferred).
    fn parse_layer(
        &mut self,
        source_index: usize,
        at_ordinal: usize,
        layer_order: &mut Vec<String>,
    ) -> Option<LayerBlock> {
        self.bump(); // @layer
        let mut names: Vec<String> = Vec::new();
        let mut malformed = false; // dotted name / unexpected prelude token
        loop {
            self.skip_whitespace();
            match self.peek().clone() {
                Token::Eof => return None,
                Token::Semicolon => {
                    self.bump();
                    // Ordering statement: register on first appearance.
                    if !malformed {
                        for n in names {
                            if !layer_order.contains(&n) {
                                layer_order.push(n);
                            }
                        }
                    }
                    return None;
                }
                Token::LeftBrace => {
                    if malformed || names.len() != 1 {
                        // anonymous / multi-name / dotted block → deferred.
                        self.skip_block();
                        return None;
                    }
                    let name = names.pop().unwrap();
                    if !layer_order.contains(&name) {
                        layer_order.push(name.clone());
                    }
                    let rules = self.parse_block_rules();
                    return Some(LayerBlock {
                        name,
                        rules,
                        source_index,
                        at_ordinal,
                    });
                }
                Token::Ident(n) => {
                    self.bump();
                    names.push(n);
                }
                Token::Comma => {
                    self.bump();
                }
                Token::Delim('.') => {
                    // dotted (nested) layer name — deferred.
                    self.bump();
                    malformed = true;
                }
                _ => {
                    // Unexpected token → behave like a skipped at-rule.
                    self.recover_at_rule_tail();
                    return None;
                }
            }
        }
    }

    // --- E17-M1: @keyframes capture ---

    /// Parse a `@keyframes` rule. The cursor is on the `@keyframes` keyword.
    /// Reads the animation name (an Ident, case preserved, or a quoted Str), then
    /// the `{ … }` body: a series of `<selector-list> { <declarations> }` keyframe
    /// blocks. A multi-selector block expands to one [`Keyframe`] per offset.
    /// Returns `None` (recovering) when there is no block before EOF.
    fn parse_keyframes(&mut self) -> Option<KeyframesRule> {
        self.bump(); // @keyframes
        self.skip_whitespace();
        // Name: an Ident (keep-case) or a quoted string (unquoted).
        let name = match self.peek().clone() {
            Token::Ident(n) => {
                self.bump();
                n
            }
            Token::Str(s) => {
                self.bump();
                s
            }
            _ => {
                // No name → behave like a skipped at-rule.
                self.recover_at_rule_tail();
                return None;
            }
        };
        self.skip_whitespace();
        if !matches!(self.peek(), Token::LeftBrace) {
            self.recover_at_rule_tail();
            return None;
        }
        self.bump(); // `{`

        let mut keyframes = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Token::Eof => break, // unbalanced at EOF → close.
                Token::RightBrace => {
                    self.bump();
                    break;
                }
                Token::Semicolon => {
                    self.bump();
                }
                _ => {
                    // Collect selector tokens up to the inner `{`.
                    let sel_start = self.pos;
                    loop {
                        match self.peek() {
                            Token::Eof | Token::RightBrace => break,
                            Token::LeftBrace => break,
                            _ => {
                                self.bump();
                            }
                        }
                    }
                    if !matches!(self.peek(), Token::LeftBrace) {
                        // Malformed keyframe block (no body) → stop.
                        break;
                    }
                    let offsets = self.parse_keyframe_selectors(sel_start, self.pos);
                    let decls = self.parse_declaration_block(); // past `}`
                    for off in offsets {
                        keyframes.push(Keyframe {
                            offset: off,
                            declarations: decls.clone(),
                        });
                    }
                }
            }
        }

        Some(KeyframesRule { name, keyframes })
    }

    /// Parse a keyframe selector list (the tokens `[start, end)` before the
    /// inner `{`) into normalized offsets in `0.0..=1.0`. Splits on `Comma`;
    /// `Percentage(p)`→`p/100`, `from`→0, `to`→1; clamps to `[0,1]`; garbage
    /// segments are dropped.
    fn parse_keyframe_selectors(&self, start: usize, end: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for (lo, hi) in self.split_top_level_commas(start, end) {
            // The first significant token in the segment.
            let tok = (lo..hi)
                .map(|k| &self.toks[k].tok)
                .find(|t| !matches!(t, Token::Whitespace));
            let off = match tok {
                Some(Token::Percentage(p)) => *p / 100.0,
                Some(Token::Number(n)) if *n == 0.0 => 0.0,
                Some(Token::Ident(id)) if id.eq_ignore_ascii_case("from") => 0.0,
                Some(Token::Ident(id)) if id.eq_ignore_ascii_case("to") => 1.0,
                _ => continue, // garbage → drop.
            };
            out.push(off.clamp(0.0, 1.0));
        }
        out
    }

    // --- §4.2 qualified rule ---

    fn parse_qualified_rule(&mut self) -> Option<Rule> {
        // Collect prelude token indices up to the first top-level `{`.
        let prelude_start = self.pos;
        let mut brace_depth = 0; // for stray parens/brackets in prelude
        loop {
            match self.peek() {
                Token::Eof => {
                    // malformed trailing rule with no block → discard.
                    return None;
                }
                Token::LeftBrace if brace_depth == 0 => break,
                // `Function(name)` consumed its own `(`, so it opens a paren too.
                Token::LeftParen | Token::LeftBracket | Token::Function(_) => {
                    brace_depth += 1;
                    self.bump();
                }
                Token::RightParen | Token::RightBracket => {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    }
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
        let prelude_end = self.pos; // index of the `{`

        // Parse the declaration block regardless, so the stream resyncs.
        let declarations = self.parse_declaration_block();

        // Now parse the prelude selectors.
        match self.parse_selector_list(prelude_start, prelude_end) {
            Some(selectors) if !selectors.is_empty() => Some(Rule {
                selectors,
                declarations,
            }),
            _ => None, // invalid prelude → drop rule (block already consumed).
        }
    }

    // --- §4.3 selector list ---

    /// Parse tokens in `[start, end)` (the prelude) as a comma-separated
    /// selector list. `None` if any selector is invalid (per spec: one bad
    /// selector invalidates the whole list).
    fn parse_selector_list(&self, start: usize, end: usize) -> Option<Vec<Selector>> {
        // Split on TOP-LEVEL commas only — a comma nested inside `(...)`/`[...]`
        // (e.g. `:is(a, b)` / `[x="a,b"]`) is NOT a list separator (E16-M1).
        let mut selectors = Vec::new();
        for (seg_lo, seg_hi) in self.split_top_level_commas(start, end) {
            let sel = self.parse_complex_selector(seg_lo, seg_hi)?;
            selectors.push(sel);
        }
        if selectors.is_empty() {
            None
        } else {
            Some(selectors)
        }
    }

    fn parse_complex_selector(&self, start: usize, end: usize) -> Option<Selector> {
        // Trim leading/trailing whitespace so they don't become dangling
        // descendant combinators.
        let mut start = start;
        let mut end = end;
        while start < end && matches!(self.toks[start].tok, Token::Whitespace) {
            start += 1;
        }
        while end > start && matches!(self.toks[end - 1].tok, Token::Whitespace) {
            end -= 1;
        }

        let mut b = SelectorBuilder::new();
        let mut i = start;
        while i < end {
            match &self.toks[i].tok {
                Token::Whitespace => b.push_combinator(Combinator::Descendant),
                Token::Ident(name) => b.set_tag(name.to_ascii_lowercase()),
                Token::Hash(text) => b.push_id(text.clone()),
                Token::Delim('*') => b.set_universal(),
                Token::Delim('>') => b.push_combinator(Combinator::Child),
                Token::Delim('+') => b.push_combinator(Combinator::NextSibling),
                Token::Delim('~') => b.push_combinator(Combinator::SubsequentSibling),
                Token::Delim('.') => {
                    // `.` then ident → class
                    if let Some(Token::Ident(name)) = self.toks.get(i + 1).map(|s| &s.tok) {
                        b.push_class(name.clone());
                        i += 1;
                    } else {
                        b.invalidate();
                    }
                }
                Token::LeftBracket => {
                    // Find the matching `]` (no nesting inside attr selectors).
                    let close = self.find_bracket_close(i, end);
                    match close.and_then(|c| self.parse_attr(i + 1, c)) {
                        Some(attr) => b.push_attr(attr),
                        None => b.invalidate(),
                    }
                    // Advance past `]` (or to `end` if unterminated).
                    i = close.unwrap_or(end);
                }
                Token::Colon
                    if matches!(self.toks.get(i + 1).map(|s| &s.tok), Some(Token::Colon)) =>
                {
                    // `::name` — modern pseudo-element (two adjacent colons).
                    match self.toks.get(i + 2).map(|s| &s.tok) {
                        Some(Token::Ident(name)) => {
                            match pseudo_element(name) {
                                Some(pe) => b.set_pseudo_element(pe),
                                // ::first-line / ::first-letter / unknown → drop.
                                None => b.invalidate(),
                            }
                            i += 2; // consume the 2nd colon + the ident
                        }
                        // `::` then non-ident, or functional `::foo()` → invalid.
                        _ => b.invalidate(),
                    }
                }
                Token::Colon => {
                    match self.toks.get(i + 1).map(|s| &s.tok) {
                        Some(Token::Ident(name)) => {
                            // Legacy single-colon pseudo-element `:before`/`:after`.
                            match pseudo_element(name) {
                                Some(pe) => b.set_pseudo_element(pe),
                                None => b.push_pseudo(bare_pseudo(name)),
                            }
                            i += 1;
                        }
                        Some(Token::Function(name)) => {
                            let fname = name.clone();
                            let close = self.find_paren_close(i + 1, end);
                            match close {
                                Some(c) => {
                                    match self.parse_functional_pseudo(&fname, i + 2, c) {
                                        Some(p) => b.push_pseudo(p),
                                        None => b.invalidate(),
                                    }
                                    i = c;
                                }
                                None => b.invalidate(),
                            }
                        }
                        // `::…` pseudo-element, or `:` at end → invalidate.
                        _ => b.invalidate(),
                    }
                }
                // Anything else → invalid.
                _ => b.invalidate(),
            }
            i += 1;
        }
        b.finish()
    }

    /// Index of the `]` matching the `[` at `open`, searching within `[.., end)`.
    /// Attribute selectors don't nest, so the first `]` wins. `None` if missing.
    fn find_bracket_close(&self, open: usize, end: usize) -> Option<usize> {
        let mut j = open + 1;
        while j < end {
            if matches!(self.toks[j].tok, Token::RightBracket) {
                return Some(j);
            }
            j += 1;
        }
        None
    }

    /// Index of the `)` matching the `Function(`/`LeftParen` at `open`,
    /// balanced, within `[.., end)`. `None` if missing.
    fn find_paren_close(&self, open: usize, end: usize) -> Option<usize> {
        let mut depth = 1;
        let mut j = open + 1;
        while j < end {
            match &self.toks[j].tok {
                Token::LeftParen | Token::Function(_) => depth += 1,
                Token::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        None
    }

    /// Parse an attribute selector body — the tokens strictly between `[` and
    /// `]`, i.e. `[lo, hi)`. Returns `None` on any malformed input.
    fn parse_attr(&self, lo: usize, hi: usize) -> Option<AttrSelector> {
        // Significant (non-whitespace) tokens in order.
        let toks: Vec<&Token> = (lo..hi)
            .map(|k| &self.toks[k].tok)
            .filter(|t| !matches!(t, Token::Whitespace))
            .collect();
        let mut it = toks.iter().copied();

        // name
        let name = match it.next() {
            Some(Token::Ident(n)) => n.to_ascii_lowercase(),
            _ => return None,
        };

        // optional operator
        let op_tok = it.next();
        let Some(op_first) = op_tok else {
            // `[name]` → Exists.
            return Some(AttrSelector {
                name,
                op: AttrOp::Exists,
                value: None,
                case_insensitive: false,
            });
        };

        let op = match op_first {
            Token::Delim('=') => AttrOp::Equals,
            Token::Delim(c @ ('~' | '|' | '^' | '$' | '*')) => {
                // must be followed immediately by `=`.
                if !matches!(it.next(), Some(Token::Delim('='))) {
                    return None;
                }
                match c {
                    '~' => AttrOp::Includes,
                    '|' => AttrOp::DashMatch,
                    '^' => AttrOp::Prefix,
                    '$' => AttrOp::Suffix,
                    '*' => AttrOp::Substring,
                    _ => unreachable!(),
                }
            }
            _ => return None,
        };

        // value: Str or Ident.
        let value = match it.next() {
            Some(Token::Str(s)) => s.clone(),
            Some(Token::Ident(s)) => s.clone(),
            _ => return None,
        };

        // optional case-insensitive flag `i`/`I`; nothing else allowed after.
        let case_insensitive = match it.next() {
            None => false,
            Some(Token::Ident(f)) if f.eq_ignore_ascii_case("i") => {
                if it.next().is_some() {
                    return None;
                }
                true
            }
            _ => return None,
        };

        Some(AttrSelector {
            name,
            op,
            value: Some(value),
            case_insensitive,
        })
    }

    /// Parse a functional pseudo-class `name(args)`; `args` are tokens
    /// `[lo, hi)` strictly inside the parens. `None` → invalidate the selector.
    fn parse_functional_pseudo(&self, name: &str, lo: usize, hi: usize) -> Option<PseudoClass> {
        if name.eq_ignore_ascii_case("nth-child") {
            self.parse_nth(lo, hi).map(PseudoClass::NthChild)
        } else if name.eq_ignore_ascii_case("nth-of-type") {
            self.parse_nth(lo, hi).map(PseudoClass::NthOfType)
        } else if name.eq_ignore_ascii_case("not") {
            self.parse_simple_compound(lo, hi)
                .map(|c| PseudoClass::Not(Box::new(c)))
        } else if name.eq_ignore_ascii_case("is") {
            Some(PseudoClass::Is(self.parse_forgiving_selector_list(lo, hi)))
        } else if name.eq_ignore_ascii_case("where") {
            Some(PseudoClass::Where(
                self.parse_forgiving_selector_list(lo, hi),
            ))
        } else if name.eq_ignore_ascii_case("has") {
            Some(PseudoClass::Has(self.parse_relative_selector_list(lo, hi)))
        } else {
            // any other unknown functional pseudo → invalidate.
            None
        }
    }

    /// Split the token range `[lo, hi)` on TOP-LEVEL commas (parens/functions
    /// raise the depth so nested `,` inside e.g. `:not(a, b)` don't split here),
    /// parse each segment as a complex selector, and DROP segments that fail to
    /// parse (forgiving selector list, E16-M1). Used by `:is()`/`:where()`.
    fn parse_forgiving_selector_list(&self, lo: usize, hi: usize) -> Vec<Selector> {
        let mut out = Vec::new();
        for (seg_lo, seg_hi) in self.split_top_level_commas(lo, hi) {
            if let Some(sel) = self.parse_complex_selector(seg_lo, seg_hi) {
                out.push(sel);
            }
        }
        out
    }

    /// Parse a `:has()` relative-selector list from `[lo, hi)`: same top-level
    /// comma split, each segment a [`RelativeSelector`] (optional leading
    /// combinator + complex selector). Failures are dropped (forgiving).
    fn parse_relative_selector_list(&self, lo: usize, hi: usize) -> Vec<RelativeSelector> {
        let mut out = Vec::new();
        for (seg_lo, seg_hi) in self.split_top_level_commas(lo, hi) {
            if let Some(rel) = self.parse_relative(seg_lo, seg_hi) {
                out.push(rel);
            }
        }
        out
    }

    /// Parse one relative selector: skip leading whitespace, read an optional
    /// leading combinator (`>`/`+`/`~`, else Descendant), then parse the rest as
    /// a complex selector. `None` if the complex selector is invalid.
    fn parse_relative(&self, lo: usize, hi: usize) -> Option<RelativeSelector> {
        let mut i = lo;
        while i < hi && matches!(self.toks[i].tok, Token::Whitespace) {
            i += 1;
        }
        let combinator = match self.toks.get(i).map(|s| &s.tok) {
            Some(Token::Delim('>')) => {
                i += 1;
                Combinator::Child
            }
            Some(Token::Delim('+')) => {
                i += 1;
                Combinator::NextSibling
            }
            Some(Token::Delim('~')) => {
                i += 1;
                Combinator::SubsequentSibling
            }
            _ => Combinator::Descendant,
        };
        let selector = self.parse_complex_selector(i, hi)?;
        Some(RelativeSelector {
            combinator,
            selector,
        })
    }

    /// Yield `(start, end)` token sub-ranges of `[lo, hi)` split on TOP-LEVEL
    /// commas. Paren/bracket/function depth is tracked so commas nested inside
    /// `(...)`/`[...]` don't split (E16-M1).
    fn split_top_level_commas(&self, lo: usize, hi: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut seg_start = lo;
        let mut i = lo;
        while i < hi {
            match &self.toks[i].tok {
                Token::LeftParen | Token::Function(_) | Token::LeftBracket => depth += 1,
                // Defensive saturating decrement (unbalanced input never panics).
                Token::RightParen | Token::RightBracket => depth = depth.saturating_sub(1),
                Token::Comma if depth == 0 => {
                    out.push((seg_start, i));
                    seg_start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        out.push((seg_start, hi));
        out
    }

    /// Parse an `An+B` micro-grammar from the token range `[lo, hi)`.
    fn parse_nth(&self, lo: usize, hi: usize) -> Option<Nth> {
        let toks: Vec<&Token> = (lo..hi)
            .map(|k| &self.toks[k].tok)
            .filter(|t| !matches!(t, Token::Whitespace))
            .collect();

        // keyword forms
        if toks.len() == 1 {
            if let Token::Ident(id) = toks[0] {
                if id.eq_ignore_ascii_case("odd") {
                    return Some(Nth { a: 2, b: 1 });
                }
                if id.eq_ignore_ascii_case("even") {
                    return Some(Nth { a: 2, b: 0 });
                }
                if id.eq_ignore_ascii_case("n") {
                    return Some(Nth { a: 1, b: 0 });
                }
                if id.eq_ignore_ascii_case("-n") {
                    return Some(Nth { a: -1, b: 0 });
                }
            }
            // plain integer `<int>` → {0, int}
            if let Token::Number(n) = toks[0] {
                let b = int_of(*n)?;
                return Some(Nth { a: 0, b });
            }
        }

        // Forms with an `n` term. The first token carries `a` (and, because of
        // the tokenizer's ident/dimension rules, sometimes the whole `±b` tail
        // is glued onto it). Tokenization realities:
        //   `n`      → Ident("n")
        //   `-n`     → Ident("-n")
        //   `2n`     → Dimension{2,"n"}
        //   `2n+1`   → Dimension{2,"n"}, Number(1)   (`+1` is a signed number)
        //   `2n-1`   → Dimension{2,"n-1"}            (`-1` glues onto the unit)
        //   `-n+2`   → Ident("-n"), Number(2)
        //   `-n-1`   → Ident("-n-1")
        // So: extract `a` and an optional inline `b` from the first token, then
        // an optional trailing `±<int>` from the remaining tokens.
        let (a, inline_b, rest) = match toks.first()? {
            Token::Ident(id) => {
                let (a, b) = parse_n_ident(id)?;
                (a, b, &toks[1..])
            }
            Token::Dimension { value, unit } => {
                let a = int_of(*value)?;
                let b = parse_n_unit(unit)?;
                (a, b, &toks[1..])
            }
            _ => return None,
        };

        let tail_b = match rest {
            [] => 0,
            // `<a>n+<b>`: trailing `+1` lexes as a signed Number.
            [Token::Number(n)] => int_of(*n)?,
            // whitespace-separated `<a>n + <b>` → Delim sign + magnitude.
            [Token::Delim(sign @ ('+' | '-')), Token::Number(n)] => {
                let mag = int_of(*n)?;
                if *sign == '-' {
                    -mag
                } else {
                    mag
                }
            }
            // whitespace-separated `<a>n - <b>`: a lone `-`/`+` between spaces
            // lexes as Ident, not Delim.
            [Token::Ident(sign), Token::Number(n)] if sign == "-" || sign == "+" => {
                let mag = int_of(*n)?;
                if sign == "-" {
                    -mag
                } else {
                    mag
                }
            }
            _ => return None,
        };

        // `inline_b` (glued onto the first token) and a separate tail can't both
        // be present (`2n-1` glues, `2n+1` separates) — guard anyway.
        if inline_b != 0 && tail_b != 0 {
            return None;
        }
        Some(Nth {
            a,
            b: inline_b + tail_b,
        })
    }

    /// Parse a single simple/compound selector (the `:not()` argument) from the
    /// token range `[lo, hi)`. No combinators, no comma, no nested `:not`,
    /// no whitespace-joined compounds, no `::`. `None` on any of those.
    fn parse_simple_compound(&self, lo: usize, hi: usize) -> Option<Compound> {
        // Trim surrounding whitespace.
        let mut lo = lo;
        let mut hi = hi;
        while lo < hi && matches!(self.toks[lo].tok, Token::Whitespace) {
            lo += 1;
        }
        while hi > lo && matches!(self.toks[hi - 1].tok, Token::Whitespace) {
            hi -= 1;
        }
        if lo >= hi {
            return None;
        }

        let mut c = Compound::new();
        let mut i = lo;
        while i < hi {
            match &self.toks[i].tok {
                Token::Ident(name) => {
                    if c.tag.is_some() {
                        return None;
                    }
                    c.tag = Some(name.to_ascii_lowercase());
                }
                Token::Hash(text) => c.ids.push(text.clone()),
                Token::Delim('*') => c.universal = true,
                Token::Delim('.') => {
                    if let Some(Token::Ident(name)) = self.toks.get(i + 1).map(|s| &s.tok) {
                        c.classes.push(name.clone());
                        i += 1;
                    } else {
                        return None;
                    }
                }
                Token::LeftBracket => {
                    let close = self.find_bracket_close(i, hi)?;
                    let attr = self.parse_attr(i + 1, close)?;
                    c.attrs.push(attr);
                    i = close;
                }
                Token::Colon => {
                    // Only a *bare* structural pseudo is allowed inside `:not`.
                    match self.toks.get(i + 1).map(|s| &s.tok) {
                        Some(Token::Ident(name)) => {
                            match bare_pseudo(name) {
                                // unknown/never-match disallowed inside :not.
                                PseudoClass::NeverMatch => return None,
                                p => c.pseudos.push(p),
                            }
                            i += 1;
                        }
                        _ => return None,
                    }
                }
                // Whitespace, combinators, functions, etc. → invalid.
                _ => return None,
            }
            i += 1;
        }
        if c.is_empty() {
            None
        } else {
            Some(c)
        }
    }

    // --- §4.5 declaration block ---

    /// Cursor must be on `{`. Parses the `;`-separated declaration list and
    /// leaves the cursor just after the matching `}`.
    fn parse_declaration_block(&mut self) -> Vec<Declaration> {
        self.bump(); // `{`
        let mut decls = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Token::Eof => return decls, // unbalanced at EOF → close.
                Token::RightBrace => {
                    self.bump();
                    return decls;
                }
                Token::Semicolon => {
                    self.bump();
                }
                _ => {
                    if let Some(d) = self.parse_declaration() {
                        decls.push(d);
                    }
                }
            }
        }
    }

    /// Parse one declaration. On any error, consumes up to (not past) the next
    /// top-level `;`/`}` and returns `None`.
    fn parse_declaration(&mut self) -> Option<Declaration> {
        // 1. name. Custom properties (`--name`) are case-sensitive; everything
        // else is lowercased.
        let name = match self.peek() {
            Token::Ident(n) if n.starts_with("--") => n.clone(),
            Token::Ident(n) => n.to_ascii_lowercase(),
            _ => {
                self.recover_declaration();
                return None;
            }
        };
        self.bump();
        self.skip_whitespace();

        // 2. colon
        if !matches!(self.peek(), Token::Colon) {
            self.recover_declaration();
            return None;
        }
        self.bump();
        self.skip_whitespace();

        // 3. value tokens up to top-level `;`/`}` (balanced parens defensive).
        let val_start = self.pos;
        let mut paren_depth = 0;
        loop {
            match self.peek() {
                Token::Eof => break,
                Token::RightBrace if paren_depth == 0 => break,
                Token::Semicolon if paren_depth == 0 => break,
                // `Function(name)` consumed its own `(`, so it opens a paren too.
                Token::LeftParen | Token::Function(_) => {
                    paren_depth += 1;
                    self.bump();
                }
                Token::RightParen => {
                    if paren_depth > 0 {
                        paren_depth -= 1;
                    }
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
        let val_end = self.pos;

        let (value, important) = self.build_value(val_start, val_end);
        // empty value → bad declaration.
        if value.raw.is_empty() && value.components.is_empty() {
            return None;
        }
        Some(Declaration {
            name,
            value,
            important,
        })
    }

    /// Skip to (not past) the next top-level `;` or `}`.
    fn recover_declaration(&mut self) {
        loop {
            match self.peek() {
                Token::Eof | Token::Semicolon | Token::RightBrace => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    // --- §4.6 value building ---

    /// Build a `Value` from the token range `[start, end)`, detecting and
    /// stripping a trailing `!important`.
    fn build_value(&self, start: usize, end: usize) -> (Value, bool) {
        // Trim trailing/leading whitespace token indices.
        let mut lo = start;
        let mut hi = end;
        while lo < hi && matches!(self.toks[lo].tok, Token::Whitespace) {
            lo += 1;
        }
        while hi > lo && matches!(self.toks[hi - 1].tok, Token::Whitespace) {
            hi -= 1;
        }

        // Detect trailing `! important` (whitespace-tolerant): scan from the
        // end for `important` ident preceded (ignoring ws) by `!` delim.
        let mut important = false;
        let mut value_hi = hi;
        {
            let mut j = hi;
            // last non-ws token
            while j > lo && matches!(self.toks[j - 1].tok, Token::Whitespace) {
                j -= 1;
            }
            if j > lo {
                if let Token::Ident(id) = &self.toks[j - 1].tok {
                    if id.eq_ignore_ascii_case("important") {
                        // look back for `!`
                        let mut k = j - 1;
                        while k > lo && matches!(self.toks[k - 1].tok, Token::Whitespace) {
                            k -= 1;
                        }
                        if k > lo && matches!(self.toks[k - 1].tok, Token::Delim('!')) {
                            important = true;
                            value_hi = k - 1;
                            // re-trim trailing whitespace before the `!`
                            while value_hi > lo
                                && matches!(self.toks[value_hi - 1].tok, Token::Whitespace)
                            {
                                value_hi -= 1;
                            }
                        }
                    }
                }
            }
        }

        let raw = self.slice_raw(lo, value_hi);
        let components = self.classify_components(lo, value_hi);
        (Value { raw, components }, important)
    }

    /// Slice the original source for `[lo, hi)` token range, collapsing internal
    /// whitespace runs to single spaces and trimming.
    fn slice_raw(&self, lo: usize, hi: usize) -> String {
        // Rebuild from token spans (rather than one source slice) so any
        // comments interleaved in the value are dropped, and internal
        // whitespace runs collapse to single spaces.
        let mut out = String::new();
        let mut prev_ws = false;
        for s in &self.toks[lo..hi] {
            if matches!(s.tok, Token::Whitespace) {
                if !out.is_empty() {
                    prev_ws = true;
                }
                continue;
            }
            if prev_ws {
                out.push(' ');
            }
            prev_ws = false;
            out.push_str(&self.css[s.start..s.end]);
        }
        out
    }

    /// Classify value tokens `[lo, hi)` into `Component`s (§1.1). A `Function(`
    /// token's argument tokens (up to the matching `RightParen`) are folded
    /// into one `Function`/`Color` component and skipped.
    fn classify_components(&self, lo: usize, hi: usize) -> Vec<Component> {
        let mut out = Vec::new();
        let mut i = lo;
        while i < hi {
            let s = &self.toks[i];
            match &s.tok {
                Token::Whitespace => {}
                Token::Comma => out.push(Component::Comma),
                Token::Number(n) => out.push(Component::Number(*n)),
                Token::Percentage(n) => out.push(Component::Dimension {
                    value: *n,
                    unit: "%".into(),
                }),
                Token::Dimension { value, unit } => out.push(Component::Dimension {
                    value: *value,
                    unit: unit.clone(),
                }),
                Token::Str(text) => out.push(Component::Str(text.clone())),
                Token::Hash(text) => match color::parse_hex(text) {
                    Some(rgba) => out.push(Component::Color(rgba)),
                    None => out.push(Component::Raw(format!("#{text}"))),
                },
                Token::Ident(name) => {
                    let lower = name.to_ascii_lowercase();
                    match color::named(&lower) {
                        Some(rgba) => out.push(Component::Color(rgba)),
                        None => out.push(Component::Keyword(name.clone())),
                    }
                }
                Token::Function(name) => {
                    let (component, next) = self.parse_function(name.clone(), i, hi);
                    out.push(component);
                    i = next;
                    continue;
                }
                _ => out.push(Component::Raw(self.css[s.start..s.end].to_string())),
            }
            i += 1;
        }
        out
    }

    /// Build a `Function`/`Color` component for the `Function(` token at index
    /// `idx`. Returns the component and the token index just past the matching
    /// `RightParen` (or `hi` at EOF). `raw_args` is sliced from the source.
    fn parse_function(&self, name: String, idx: usize, hi: usize) -> (Component, usize) {
        let open_end = self.toks[idx].end; // byte offset just after `(`
                                           // Walk tokens to find the matching RightParen (balanced).
        let mut depth = 1;
        let mut j = idx + 1;
        let mut args_byte_end = open_end;
        while j < self.toks.len() {
            match self.toks[j].tok {
                // `Function(name)` consumed its own `(`, so it opens a paren too.
                Token::LeftParen | Token::Function(_) => depth += 1,
                Token::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                // Unterminated function: don't swallow the block's `}`.
                Token::RightBrace | Token::Eof => break,
                _ => {}
            }
            args_byte_end = self.toks[j].end;
            j += 1;
        }
        let raw_args = self.css[open_end..args_byte_end].trim().to_string();
        let next = (j + 1).min(hi);

        let lower = name.to_ascii_lowercase();
        if lower == "rgb" || lower == "rgba" {
            if let Some(rgba) = color::parse_rgb(&raw_args) {
                return (Component::Color(rgba), next);
            }
        } else if lower == "hsl" || lower == "hsla" {
            if let Some(rgba) = color::parse_hsl(&raw_args) {
                return (Component::Color(rgba), next);
            }
        } else if lower == "color-mix" {
            // E24-M3: `color-mix(in srgb, A p%, B q%)` folds to a literal color.
            if let Some(rgba) = color::parse_color_mix(&raw_args) {
                return (Component::Color(rgba), next);
            }
        }
        (
            Component::Function {
                name: lower,
                raw_args,
            },
            next,
        )
    }
}

// --- E7-M1: selector pseudo-class / An+B helpers ---

/// Map a bare `:ident` to its `PseudoClass`. Known structural pseudos map to
/// themselves; everything else (incl. `:hover`, `:focus`, unknown) → NeverMatch.
/// Map a pseudo-element name to its variant; `None` for any other `::name`
/// (first-line / first-letter / marker / selection / unknown) → caller drops
/// the rule (E7-M2).
fn pseudo_element(name: &str) -> Option<PseudoElement> {
    if name.eq_ignore_ascii_case("before") {
        Some(PseudoElement::Before)
    } else if name.eq_ignore_ascii_case("after") {
        Some(PseudoElement::After)
    } else {
        None
    }
}

fn bare_pseudo(name: &str) -> PseudoClass {
    if name.eq_ignore_ascii_case("first-child") {
        PseudoClass::FirstChild
    } else if name.eq_ignore_ascii_case("last-child") {
        PseudoClass::LastChild
    } else if name.eq_ignore_ascii_case("only-child") {
        PseudoClass::OnlyChild
    } else if name.eq_ignore_ascii_case("root") {
        PseudoClass::Root
    } else if name.eq_ignore_ascii_case("empty") {
        PseudoClass::Empty
    } else if name.eq_ignore_ascii_case("checked") {
        PseudoClass::Checked
    } else if name.eq_ignore_ascii_case("disabled") {
        PseudoClass::Disabled
    } else if name.eq_ignore_ascii_case("enabled") {
        PseudoClass::Enabled
    } else if name.eq_ignore_ascii_case("required") {
        PseudoClass::Required
    } else if name.eq_ignore_ascii_case("read-only") {
        PseudoClass::ReadOnly
    } else if name.eq_ignore_ascii_case("read-write") {
        PseudoClass::ReadWrite
    } else {
        PseudoClass::NeverMatch
    }
}

/// Parse an `An+B` ident term such as `"n"`, `"-n"`, `"n-1"`, `"-n+2"`,
/// returning `(a, b_inline)` where `a` is ±1 and `b_inline` is the glued tail
/// (0 if absent). The string starts with an optional `-`, then `n`/`N`.
fn parse_n_ident(s: &str) -> Option<(i32, i32)> {
    let (a, rest) = if let Some(r) = s.strip_prefix('-') {
        (-1, r)
    } else {
        (1, s)
    };
    let tail = rest.strip_prefix(['n', 'N'])?;
    Some((a, parse_b_tail(tail)?))
}

/// Parse a dimension unit such as `"n"`, `"n-1"`, returning the glued `b` tail.
fn parse_n_unit(unit: &str) -> Option<i32> {
    let tail = unit.strip_prefix(['n', 'N'])?;
    parse_b_tail(tail)
}

/// Parse an optional glued `±<int>` tail (e.g. `""`, `"-1"`, `"+2"`).
fn parse_b_tail(tail: &str) -> Option<i32> {
    if tail.is_empty() {
        Some(0)
    } else {
        // must be a signed integer like `-1` / `+2`.
        let signed = tail.strip_prefix('+').unwrap_or(tail);
        signed.parse::<i32>().ok()
    }
}

/// Convert an `f32` token value to an `i32`, rejecting non-integers (`2.5`).
fn int_of(n: f32) -> Option<i32> {
    if n.fract() == 0.0 && n.is_finite() {
        Some(n as i32)
    } else {
        None
    }
}

// --- E13-M3: @media prelude mini-parser ---

/// Parse a media-query / media-condition *string* into a [`MediaQuery`] (E15-M2,
/// reused for `<source media>` + `sizes` media-conditions). Tokenizes `s` then
/// delegates to the token-based parser. NEVER panics.
pub fn parse_media_query(s: &str) -> MediaQuery {
    let mut tz = Tokenizer::new(s);
    let mut toks: Vec<Token> = Vec::new();
    loop {
        let tok = tz.next_token();
        if matches!(tok, Token::Eof) {
            break;
        }
        toks.push(tok);
    }
    parse_media_query_tokens(&toks)
}

/// Parse a `@media` prelude token slice into a [`MediaQuery`]. NEVER panics:
/// malformed input yields empty conditions (never match) or `Unknown` features.
/// Splits on top-level `Comma` into conditions (OR).
fn parse_media_query_tokens(tokens: &[Token]) -> MediaQuery {
    let mut conditions = Vec::new();
    for seg in tokens.split(|t| matches!(t, Token::Comma)) {
        if let Some(c) = parse_media_condition(seg) {
            conditions.push(c);
        }
        // A condition that fully fails to parse is dropped (→ that OR branch
        // never matches); an unknown feature becomes `MediaFeature::Unknown`.
    }
    MediaQuery { conditions }
}

/// Parse one comma-separated condition. Grammar (lenient):
/// `[not] [<media-type> [and]] (<feature>)*` where features are `and`-joined
/// `( ident : value )` groups. Returns `None` only when the whole segment is
/// empty/garbage with no usable content.
fn parse_media_condition(seg: &[Token]) -> Option<MediaCondition> {
    // Significant (non-whitespace) tokens.
    let toks: Vec<&Token> = seg
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace))
        .collect();
    if toks.is_empty() {
        return None;
    }
    let mut i = 0;
    let mut negated = false;
    // optional leading `not`
    if let Some(Token::Ident(id)) = toks.get(i).copied() {
        if id.eq_ignore_ascii_case("not") {
            negated = true;
            i += 1;
        }
    }
    // optional media type ident (anything that isn't `(` and isn't `and`)
    let mut media_type = MediaType::All;
    let mut saw_type = false;
    if let Some(Token::Ident(id)) = toks.get(i).copied() {
        if !id.eq_ignore_ascii_case("and") {
            media_type = match id.to_ascii_lowercase().as_str() {
                "all" => MediaType::All,
                "screen" => MediaType::Screen,
                "print" => MediaType::Print,
                // unknown type → Print-ish (never matches screen).
                _ => MediaType::Print,
            };
            saw_type = true;
            i += 1;
        }
    }
    // optional `and` joining type and the first feature group.
    if saw_type {
        if let Some(Token::Ident(id)) = toks.get(i).copied() {
            if id.eq_ignore_ascii_case("and") {
                i += 1;
            }
        }
    }

    // zero or more `( ident : value )` feature groups joined by `and`.
    let mut features = Vec::new();
    while i < toks.len() {
        // optional `and` between feature groups.
        if let Some(Token::Ident(id)) = toks.get(i).copied() {
            if id.eq_ignore_ascii_case("and") {
                i += 1;
                continue;
            }
        }
        match toks.get(i).copied() {
            Some(Token::LeftParen) => {
                // find the matching `)`.
                let open = i;
                let mut depth = 1;
                let mut j = i + 1;
                while j < toks.len() {
                    match toks[j] {
                        Token::LeftParen => depth += 1,
                        Token::RightParen => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j >= toks.len() {
                    // unterminated `(` → structural break, this feature never matches.
                    features.push(MediaFeature::Unknown);
                    break;
                }
                features.push(parse_media_feature(&toks[open + 1..j]));
                i = j + 1;
            }
            // Any non-paren token here is unexpected → unknown feature; advance
            // to avoid an infinite loop.
            _ => {
                features.push(MediaFeature::Unknown);
                i += 1;
            }
        }
    }

    Some(MediaCondition {
        negated,
        media_type,
        features,
    })
}

/// Parse the tokens strictly inside a feature's parens: `ident : value`.
fn parse_media_feature(inner: &[&Token]) -> MediaFeature {
    let mut it = inner.iter().copied();
    let name = match it.next() {
        Some(Token::Ident(n)) => n.to_ascii_lowercase(),
        _ => return MediaFeature::Unknown,
    };
    if !matches!(it.next(), Some(Token::Colon)) {
        return MediaFeature::Unknown;
    }
    let val = it.next();
    match name.as_str() {
        "min-width" | "max-width" | "min-height" | "max-height" => {
            let px = match val {
                Some(Token::Dimension { value, unit }) if unit.eq_ignore_ascii_case("px") => *value,
                // a bare `0` is a valid length.
                Some(Token::Number(n)) if *n == 0.0 => 0.0,
                _ => return MediaFeature::Unknown,
            };
            match name.as_str() {
                "min-width" => MediaFeature::MinWidth(px),
                "max-width" => MediaFeature::MaxWidth(px),
                "min-height" => MediaFeature::MinHeight(px),
                "max-height" => MediaFeature::MaxHeight(px),
                _ => unreachable!(),
            }
        }
        "orientation" => match val {
            Some(Token::Ident(o)) if o.eq_ignore_ascii_case("portrait") => {
                MediaFeature::Orientation(Orientation::Portrait)
            }
            Some(Token::Ident(o)) if o.eq_ignore_ascii_case("landscape") => {
                MediaFeature::Orientation(Orientation::Landscape)
            }
            _ => MediaFeature::Unknown,
        },
        _ => MediaFeature::Unknown,
    }
}

// --- E6-M2: fold @font-face declarations into a FontFaceRule ---

/// Strip a single pair of matching surrounding quotes (`"…"` or `'…'`) from a
/// `url()`/`format()`/`local()` raw argument; returns it trimmed otherwise.
fn strip_quotes(raw: &str) -> String {
    let t = raw.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        if (first == b'"' || first == b'\'') && bytes[bytes.len() - 1] == first {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// Fold an `@font-face` declaration list into a `FontFaceRule`. Returns `None`
/// when the required `font-family` or `src` descriptor is missing/empty.
fn fold_font_face(decls: &[Declaration]) -> Option<FontFaceRule> {
    let mut family: Option<String> = None;
    let mut src: Vec<FontSrc> = Vec::new();
    let mut weight: Option<u16> = None;
    let mut style: Option<FontFaceStyle> = None;

    for d in decls {
        match d.name.as_str() {
            "font-family" => {
                // First quoted string, else the raw value (unquoted ident(s)).
                let name = d
                    .value
                    .components
                    .iter()
                    .find_map(|c| match c {
                        Component::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| d.value.raw.trim().to_string());
                if !name.is_empty() {
                    family = Some(name);
                }
            }
            "src" => {
                src = parse_src(&d.value.components);
            }
            "font-weight" => {
                weight = d.value.components.iter().find_map(|c| match c {
                    Component::Number(n) => Some(*n as u16),
                    Component::Keyword(k) if k.eq_ignore_ascii_case("bold") => Some(700),
                    Component::Keyword(k) if k.eq_ignore_ascii_case("normal") => Some(400),
                    _ => None,
                });
            }
            "font-style" => {
                style = d.value.components.iter().find_map(|c| match c {
                    Component::Keyword(k) if k.eq_ignore_ascii_case("italic") => {
                        Some(FontFaceStyle::Italic)
                    }
                    Component::Keyword(k) if k.eq_ignore_ascii_case("oblique") => {
                        Some(FontFaceStyle::Oblique)
                    }
                    Component::Keyword(k) if k.eq_ignore_ascii_case("normal") => {
                        Some(FontFaceStyle::Normal)
                    }
                    _ => None,
                });
            }
            _ => {} // unicode-range, font-display, … ignored.
        }
    }

    let family = family?;
    if src.is_empty() {
        return None;
    }
    Some(FontFaceRule {
        family,
        src,
        weight,
        style,
    })
}

/// Split the `src` components on top-level commas into `FontSrc` entries. Per
/// entry: a `url(…)` (optionally followed by `format(…)`) → `FontSrc::Url`; a
/// `local(…)` → `FontSrc::Local`. Anything else is ignored.
fn parse_src(components: &[Component]) -> Vec<FontSrc> {
    let mut out = Vec::new();
    for entry in components.split(|c| matches!(c, Component::Comma)) {
        let mut url: Option<String> = None;
        let mut format: Option<String> = None;
        for c in entry {
            if let Component::Function { name, raw_args } = c {
                match name.as_str() {
                    "url" => url = Some(strip_quotes(raw_args)),
                    "format" if url.is_some() => {
                        format = Some(strip_quotes(raw_args).to_ascii_lowercase())
                    }
                    "local" => out.push(FontSrc::Local(strip_quotes(raw_args))),
                    _ => {}
                }
            }
        }
        if let Some(url) = url {
            out.push(FontSrc::Url { url, format });
        }
    }
    out
}
