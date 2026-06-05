//! The lenient parser: token stream → [`Stylesheet`]. Never panics; recovers
//! from malformed input the way CSS error handling does.

use crate::color;
use crate::model::{Component, Declaration, Rule, Stylesheet, Value};
use crate::selector::{Combinator, Selector, SelectorBuilder};
use crate::tokenizer::{Token, Tokenizer};

/// A token paired with the byte span `[start, end)` it covers in the source.
struct Spanned {
    tok: Token,
    start: usize,
    end: usize,
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
    p.parse_rule_list(&mut rules);
    Stylesheet { rules }
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

    fn parse_rule_list(&mut self, out: &mut Vec<Rule>) {
        loop {
            self.skip_whitespace();
            match self.peek() {
                Token::Eof => return,
                Token::AtKeyword(_) => self.skip_at_rule(),
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
        let mut selectors = Vec::new();
        let mut i = start;
        let mut seg_start = start;
        // split on top-level commas
        while i <= end {
            let is_comma = i < end && matches!(self.toks[i].tok, Token::Comma);
            if is_comma || i == end {
                let sel = self.parse_complex_selector(seg_start, i)?;
                selectors.push(sel);
                seg_start = i + 1;
            }
            i += 1;
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
                Token::Delim('.') => {
                    // `.` then ident → class
                    if let Some(Token::Ident(name)) = self.toks.get(i + 1).map(|s| &s.tok) {
                        b.push_class(name.clone());
                        i += 1;
                    } else {
                        b.invalidate();
                    }
                }
                // Anything else (`:`, `[`, `+`, `~`, Function, …) → invalid.
                _ => b.invalidate(),
            }
            i += 1;
        }
        b.finish()
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
        // 1. name
        let name = match self.peek() {
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
