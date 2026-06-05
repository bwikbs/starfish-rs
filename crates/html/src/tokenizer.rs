//! Character-driven HTML tokenizer (pragmatic subset of the WHATWG spec).
//!
//! Tokens are pulled one at a time via [`Tokenizer::next_token`]. Tag and
//! attribute names are lowercased before emission; `Character` is emitted per
//! character (the tree builder coalesces runs into Text nodes).

use starfish_dom::Attr;

#[derive(Debug, PartialEq, Eq)]
pub enum Token {
    Doctype { name: String },
    StartTag {
        name: String,
        attrs: Vec<Attr>,
        self_closing: bool,
    },
    EndTag { name: String },
    Character(char),
    Comment(String),
    Eof,
}

pub struct Tokenizer<'a> {
    input: &'a [u8],
    text: &'a str,
    pos: usize,
    /// Buffered Character tokens produced from a single character reference
    /// (e.g. `&nbsp;`), emitted one per `next_token` call.
    pending: std::collections::VecDeque<char>,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Tokenizer {
            input: input.as_bytes(),
            text: input,
            pos: 0,
            pending: std::collections::VecDeque::new(),
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
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.next_char();
        }

        let mut attrs: Vec<Attr> = Vec::new();
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
                    if let Some(attr) = self.attribute() {
                        // duplicate attributes: first wins
                        if !attr.name.is_empty() && !attrs.iter().any(|a| a.name == attr.name) {
                            attrs.push(attr);
                        }
                    }
                }
            }
        }

        if end {
            Token::EndTag { name }
        } else {
            Token::StartTag {
                name,
                attrs,
                self_closing,
            }
        }
    }

    /// Parse a single attribute starting at a name char. Leaves `pos` at the
    /// char after the value (or after the name for valueless attrs).
    fn attribute(&mut self) -> Option<Attr> {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == '=' || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.next_char();
        }

        self.skip_whitespace();
        if self.peek() != Some('=') {
            // valueless attribute
            return Some(Attr {
                name,
                value: String::new(),
            });
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

        Some(Attr { name, value })
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

            if end == 0 || !body[end..].starts_with(';') {
                return vec!['&']; // malformed: literal '&'
            }
            let radix = if is_hex { 16 } else { 10 };
            let code = u32::from_str_radix(&body[..end], radix).ok();
            // advance past `#`, optional x, digits, and `;`
            self.pos += 1 + digits_start + end + 1;
            let ch = code
                .and_then(char::from_u32)
                .filter(|c| *c != '\0')
                .unwrap_or('\u{FFFD}');
            return vec![ch];
        }

        // named reference (minimal set)
        for (name, ch) in NAMED_REFS {
            if rest.starts_with(name) {
                self.pos += name.len();
                return vec![*ch];
            }
        }

        vec!['&']
    }
}

const NAMED_REFS: &[(&str, char)] = &[
    ("amp;", '&'),
    ("lt;", '<'),
    ("gt;", '>'),
    ("quot;", '"'),
    ("apos;", '\''),
    ("nbsp;", '\u{00A0}'),
];

#[cfg(test)]
mod tests {
    use super::*;

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
                    attrs: vec![attr("class", "a")],
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
                    attrs: vec![],
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
                    attrs: vec![attr("href", "x"), attr("disabled", "")],
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
            vec![Token::EndTag { name: "p".into() }, Token::Eof]
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
                    attrs: vec![attr("class", "X")],
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
}
