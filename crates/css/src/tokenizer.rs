//! Hand-rolled CSS Syntax-subset tokenizer.
//!
//! A byte-offset scanner over the input `&str`. Comments `/* … */` are skipped.
//! The parser pulls tokens one at a time and uses [`Tokenizer::pos`] to slice
//! the original source for `Value.raw`.

/// A pragmatic subset of CSS Syntax Level 3 tokens.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// `div`, `color` — case preserved; the parser lowercases where needed.
    Ident(String),
    /// `rgb(` → `Function("rgb")` (ident immediately followed by `(`).
    Function(String),
    /// `@media` → `AtKeyword("media")`.
    AtKeyword(String),
    /// `#main`, `#fff` — stores text after `#`.
    Hash(String),
    /// `"…"` / `'…'` — decoded, quotes stripped.
    Str(String),
    /// `12`, `1.5`, `-3`.
    Number(f32),
    /// `50%` → stores `50.0`.
    Percentage(f32),
    /// `12px`, `1.5em` — unit lowercased.
    Dimension { value: f32, unit: String },
    /// A single non-token char: `.`, `*`, `>`, `+`, `~`, `!`, `=`, `/`, …
    Delim(char),
    /// A run of whitespace, collapsed to one token.
    Whitespace,
    Colon,
    Semicolon,
    Comma,
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Eof,
}

pub struct Tokenizer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Tokenizer {
            input,
            bytes: input.as_bytes(),
            pos: 0,
        }
    }

    /// Current byte offset (start of the next token, after comment skipping is
    /// only applied inside `next_token`).
    pub fn pos(&self) -> usize {
        self.pos
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// Pull the next token. Returns [`Token::Eof`] at end (idempotent).
    pub fn next_token(&mut self) -> Token {
        self.skip_comments();
        let b = match self.peek() {
            Some(b) => b,
            None => return Token::Eof,
        };

        if is_whitespace(b) {
            self.consume_whitespace();
            return Token::Whitespace;
        }

        match b {
            b'{' => self.single(Token::LeftBrace),
            b'}' => self.single(Token::RightBrace),
            b'(' => self.single(Token::LeftParen),
            b')' => self.single(Token::RightParen),
            b'[' => self.single(Token::LeftBracket),
            b']' => self.single(Token::RightBracket),
            b':' => self.single(Token::Colon),
            b';' => self.single(Token::Semicolon),
            b',' => self.single(Token::Comma),
            b'"' | b'\'' => self.consume_string(b),
            b'#' => self.consume_hash(),
            b'@' => self.consume_at(),
            b'+' | b'-' | b'.' if self.is_number_start() => self.consume_numeric(),
            b'0'..=b'9' => self.consume_numeric(),
            _ if is_ident_start(b) => self.consume_ident_like(),
            _ => {
                // Single delim char. Advance by one byte; for multi-byte
                // non-ASCII handled by ident-start above, so here b is ASCII.
                self.pos += 1;
                Token::Delim(b as char)
            }
        }
    }

    fn single(&mut self, tok: Token) -> Token {
        self.pos += 1;
        tok
    }

    /// Skip any run of `/* … */` comments and the whitespace is *not* skipped
    /// here (whitespace is a significant token). Only comments are consumed.
    fn skip_comments(&mut self) {
        while self.peek() == Some(b'/') && self.peek_at(1) == Some(b'*') {
            self.pos += 2;
            loop {
                match self.peek() {
                    None => return, // unterminated → consume to EOF
                    Some(b'*') if self.peek_at(1) == Some(b'/') => {
                        self.pos += 2;
                        break;
                    }
                    _ => self.pos += 1,
                }
            }
        }
    }

    /// Consume a whitespace run, absorbing any comments interleaved within it
    /// so that `ws /*c*/ ws` collapses to a single `Whitespace` token.
    fn consume_whitespace(&mut self) {
        loop {
            while matches!(self.peek(), Some(b) if is_whitespace(b)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'/') && self.peek_at(1) == Some(b'*') {
                self.skip_comments();
                continue;
            }
            break;
        }
    }

    fn consume_string(&mut self, quote: u8) -> Token {
        self.pos += 1; // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => break, // unterminated → close at EOF
                Some(b) if b == quote => break,
                Some(b'\\') => {
                    // minimal escapes: \" \' \\, else copy next char verbatim.
                    if let Some(esc) = self.bump() {
                        self.push_byte(&mut s, esc);
                    }
                }
                Some(b) => self.push_byte(&mut s, b),
            }
        }
        Token::Str(s)
    }

    /// Push a (possibly multi-byte UTF-8 lead) byte we already bumped. For
    /// non-ASCII we need to copy the whole char from the source slice.
    fn push_byte(&mut self, s: &mut String, b: u8) {
        if b < 0x80 {
            s.push(b as char);
        } else {
            // `self.pos` is just past `b`. Find the char that starts at pos-1.
            let start = self.pos - 1;
            let rest = &self.input[start..];
            if let Some(c) = rest.chars().next() {
                s.push(c);
                self.pos = start + c.len_utf8();
            }
        }
    }

    fn consume_hash(&mut self) -> Token {
        self.pos += 1; // '#'
        let name = self.consume_name();
        if name.is_empty() {
            Token::Delim('#')
        } else {
            Token::Hash(name)
        }
    }

    fn consume_at(&mut self) -> Token {
        // '@' followed by an ident → AtKeyword, else a bare delim.
        if matches!(self.peek_at(1), Some(b) if is_ident_start(b)) {
            self.pos += 1; // '@'
            Token::AtKeyword(self.consume_name())
        } else {
            self.pos += 1;
            Token::Delim('@')
        }
    }

    fn consume_ident_like(&mut self) -> Token {
        let name = self.consume_name();
        if self.peek() == Some(b'(') {
            self.pos += 1; // consume '('
            Token::Function(name)
        } else {
            Token::Ident(name)
        }
    }

    /// Consume an ident-name run (letters/digits/`_`/`-`/non-ASCII).
    fn consume_name(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_ident_char(b) {
                self.pos += 1;
            } else if b >= 0x80 {
                // advance one full UTF-8 char
                let c = self.input[self.pos..].chars().next().unwrap();
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        self.input[start..self.pos].to_string()
    }

    /// `true` if the upcoming `+`/`-`/`.` begins a number.
    fn is_number_start(&self) -> bool {
        match self.peek() {
            Some(b'+') | Some(b'-') => {
                // sign then digit, or sign then `.digit`
                match self.peek_at(1) {
                    Some(b'0'..=b'9') => true,
                    Some(b'.') => matches!(self.peek_at(2), Some(b'0'..=b'9')),
                    _ => false,
                }
            }
            Some(b'.') => matches!(self.peek_at(1), Some(b'0'..=b'9')),
            _ => false,
        }
    }

    fn consume_numeric(&mut self) -> Token {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        self.consume_digits();
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
            self.pos += 1;
            self.consume_digits();
        }
        // exponent (lenient)
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(b'0'..=b'9')) {
                self.consume_digits();
            } else {
                self.pos = save; // not an exponent; back off
            }
        }
        let num: f32 = self.input[start..self.pos].parse().unwrap_or(0.0);

        // suffix: `%` → Percentage; ident-start → Dimension; else Number.
        if self.peek() == Some(b'%') {
            self.pos += 1;
            Token::Percentage(num)
        } else if matches!(self.peek(), Some(b) if is_ident_start(b)) {
            let unit = self.consume_name().to_ascii_lowercase();
            Token::Dimension { value: num, unit }
        } else {
            Token::Number(num)
        }
    }

    fn consume_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Ident *start*: letter, `_`, `-`, or non-ASCII.
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'-' || b >= 0x80
}

