//! Simplified HTML tree builder: a stack of open elements driven by a small
//! set of insertion modes. Synthesizes the `html`/`head`/`body` skeleton,
//! handles void elements, minimal `p`/`li`/`dt`/`dd`/`option` auto-closing, and
//! coalesces text. The full mode lattice, adoption agency, and table foster
//! parenting are out of scope (see the M1 design note).

use crate::tokenizer::{Token, Tokenizer};
use starfish_dom::{Attr, Document, NodeId};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
}

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

/// Head-content element names that belong in `<head>`.
fn is_head_element(name: &str) -> bool {
    matches!(
        name,
        "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script"
    )
}

struct TreeBuilder {
    doc: Document,
    open: Vec<NodeId>,
    mode: Mode,
    head: Option<NodeId>,
}

impl TreeBuilder {
    fn new() -> Self {
        TreeBuilder {
            doc: Document::new(),
            open: Vec::new(),
            mode: Mode::Initial,
            head: None,
        }
    }

    /// Whether an `<svg>` element is currently on the open-elements stack, i.e.
    /// we are inside SVG foreign content. Replaces the old `svg_depth` counter
    /// so SVG mode clears automatically when the `<svg>` is popped by ANY
    /// mechanism — explicit `</svg>`, `</div>` truncation, etc. (E9-M1 §1.3).
    fn in_svg(&self) -> bool {
        self.open.iter().any(|&n| self.doc.tag_name(n) == Some("svg"))
    }

    fn current(&self) -> NodeId {
        *self.open.last().expect("open stack non-empty in body modes")
    }

    fn insert_element(&mut self, name: &str, attrs: Vec<Attr>) -> NodeId {
        let el = self.doc.create_element(name);
        if let starfish_dom::NodeKind::Element(e) = &mut self.doc.node_mut(el).kind {
            e.attrs = attrs;
        }
        let parent = self.current();
        self.doc.append_child(parent, el);
        el
    }

    /// Insert an element with its tag/attr names stored case-preserved (SVG
    /// foreign content, E9-M1 §2).
    fn insert_element_ns(&mut self, name: &str, attrs: Vec<Attr>) -> NodeId {
        let el = self.doc.create_element_ns(name);
        if let starfish_dom::NodeKind::Element(e) = &mut self.doc.node_mut(el).kind {
            e.attrs = attrs;
        }
        let parent = self.current();
        self.doc.append_child(parent, el);
        el
    }

    fn insert_text(&mut self, c: char) {
        let parent = self.current();
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        if !self.doc.append_text(parent, s) {
            let t = self.doc.create_text(s);
            self.doc.append_child(parent, t);
        }
    }

    fn insert_comment_under(&mut self, parent: NodeId, data: &str) {
        let c = self.doc.create_comment(data);
        self.doc.append_child(parent, c);
    }

    /// Synthesize `<html>` under the document, push it, → BeforeHead.
    fn synthesize_html(&mut self) {
        let root = self.doc.root();
        let html = self.doc.create_element("html");
        self.doc.append_child(root, html);
        self.open.push(html);
        self.mode = Mode::BeforeHead;
    }

    /// Synthesize `<head>`, push it, record it, → InHead.
    fn synthesize_head(&mut self) {
        let head = self.insert_element("head", Vec::new());
        self.head = Some(head);
        self.open.push(head);
        self.mode = Mode::InHead;
    }

    /// Synthesize `<body>`, push it, → InBody.
    fn synthesize_body(&mut self) {
        let body = self.insert_element("body", Vec::new());
        self.open.push(body);
        self.mode = Mode::InBody;
    }

    fn build(mut self, input: &str) -> Document {
        let mut tok = Tokenizer::new(input);
        loop {
            let token = tok.next_token();
            if matches!(token, Token::Eof) {
                self.process(Token::Eof);
                break;
            }
            self.process(token);
        }
        self.doc
    }

    /// Process one token in the current mode. Some branches re-enter `process`
    /// to "reprocess" the token after switching modes (the spec's reprocess).
    fn process(&mut self, token: Token) {
        match self.mode {
            Mode::Initial => self.in_initial(token),
            Mode::BeforeHtml => self.in_before_html(token),
            Mode::BeforeHead => self.in_before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::AfterHead => self.in_after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::AfterBody => self.in_after_body(token),
        }
    }

