//! Character-driven HTML tokenizer (pragmatic subset of the WHATWG spec).
//!
//! Tokens are pulled one at a time via [`Tokenizer::next_token`]. Tag and
//! attribute names are lowercased before emission; `Character` is emitted per
//! character (the tree builder coalesces runs into Text nodes).

use crate::entities;
use starfish_dom::Attr;

#[derive(Debug, PartialEq, Eq)]
pub enum Token {
    Doctype {
        name: String,
    },
    StartTag {
        name: String,
        /// Source-case tag name (used by the tree builder for SVG foreign
        /// content where case is significant; `name` stays lowercased for the
        /// byte-identical HTML path).
        name_raw: String,
        attrs: Vec<Attr>,
        /// Attributes with source-case names (SVG-only; `attrs` keeps the
        /// lowercased names HTML expects). Same order/values as `attrs`.
        attrs_raw: Vec<Attr>,
        self_closing: bool,
    },
    EndTag {
        name: String,
        name_raw: String,
    },
    Character(char),
    Comment(String),
    Eof,
}

/// Content model for the bytes that follow a special start tag. `RawText`
/// (script/style) emits the body literally; `RcData` (textarea/title) also
/// decodes character references but treats `<` as text.
#[derive(PartialEq, Eq)]
enum TokenizerMode {
    Data,
    RawText,
    RcData,
}