/// Ident continue: ident-start chars plus digits.
fn is_ident_char(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(s);
        let mut out = Vec::new();
        loop {
            let tok = t.next_token();
            let eof = tok == Token::Eof;
            out.push(tok);
            if eof {
                break;
            }
        }
        out
    }

    #[test]
    fn ident() {
        assert_eq!(lex("div"), vec![Token::Ident("div".into()), Token::Eof]);
    }

    #[test]
    fn class_delim() {
        assert_eq!(
            lex(".foo"),
            vec![Token::Delim('.'), Token::Ident("foo".into()), Token::Eof]
        );
    }

    #[test]
    fn hash() {
        assert_eq!(lex("#bar"), vec![Token::Hash("bar".into()), Token::Eof]);
    }

    #[test]
    fn numbers() {
        assert_eq!(
            lex("12px"),
            vec![
                Token::Dimension {
                    value: 12.0,
                    unit: "px".into()
                },
                Token::Eof
            ]
        );
        assert_eq!(lex("50%"), vec![Token::Percentage(50.0), Token::Eof]);
        assert_eq!(lex("1.5"), vec![Token::Number(1.5), Token::Eof]);
        assert_eq!(lex("-3"), vec![Token::Number(-3.0), Token::Eof]);
    }

    #[test]
    fn function_and_args() {
        assert_eq!(
            lex("rgb(0,0,0)"),
            vec![
                Token::Function("rgb".into()),
                Token::Number(0.0),
                Token::Comma,
                Token::Number(0.0),
                Token::Comma,
                Token::Number(0.0),
                Token::RightParen,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn string_with_space() {
        assert_eq!(
            lex("\"Helvetica Neue\""),
            vec![Token::Str("Helvetica Neue".into()), Token::Eof]
        );
    }

    #[test]
    fn comment_dropped() {
        assert_eq!(
            lex("a /* c */ b"),
            vec![
                Token::Ident("a".into()),
                Token::Whitespace,
                Token::Ident("b".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn assorted_delims() {
        assert_eq!(
            lex(">+~*!"),
            vec![
                Token::Delim('>'),
                Token::Delim('+'),
                Token::Delim('~'),
                Token::Delim('*'),
                Token::Delim('!'),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn structural_tokens() {
        assert_eq!(
            lex("{}();:,"),
            vec![
                Token::LeftBrace,
                Token::RightBrace,
                Token::LeftParen,
                Token::RightParen,
                Token::Semicolon,
                Token::Colon,
                Token::Comma,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn unterminated_string() {
        assert_eq!(lex("\"abc"), vec![Token::Str("abc".into()), Token::Eof]);
    }

    #[test]
    fn unterminated_comment() {
        assert_eq!(lex("/* abc"), vec![Token::Eof]);
    }
}