    fn in_initial(&mut self, token: Token) {
        match token {
            Token::Doctype { name } => {
                let root = self.doc.root();
                let dt = self.doc.create_doctype(&name);
                self.doc.append_child(root, dt);
                self.mode = Mode::BeforeHtml;
            }
            Token::Comment(data) => {
                let root = self.doc.root();
                self.insert_comment_under(root, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            other => {
                self.mode = Mode::BeforeHtml;
                self.process(other);
            }
        }
    }

    fn in_before_html(&mut self, token: Token) {
        match token {
            Token::Comment(data) => {
                let root = self.doc.root();
                self.insert_comment_under(root, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            Token::StartTag { ref name, .. } if name == "html" => {
                let root = self.doc.root();
                let html = self.doc.create_element("html");
                self.doc.append_child(root, html);
                self.open.push(html);
                self.mode = Mode::BeforeHead;
            }
            Token::EndTag { ref name, .. } if !matches!(name.as_str(), "html" | "head" | "body") => {
                // stray end tag before html: ignore
            }
            other => {
                self.synthesize_html();
                self.process(other);
            }
        }
    }

    fn in_before_head(&mut self, token: Token) {
        match token {
            Token::Comment(data) => {
                let parent = self.current();
                self.insert_comment_under(parent, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            Token::StartTag { ref name, .. } if name == "head" => {
                let head = self.insert_element("head", Vec::new());
                self.head = Some(head);
                self.open.push(head);
                self.mode = Mode::InHead;
            }
            other => {
                self.synthesize_head();
                self.process(other);
            }
        }
    }

    fn in_head(&mut self, token: Token) {
        // If a head-child element (e.g. <title>, <style>) is open, its content
        // is parsed as ordinary markup (no RCDATA in M1): insert under it.
        let in_head_child = self.head != Some(self.current());

        match token {
            Token::Character(c) if in_head_child => self.insert_text(c),
            Token::Comment(data) => {
                let parent = self.current();
                self.insert_comment_under(parent, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            Token::StartTag {
                name, attrs, ..
            } if is_head_element(&name) => {
                if is_void(&name) {
                    self.insert_element(&name, attrs);
                } else {
                    let el = self.insert_element(&name, attrs);
                    self.open.push(el);
                }
            }
            Token::EndTag { ref name, .. } if name == "head" => {
                self.open.pop();
                self.mode = Mode::AfterHead;
            }
            Token::EndTag { ref name, .. }
                if matches!(name.as_str(), "base" | "link" | "meta" | "title" | "style") =>
            {
                // pop non-void head element if it is current
                if self.doc.tag_name(self.current()) == Some(name.as_str()) {
                    self.open.pop();
                }
            }
            other => {
                // anything not belonging in head: pop head, → AfterHead, reprocess
                self.open.pop();
                self.mode = Mode::AfterHead;
                self.process(other);
            }
        }
    }

    fn in_after_head(&mut self, token: Token) {
        match token {
            Token::Comment(data) => {
                let parent = self.current();
                self.insert_comment_under(parent, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            Token::StartTag { ref name, .. } if name == "body" => {
                let attrs = match token {
                    Token::StartTag { attrs, .. } => attrs,
                    _ => unreachable!(),
                };
                let body = self.insert_element("body", attrs);
                self.open.push(body);
                self.mode = Mode::InBody;
            }
            Token::StartTag { ref name, .. } if is_head_element(name) => {
                // late head element: process in head temporarily
                if let Some(head) = self.head {
                    self.open.push(head);
                    self.mode = Mode::InHead;
                    self.process(token);
                    // pop head back off; if in_head left it pushed, drop it
                    while self.current() != head {
                        self.open.pop();
                    }
                    self.open.pop();
                    self.mode = Mode::AfterHead;
                }
            }
            other => {
                self.synthesize_body();
                self.process(other);
            }
        }
    }

    fn in_body(&mut self, token: Token) {
        match token {
            Token::Character(c) => self.insert_text(c),
            Token::Comment(data) => {
                let parent = self.current();
                self.insert_comment_under(parent, &data);
            }
            Token::Doctype { .. } => {} // stray doctype in body: ignore
            Token::StartTag {
                name,
                name_raw,
                attrs,
                attrs_raw,
                self_closing,
            } => {
                // SVG foreign content: in SVG mode (or entering it via <svg>),
                // store the element case-preserved, honor self-closing, and skip
                // HTML auto-close/void logic (E9-M1 §1.3). The `<svg>` element
                // itself isn't on `open` yet, so gate on `name == "svg"` too.
                let svg = name == "svg" || self.in_svg();
                if svg {
                    let el = self.insert_element_ns(&name_raw, attrs_raw);
                    if !self_closing {
                        self.open.push(el);
                    }
                } else {
                    self.auto_close_for(&name);
                    if is_void(&name) {
                        self.insert_element(&name, attrs);
                    } else {
                        let el = self.insert_element(&name, attrs);
                        self.open.push(el);
                    }
                }
            }
            Token::EndTag { name, name_raw } => {
                if self.in_svg() {
                    if name == "svg" {
                        self.close_element("svg");
                    } else {
                        // Case-sensitive match against the case-preserved stack.
                        self.close_element(&name_raw);
                    }
                    return;
                }
                if name == "body" || name == "html" {
                    self.mode = Mode::AfterBody;
                    return;
                }
                self.close_element(&name);
            }
            Token::Eof => {
                self.open.clear();
            }
        }
    }

    fn in_after_body(&mut self, token: Token) {
        match token {
            Token::Comment(data) => {
                // trailing comments go under <html> (open[0])
                let html = self.open.first().copied().unwrap_or_else(|| self.doc.root());
                self.insert_comment_under(html, &data);
            }
            Token::Character(c) if c.is_ascii_whitespace() => {}
            Token::Eof => {
                self.open.clear();
            }
            other => {
                // stray content after body: reprocess in body (lenient)
                self.mode = Mode::InBody;
                self.process(other);
            }
        }
    }

    /// Minimal auto-close: a new start tag closes a currently-open implied
    /// element. `<p>`-ish block tags close an open `<p>`; list/def items close
    /// a same-kind open item; `<option>` closes an open `<option>`.
    fn auto_close_for(&mut self, name: &str) {
        // a <p> in scope is closed by p and common block tags
        const CLOSES_P: &[&str] = &[
            "p", "address", "article", "aside", "blockquote", "div", "dl", "fieldset", "figure",
            "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "main", "nav",
            "ol", "pre", "section", "table", "ul",
        ];
        if CLOSES_P.contains(&name) && self.open.iter().any(|&n| self.doc.tag_name(n) == Some("p")) {
            self.close_element("p");
        }
        // a new <li> closes the nearest open <li>, even across block elements
        // (e.g. <li><div>a<li>b nests the second li under <ul>) — but NOT across
        // a nested list: if a <ul>/<ol> was opened inside the current <li>, the
        // new <li> belongs to that inner list, so leave the outer <li> open.
        if name == "li" {
            if let Some(li_idx) = self
                .open
                .iter()
                .rposition(|&n| self.doc.tag_name(n) == Some("li"))
            {
                let nested_list = self.open[li_idx + 1..]
                    .iter()
                    .any(|&n| matches!(self.doc.tag_name(n), Some("ul") | Some("ol")));
                if !nested_list {
                    self.open.truncate(li_idx);
                }
            }
        }
        // a new <dt>/<dd> closes an open <dt> or <dd> anywhere on the stack.
        if matches!(name, "dt" | "dd") {
            if self.open_has("dt") {
                self.close_element("dt");
            } else if self.open_has("dd") {
                self.close_element("dd");
            }
        }
        if name == "option" && self.open_has("option") {
            self.close_element("option");
        }
    }

    /// Whether an element named `name` is anywhere on the open stack.
    fn open_has(&self, name: &str) -> bool {
        self.open.iter().any(|&n| self.doc.tag_name(n) == Some(name))
    }

    /// Search the open stack top-down for `name`; if found, pop through it.
    /// Void elements are never on the stack, so their end tags are ignored.
    fn close_element(&mut self, name: &str) {
        if let Some(idx) = self
            .open
            .iter()
            .rposition(|&n| self.doc.tag_name(n) == Some(name))
        {
            self.open.truncate(idx);
        }
        // not found: stray end tag, ignore
    }
}

/// Parse an HTML document string into a DOM `Document`. Infallible/lenient.
pub fn parse(html: &str) -> Document {
    TreeBuilder::new().build(html)
}