pub struct Tokenizer<'a> {
    input: &'a [u8],
    text: &'a str,
    pos: usize,
    /// Buffered Character tokens produced from a single character reference
    /// (e.g. `&nbsp;`), emitted one per `next_token` call.
    pending: std::collections::VecDeque<char>,
    /// Active content model; switched to RawText/RcData by special start tags.
    mode: TokenizerMode,
    /// Lowercased tag name whose matching end tag exits the raw/rcdata mode.
    mode_end_tag: String,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Tokenizer {
            input: input.as_bytes(),
            text: input,
            pos: 0,
            pending: std::collections::VecDeque::new(),
            mode: TokenizerMode::Data,
            mode_end_tag: String::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    /// Consume and return the next char, advancing `pos` by its UTF-8 length.
    fn next_char(&mut self) -> Option<char> {
        let c = self.text[self.pos..].chars().next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// Pull the next token. Returns `Token::Eof` at end (idempotent).
    pub fn next_token(&mut self) -> Token {
        if let Some(c) = self.pending.pop_front() {
            return Token::Character(c);
        }
        if self.eof() {
            return Token::Eof;
        }

        match self.mode {
            TokenizerMode::RawText => return self.scan_rawtext(false),
            TokenizerMode::RcData => return self.scan_rawtext(true),
            TokenizerMode::Data => {}
        }

        let c = self.peek().unwrap();
        if c == '<' {
            self.tag_open()
        } else if c == '&' {
            // Character reference in data: decode, queue extras, emit first.
            self.next_char();
            let chars = self.consume_char_reference(None);
            let mut it = chars.into_iter();
            let first = it.next().unwrap();
            self.pending.extend(it);
            Token::Character(first)
        } else {
            self.next_char();
            Token::Character(c)
        }
    }

    /// After `<`.
    fn tag_open(&mut self) -> Token {
        self.next_char(); // consume '<'
        match self.peek() {
            Some('/') => {
                self.next_char();
                self.end_tag_open()
            }
            Some('!') => {
                self.next_char();
                self.markup_declaration_open()
            }
            Some(c) if c.is_ascii_alphabetic() => self.tag_name(false),
            _ => Token::Character('<'),
        }
    }

    /// After `</`.
    fn end_tag_open(&mut self) -> Token {
        match self.peek() {
            Some('>') => {
                self.next_char();
                // empty end tag `</>`: ignore, restart
                self.next_token()
            }
            Some(c) if c.is_ascii_alphabetic() => self.tag_name(true),
            _ => self.bogus_comment(),
        }
    }

    /// Accumulate a tag name and any attributes; emit the start/end tag.
    fn tag_name(&mut self, end: bool) -> Token {
        let mut name = String::new();
        let mut name_raw = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            name_raw.push(c);
            self.next_char();
        }

        let mut attrs: Vec<Attr> = Vec::new();
        let mut attrs_raw: Vec<Attr> = Vec::new();
        let mut self_closing = false;

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => break,
                Some('>') => {
                    self.next_char();
                    break;
                }
                Some('/') => {
                    self.next_char();
                    // self-closing if followed by '>'
                    self.skip_whitespace();
                    if self.peek() == Some('>') {
                        self.next_char();
                        self_closing = true;
                        break;
                    }
                    // stray '/': reconsume as before-attribute-name
                    continue;
                }
                Some(_) => {
                    if let Some((attr, raw)) = self.attribute() {
                        // duplicate attributes: first wins (keyed on lowercased name)
                        if !attr.name.is_empty() && !attrs.iter().any(|a| a.name == attr.name) {
                            attrs.push(attr);
                            attrs_raw.push(raw);
                        }
                    }
                }
            }
        }

        if end {
            Token::EndTag { name, name_raw }
        } else {
            if !self_closing {
                match name.as_str() {
                    "script" | "style" => {
                        self.mode = TokenizerMode::RawText;
                        self.mode_end_tag = name.clone();
                    }
                    "textarea" | "title" => {
                        self.mode = TokenizerMode::RcData;
                        self.mode_end_tag = name.clone();
                    }
                    _ => {}
                }
            }
            Token::StartTag {
                name,
                name_raw,
                attrs,
                attrs_raw,
                self_closing,
            }
        }
    }

    /// Parse a single attribute starting at a name char. Leaves `pos` at the
    /// char after the value (or after the name for valueless attrs). Returns
    /// both the lowercased-name `Attr` (HTML) and the source-case `Attr` (SVG).
    fn attribute(&mut self) -> Option<(Attr, Attr)> {
        let mut name = String::new();
        let mut name_raw = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '=' || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            name_raw.push(c);
            self.next_char();
        }

        self.skip_whitespace();
        if self.peek() != Some('=') {
            // valueless attribute
            return Some((
                Attr {
                    name,
                    value: String::new(),
                },
                Attr {
                    name: name_raw,
                    value: String::new(),
                },
            ));
        }
        self.next_char(); // consume '='
        self.skip_whitespace();

        let value = match self.peek() {
            Some('"') => {
                self.next_char();
                self.attribute_value_quoted('"')
            }
            Some('\'') => {
                self.next_char();
                self.attribute_value_quoted('\'')
            }
            Some('>') | None => String::new(),
            Some(_) => self.attribute_value_unquoted(),
        };

        Some((
            Attr {
                name,
                value: value.clone(),
            },
            Attr {
                name: name_raw,
                value,
            },
        ))
    }

    fn attribute_value_quoted(&mut self, quote: char) -> String {
        let mut value = String::new();
        while let Some(c) = self.peek() {
            if c == quote {
                self.next_char();
                break;
            }
            if c == '&' {
                self.next_char();
                value.extend(self.consume_char_reference(Some(quote)));
            } else {
                value.push(c);
                self.next_char();
            }
        }
        value
    }

    fn attribute_value_unquoted(&mut self) -> String {
        let mut value = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '>' {
                break;
            }
            if c == '&' {
                self.next_char();
                value.extend(self.consume_char_reference(None));
            } else {
                value.push(c);
                self.next_char();
            }
        }
        value
    }

    /// After `<!`: dispatch to comment, doctype, or bogus comment.
    fn markup_declaration_open(&mut self) -> Token {
        if self.text[self.pos..].starts_with("--") {
            self.pos += 2;
            self.comment()
        } else if self.text[self.pos..]
            .get(..7)
            .map(|s| s.eq_ignore_ascii_case("doctype"))
            .unwrap_or(false)
        {
            self.pos += 7;
            self.doctype()
        } else {
            self.bogus_comment()
        }
    }

    /// `<!-- ... -->`; accumulate until `-->` (or EOF).
    fn comment(&mut self) -> Token {
        let mut data = String::new();
        loop {
            if self.text[self.pos..].starts_with("-->") {
                self.pos += 3;
                break;
            }
            match self.next_char() {
                Some(c) => data.push(c),
                None => break, // unterminated at EOF
            }
        }
        Token::Comment(data)
    }

    /// Accumulate until `>` (or EOF); used for `<!`-junk and `</`-junk.
    fn bogus_comment(&mut self) -> Token {
        let mut data = String::new();
        while let Some(c) = self.peek() {
            if c == '>' {
                self.next_char();
                break;
            }
            data.push(c);
            self.next_char();
        }
        Token::Comment(data)
    }

    /// After `<!doctype`: read a name, discard the rest until `>`.
    fn doctype(&mut self) -> Token {
        self.skip_whitespace();
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.next_char();
        }
        // discard the rest up to '>'
        while let Some(c) = self.peek() {
            self.next_char();
            if c == '>' {
                break;
            }
        }
        Token::Doctype { name }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.next_char();
            } else {
                break;
            }
        }
    }

    /// Consume a character reference, the `&` already having been consumed.
    /// Returns the decoded char(s); on no match, returns a literal `&` and
    /// leaves the scan position unchanged (lenient). `_additional` is the
    /// attribute-value quote context (unused beyond signaling; kept minimal).
    fn consume_char_reference(&mut self, _additional: Option<char>) -> Vec<char> {
        let rest = &self.text[self.pos..];

        if let Some(after) = rest.strip_prefix('#') {
            // numeric reference
            let (is_hex, digits_start) = if after.starts_with(['x', 'X']) {
                (true, 1)
            } else {
                (false, 0)
            };
            let body = &after[digits_start..];
            let end = if is_hex {
                body.find(|c: char| !c.is_ascii_hexdigit())
            } else {
                body.find(|c: char| !c.is_ascii_digit())
            }
            .unwrap_or(body.len());

            if end == 0 {
                return vec!['&']; // malformed: no digits, literal '&'
            }
            let radix = if is_hex { 16 } else { 10 };
            // u32::MAX overflow → treat as out-of-range (FFFD).
            let code = u32::from_str_radix(&body[..end], radix).unwrap_or(0x110000);
            // advance past `#`, optional x, digits, and an optional `;`.
            let semi = if body[end..].starts_with(';') { 1 } else { 0 };
            self.pos += 1 + digits_start + end + semi;
            let ch = if code == 0 {
                '\u{FFFD}'
            } else if (0x80..=0x9F).contains(&code) {
                entities::c1_replacement(code)
            } else {
                char::from_u32(code).unwrap_or('\u{FFFD}')
            };
            return vec![ch];
        }

        // named / legacy reference
        if let Some((repl, len)) = entities::lookup_named(rest) {
            self.pos += len;
            return repl.chars().collect();
        }

        vec!['&']
    }

    /// Scan a RAWTEXT (script/style) or RCDATA (textarea/title) body. Consumes
    /// characters until the matching end tag `</name` (followed by a name
    /// boundary), at which point it switches back to Data mode and leaves `pos`
    /// at the `<` so the end tag is tokenized normally. With `decode_entities`
    /// (RCDATA) character references are decoded; `<` is always literal text.
    fn scan_rawtext(&mut self, decode_entities: bool) -> Token {
        let mut buf = String::new();
        while let Some(c) = self.peek() {
            if c == '<' && self.at_close_tag() {
                self.mode = TokenizerMode::Data;
                break;
            }
            if decode_entities && c == '&' {
                self.next_char();
                buf.extend(self.consume_char_reference(None));
            } else {
                buf.push(c);
                self.next_char();
            }
        }
        if self.peek().is_none() {
            // EOF without a close tag.
            self.mode = TokenizerMode::Data;
        }

        let mut chars: std::collections::VecDeque<char> = buf.chars().collect();
        match chars.pop_front() {
            None => self.next_token(),
            Some(first) => {
                self.pending.append(&mut chars);
                Token::Character(first)
            }
        }
    }

    /// True if the text at `pos` is `</` + `mode_end_tag` (ASCII
    /// case-insensitive) followed by a name boundary (whitespace, `/`, or `>`).
    fn at_close_tag(&self) -> bool {
        let rest = &self.text[self.pos..];
        let Some(after) = rest.strip_prefix("</") else {
            return false;
        };
        let name = &self.mode_end_tag;
        // Compare on bytes (mode_end_tag is ASCII) so a multibyte char straddling
        // the name-length boundary can't trigger a mid-UTF-8 slice panic.
        let (nb, ab) = (name.as_bytes(), after.as_bytes());
        if ab.len() < nb.len() || !ab[..nb.len()].eq_ignore_ascii_case(nb) {
            return false;
        }
        // The matched prefix is ASCII, so `nb.len()` is a valid char boundary.
        match after[nb.len()..].chars().next() {
            None => true, // end tag runs to EOF
            Some(c) => c.is_ascii_whitespace() || c == '/' || c == '>',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rawtext_close_tag_multibyte_no_panic() {
        // A near-miss close tag with a multibyte char must not panic on a
        // mid-UTF-8 slice; content runs to EOF.
        let toks = tokens("<script>x</abc\u{1F600}>");
        let text: String = toks
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert!(text.starts_with('x'));
    }

    fn tokens(s: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(s);
        let mut out = Vec::new();
        loop {
            let tok = t.next_token();
            let done = tok == Token::Eof;
            out.push(tok);
            if done {
                break;
            }
        }
        out
    }

    fn attr(name: &str, value: &str) -> Attr {
        Attr {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn plain_text() {
        assert_eq!(
            tokens("Hi"),
            vec![Token::Character('H'), Token::Character('i'), Token::Eof]
        );
    }

    #[test]
    fn start_tag_with_attr() {
        assert_eq!(
            tokens(r#"<div class="a">"#),
            vec![
                Token::StartTag {
                    name: "div".into(),
                    name_raw: "div".into(),
                    attrs: vec![attr("class", "a")],
                    attrs_raw: vec![attr("class", "a")],
                    self_closing: false,
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn self_closing() {
        assert_eq!(
            tokens("<br/>"),
            vec![
                Token::StartTag {
                    name: "br".into(),
                    name_raw: "br".into(),
                    attrs: vec![],
                    attrs_raw: vec![],
                    self_closing: true,
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn single_quoted_and_valueless() {
        assert_eq!(
            tokens("<a href='x' disabled>"),
            vec![
                Token::StartTag {
                    name: "a".into(),
                    name_raw: "a".into(),
                    attrs: vec![attr("href", "x"), attr("disabled", "")],
                    attrs_raw: vec![attr("href", "x"), attr("disabled", "")],
                    self_closing: false,
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn end_tag() {
        assert_eq!(
            tokens("</p>"),
            vec![
                Token::EndTag {
                    name: "p".into(),
                    name_raw: "p".into()
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn comment() {
        assert_eq!(
            tokens("<!-- hi -->"),
            vec![Token::Comment(" hi ".into()), Token::Eof]
        );
    }

    #[test]
    fn doctype() {
        assert_eq!(
            tokens("<!DOCTYPE html>"),
            vec![
                Token::Doctype {
                    name: "html".into()
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn char_references() {
        let toks = tokens("a&amp;b&lt;c&#65;&#x42;");
        let chars: String = toks
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(chars, "a&b<cAB");
    }

    #[test]
    fn uppercase_normalization() {
        assert_eq!(
            tokens("<DIV CLASS=X>"),
            vec![
                Token::StartTag {
                    name: "div".into(),
                    name_raw: "DIV".into(),
                    attrs: vec![attr("class", "X")],
                    attrs_raw: vec![attr("CLASS", "X")],
                    self_closing: false,
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn unterminated_comment_at_eof() {
        assert_eq!(
            tokens("<!-- oops"),
            vec![Token::Comment(" oops".into()), Token::Eof]
        );
    }

    #[test]
    fn nbsp_and_invalid_ref() {
        // &nbsp; -> U+00A0 ; bare & with no match stays literal
        let toks = tokens("&nbsp;&zzz");
        let chars: String = toks
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(chars, "\u{00A0}&zzz");
    }

    // Collect all Character tokens of `s` into a String.
    fn ref_chars(s: &str) -> String {
        tokens(s)
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn named_entities_decode() {
        assert_eq!(ref_chars("&shy;"), "\u{00AD}");
        assert_eq!(ref_chars("&copy;"), "\u{00A9}");
        assert_eq!(ref_chars("&mdash;"), "\u{2014}");
        assert_eq!(ref_chars("&nbsp;"), "\u{00A0}");
        // unrecognized name stays literal
        assert_eq!(ref_chars("&zzz;"), "&zzz;");
    }

    #[test]
    fn numeric_entities_decode() {
        assert_eq!(ref_chars("&#169;"), "\u{00A9}");
        assert_eq!(ref_chars("&#xA9;"), "\u{00A9}");
        assert_eq!(ref_chars("&#x1F600;"), "\u{1F600}"); // 😀
        assert_eq!(ref_chars("&#128;"), "\u{20AC}"); // C1 → €
        assert_eq!(ref_chars("&#0;"), "\u{FFFD}");
        assert_eq!(ref_chars("&#xD800;"), "\u{FFFD}"); // surrogate
    }

    #[test]
    fn legacy_semicolonless_named() {
        // &copy without trailing ';' still decodes (legacy table).
        assert_eq!(ref_chars("&copy"), "\u{00A9}");
        // numeric without ';' decodes too.
        assert_eq!(ref_chars("&#169"), "\u{00A9}");
    }

    // --- E23-M3: RAWTEXT / RCDATA ---

    /// The single coalesced Character run between the first and last token.
    fn rawtext_body(toks: &[Token]) -> String {
        toks.iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rawtext_script_keeps_lt_and_amp_literal() {
        let toks = tokens("<script>if(a<b){x&y}</script>");
        assert_eq!(
            toks[0],
            Token::StartTag {
                name: "script".into(),
                name_raw: "script".into(),
                attrs: vec![],
                attrs_raw: vec![],
                self_closing: false,
            }
        );
        // `<b` is NOT a tag and `&` is NOT decoded: one literal run.
        assert_eq!(rawtext_body(&toks), "if(a<b){x&y}");
        assert_eq!(
            toks[toks.len() - 2],
            Token::EndTag {
                name: "script".into(),
                name_raw: "script".into(),
            }
        );
    }

    #[test]
    fn rawtext_style_keeps_lt_and_amp_literal() {
        let toks = tokens("<style>a<b & c</style>");
        assert_eq!(rawtext_body(&toks), "a<b & c");
        assert_eq!(
            toks[toks.len() - 2],
            Token::EndTag {
                name: "style".into(),
                name_raw: "style".into(),
            }
        );
    }

    #[test]
    fn rcdata_textarea_decodes_entities_but_not_tags() {
        let toks = tokens("<textarea>a&amp;b<c></textarea>");
        // &amp; decodes to &, but `<c>` is plain text (not a tag), `>` included.
        assert_eq!(rawtext_body(&toks), "a&b<c>");
        assert_eq!(
            toks[toks.len() - 2],
            Token::EndTag {
                name: "textarea".into(),
                name_raw: "textarea".into(),
            }
        );
    }

    #[test]
    fn rcdata_title_decodes_entity() {
        let toks = tokens("<title>x &amp; y</title>");
        assert_eq!(rawtext_body(&toks), "x & y");
    }

    #[test]
    fn rawtext_close_tag_case_insensitive() {
        let toks = tokens("<script>x</SCRIPT>");
        assert_eq!(rawtext_body(&toks), "x");
        assert_eq!(
            toks[toks.len() - 2],
            Token::EndTag {
                name: "script".into(),
                name_raw: "SCRIPT".into(),
            }
        );
    }

    #[test]
    fn rawtext_near_miss_close_tag_is_text() {
        // `</scriptx>` is not the close tag (no name boundary after "script");
        // `</scripty` and the rest stay literal until the real </script>.
        let toks = tokens("<script>a</scriptx>b</script>");
        assert_eq!(rawtext_body(&toks), "a</scriptx>b");
    }

    #[test]
    fn rawtext_eof_without_close_tag() {
        let toks = tokens("<script>abc");
        assert_eq!(rawtext_body(&toks), "abc");
        assert_eq!(toks.last(), Some(&Token::Eof));
    }

    #[test]
    fn self_closing_script_stays_data_mode() {
        // A self-closed <script/> does not enter RAWTEXT: the following `<b`
        // tokenizes as a normal start tag.
        let toks = tokens("<script/><b>x");
        assert!(matches!(toks[1], Token::StartTag { ref name, .. } if name == "b"));
    }

    // --- E23-M3: confirming comments / doctype / attributes ---

    #[test]
    fn comment_basic_no_panic() {
        assert_eq!(
            tokens("<!--hi-->"),
            vec![Token::Comment("hi".into()), Token::Eof]
        );
    }

    #[test]
    fn doctype_then_bogus_comment() {
        // A normal doctype followed by malformed `<!weird>` (bogus comment).
        assert_eq!(
            tokens("<!DOCTYPE html><!weird>"),
            vec![
                Token::Doctype {
                    name: "html".into()
                },
                Token::Comment("weird".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn duplicate_attr_first_wins() {
        let toks = tokens("<a x=1 x=2>");
        assert_eq!(
            toks[0],
            Token::StartTag {
                name: "a".into(),
                name_raw: "a".into(),
                attrs: vec![attr("x", "1")],
                attrs_raw: vec![attr("x", "1")],
                self_closing: false,
            }
        );
    }

    #[test]
    fn boolean_and_unquoted_attrs() {
        let toks = tokens("<input disabled value=foo>");
        assert_eq!(
            toks[0],
            Token::StartTag {
                name: "input".into(),
                name_raw: "input".into(),
                attrs: vec![attr("disabled", ""), attr("value", "foo")],
                attrs_raw: vec![attr("disabled", ""), attr("value", "foo")],
                self_closing: false,
            }
        );
    }

    #[test]
    fn void_self_closing_slash() {
        let toks = tokens("<br/>");
        assert!(matches!(
            toks[0],
            Token::StartTag { self_closing: true, ref name, .. } if name == "br"
        ));
    }
}
