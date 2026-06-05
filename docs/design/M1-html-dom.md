# M1 — HTML → DOM design note

Scope: a DOM node tree (`starfish-dom`) and a hand-rolled HTML tokenizer + tree
builder (`starfish-html`) that turns an HTML document string into a DOM tree.
No JavaScript, no networking. We target a pragmatic subset of the WHATWG HTML
spec — enough to parse typical static pages — and explicitly defer the rest.

Guiding rule (project "Simplicity First"): design only what M1 needs. Every
type below has a consumer in M1 or in the very next milestone (M2/M3 read the
DOM). No speculative abstraction.

---

## 1. DOM data model

### 1.1 Arena vs `Rc<RefCell>` — decision

**Use an arena: `Vec<Node>` indexed by a `u32`-newtype `NodeId`.** Rejected the
`Rc<RefCell<Node>>` graph.

Justification:

- **No borrow-checker fight.** A DOM is a cyclic graph (parent ⇄ child). With
  `Rc<RefCell>` every traversal is a runtime-borrow dance, parent links need
  `Weak`, and a stray double-borrow panics at runtime. The arena makes links
  plain integers: mutating one node while reading another is trivially legal.
- **Cheap + cache-friendly.** Nodes live contiguously in one `Vec`; a `NodeId`
  is a 4-byte `Copy` value, so links cost nothing to clone/store and traversal
  is mostly linear memory access. No per-node heap allocation or refcount
  traffic.
- **Trivial to pass around.** `NodeId` is `Copy + Eq + Hash`, so later
  milestones (style, layout) can key maps on it and hold "references" into the
  tree without lifetime entanglement.
- **Simple ownership.** The whole tree is owned by one `Document`; dropping it
  frees everything at once. No cycles to leak.

Cost we accept: nodes are not freed individually (fine — M1 never removes
nodes), and a `NodeId` is only meaningful together with its `Document`.

### 1.2 Types

```rust
/// Index into `Document::nodes`. 4 bytes, Copy. Only valid for the Document
/// that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);

pub struct Node {
    pub kind: NodeKind,
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
}

pub enum NodeKind {
    Document,
    Doctype(Doctype),
    Element(Element),
    Text(String),
    Comment(String),
}

pub struct Doctype {
    pub name: String,        // e.g. "html"
    // public_id / system_id omitted for M1 (legacy doctypes are out of scope).
}

pub struct Element {
    pub name: String,             // lowercased tag name, e.g. "div"
    pub attrs: Vec<Attr>,
    // No namespace field in M1: HTML-namespace only. See non-goals.
}

pub struct Attr {
    pub name: String,   // lowercased
    pub value: String,
}
```

Notes on field choices:

- **Sibling links (doubly-linked child list)** rather than a `Vec<NodeId>` of
  children per node. The tree builder appends one child at a time and never
  random-indexes; a linked list makes `append_child` O(1) and avoids a `Vec`
  per element. Iteration is via `first_child` + `next_sibling`.
- **Attributes as `Vec<Attr>`**, not a map. Element attribute counts are tiny,
  order matters for serialization, and the spec keeps the *first* occurrence of
  a duplicate attribute — both trivial with a `Vec`. `get_attribute` is a linear
  scan, which is fine at these sizes.
- `Text` / `Comment` carry their `String` inline in the `NodeKind` variant — no
  separate struct needed.

### 1.3 Document / arena public API

```rust
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,   // the NodeKind::Document node, always nodes[0]
}

impl Document {
    /// New document containing only the Document root node.
    pub fn new() -> Document;

    pub fn root(&self) -> NodeId;

    // --- node creation (returns a detached node owned by the arena) ---
    pub fn create_element(&mut self, name: &str) -> NodeId;
    pub fn create_text(&mut self, data: &str) -> NodeId;
    pub fn create_comment(&mut self, data: &str) -> NodeId;
    pub fn create_doctype(&mut self, name: &str) -> NodeId;

    // --- tree mutation ---
    /// Append `child` as the last child of `parent`. `child` must be detached.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId);

    // --- access ---
    pub fn node(&self, id: NodeId) -> &Node;
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node;

    pub fn kind(&self, id: NodeId) -> &NodeKind;

    pub fn parent(&self, id: NodeId) -> Option<NodeId>;
    pub fn first_child(&self, id: NodeId) -> Option<NodeId>;
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId>;

    /// Collect children in order (convenience; allocates).
    pub fn children(&self, id: NodeId) -> Vec<NodeId>;

    // --- element helpers ---
    /// Tag name if `id` is an element, else None.
    pub fn tag_name(&self, id: NodeId) -> Option<&str>;
    /// First attribute value with this (already-lowercased) name.
    pub fn get_attribute(&self, id: NodeId, name: &str) -> Option<&str>;
}
```

`children` is the only allocating accessor and exists for ergonomic consumers
and tests; hot traversal uses `first_child`/`next_sibling`.

Internal-only helper used by the tree builder for text coalescing:

```rust
impl Document {
    /// If `parent`'s last child is a Text node, push `s` onto it and return
    /// true; otherwise return false (caller then creates a new Text node).
    pub(crate) fn append_text(&mut self, parent: NodeId, s: &str) -> bool;
}
```

---

## 2. HTML tokenizer

A character-driven state machine over the input `&str` (decoded as UTF-8;
input is already a Rust `&str` so encoding sniffing is out of scope). Produces a
stream of tokens pulled one at a time by the tree builder.

### 2.1 Token types

```rust
pub enum Token {
    Doctype { name: String },
    StartTag { name: String, attrs: Vec<Attr>, self_closing: bool },
    EndTag { name: String },
    Character(char),
    Comment(String),
    Eof,
}
```

`Attr` is reused from `starfish-dom` (re-exported). Tag and attribute names are
lowercased by the tokenizer before emission. `Character` is emitted per
character; the tree builder coalesces runs into text nodes (§3). (Emitting
per-char keeps the tokenizer dead simple; coalescing is cheap and lives in one
place.)

### 2.2 States (pragmatic subset)

Spec state names, only the ones we implement:

- **Data** — default. `<` → TagOpen. `&` → consume a character reference (§2.3)
  and emit the result as Character(s). Anything else → emit `Character(c)`.
- **TagOpen** — after `<`. `/` → EndTagOpen. ASCII alpha → start a StartTag,
  reconsume in TagName. `!` → MarkupDeclarationOpen. Else → emit `<` as a
  character, reconsume in Data (parse error, lenient).
- **EndTagOpen** — after `</`. ASCII alpha → start an EndTag, reconsume in
  TagName. `>` → ignore (empty end tag). Else → treat as a bogus comment.
- **TagName** — accumulate lowercased name. whitespace → BeforeAttributeName.
  `/` → SelfClosingStartTag. `>` → emit current tag, → Data.
- **BeforeAttributeName** — skip whitespace. `/` → SelfClosingStartTag. `>` →
  emit, → Data. Else → start an attribute, reconsume in AttributeName.
- **AttributeName** — accumulate lowercased name. whitespace → AfterAttributeName.
  `=` → BeforeAttributeValue. `/` or `>` → finish attr, reconsume in
  BeforeAttributeName.
- **AfterAttributeName** — skip whitespace. `=` → BeforeAttributeValue. `/` →
  SelfClosingStartTag. `>` → emit. Else → new attribute (value defaults to "").
- **BeforeAttributeValue** — skip whitespace. `"` → AttributeValueDoubleQuoted.
  `'` → AttributeValueSingleQuoted. `>` → emit (missing value). Else →
  reconsume in AttributeValueUnquoted.
- **AttributeValueDoubleQuoted / SingleQuoted** — accumulate until matching
  quote → AfterAttributeValueQuoted. `&` → character reference into the value.
- **AttributeValueUnquoted** — accumulate until whitespace (→ BeforeAttributeName)
  or `>` (→ emit). `&` → character reference into the value.
- **AfterAttributeValueQuoted** — whitespace → BeforeAttributeName. `/` →
  SelfClosingStartTag. `>` → emit. Else → reconsume in BeforeAttributeName.
- **SelfClosingStartTag** — `>` → set `self_closing = true`, emit, → Data. Else
  → reconsume in BeforeAttributeName.
- **MarkupDeclarationOpen** — after `<!`. If the next chars are `--` →
  CommentStart. If they (case-insensitively) spell `doctype` → DOCTYPE. Else →
  BogusComment.
- **Comment** (`<!-- ... -->`) — accumulate until `-->`; emit `Comment`.
- **BogusComment** — accumulate until `>`; emit `Comment` (used for `<!`-junk
  and `</`-junk). Lenient recovery, no panic.
- **Doctype** — after `<!doctype`, skip whitespace, read a name until whitespace
  or `>`, skip the rest until `>`, emit `Doctype { name }`. We do **not** parse
  public/system identifiers — read-and-discard to the closing `>`.

Tokenizer struct sketch:

```rust
pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,        // byte offset
    state: State,
    // partial token being built (current tag name/attrs, current comment, ...)
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self;
    /// Pull the next token. Returns Token::Eof at end (idempotent).
    pub fn next_token(&mut self) -> Token;
}
```

**Out of M1 tokenizer:** RAWTEXT / RCDATA / script-data states. `<script>`,
`<style>`, `<textarea>` content is tokenized as ordinary markup in M1. This is
acceptable because M1 has no JS and no CSS-in-DOM execution; it is listed as a
known limitation (§5) and is the first thing M2+ may revisit.

### 2.3 Character references

Minimal set only:

- Named: `&amp;` → `&`, `&lt;` → `<`, `&gt;` → `>`, `&quot;` → `"`,
  `&apos;` → `'`, `&nbsp;` → U+00A0.
- Numeric decimal: `&#NN;` (e.g. `&#65;` → `A`).
- Numeric hex: `&#xHH;` / `&#XHH;` (e.g. `&#x41;` → `A`).

Resolution rules (lenient): a reference must be terminated by `;`. If the text
after `&` does not match one of the above, the `&` is emitted literally and
scanning resumes at the next character (no error, no consumption of the rest).
Numeric values that are invalid/out-of-range map to U+FFFD. The full WHATWG
named-entity table (2000+ names) is explicitly **not** included in M1.

---

## 3. Tree builder

Consumes the token stream and builds the `Document`. We use a **stripped-down
insertion-mode model** with a **stack of open elements**, but only the handful
of modes a static page needs. We do not implement the full mode lattice,
active-formatting-elements list, adoption agency, or template/table foster
parenting.

### 3.1 State

```rust
struct TreeBuilder {
    doc: Document,
    open: Vec<NodeId>,      // stack of open elements; open[0] is <html>
    mode: Mode,
    head: Option<NodeId>,
}

enum Mode {
    Initial,        // expecting doctype
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
}
```

"Current node" = `*open.last()`. New nodes are appended under the current node.

### 3.2 Implied html/head/body

The builder synthesizes the document skeleton when tokens imply it, mirroring
the spec's lenient behavior:

- **Initial:** a `Doctype` token → append a Doctype node to Document, → BeforeHtml.
  A comment → append to Document. Whitespace → ignore. Anything else →
  (no doctype: tolerated) reprocess in BeforeHtml.
- **BeforeHtml:** a `<html>` start tag → create it. Anything else (or `<head>`,
  text, etc.) → synthesize `<html>`, push it, reprocess the token in BeforeHead.
- **BeforeHead:** `<head>` → create/push it, record `head`, → InHead. Whitespace
  → ignore. Anything else → synthesize `<head>`, → reprocess in InHead.
- **InHead:** head-content tags (`meta`, `link`, `title`, `base`, `style`) →
  insert; void ones don't push. `</head>` → pop, → AfterHead. Anything not
  belonging in head (e.g. text, `<body>`, `<p>`) → pop head, → AfterHead and
  reprocess.
- **AfterHead:** `<body>` → create/push, → InBody. Whitespace → ignore. Anything
  else → synthesize `<body>`, push, → reprocess in InBody.
- **InBody:** the main mode (below).
- **AfterBody / done:** trailing comments appended to `<html>`; stray content
  reprocessed in InBody (lenient).

This guarantees every parsed document has the shape
`Document > html > (head, body)` even for input like `<p>hi`.

### 3.3 InBody handling

- **Start tag:**
  - If it is a **void element** (see set) → create the element, append under
    current node, **do not push** onto the open stack, ignore any explicit end
    tag later.
  - If it triggers **auto-close** (see §3.4) → close the implied-open element
    first, then insert.
  - Otherwise → create, append, push onto `open`.
  - `self_closing` on a non-void element is ignored (treated as a normal start
    tag, per HTML — foreign content is out of scope).
- **End tag:** search the open stack top-down for a matching element. If found,
  pop everything up to and including it. If not found, ignore (stray end tag).
  This is the simplified "generate implied end tags" — good enough for static
  pages, no adoption-agency reparenting.
- **Character:** insert into the current node via text coalescing (§3.5).
- **Comment:** append a Comment node under the current node.
- **Eof:** pop the whole stack; done.

### 3.4 Void elements & auto-closing

Void elements (never have children, never pushed):

```
area, base, br, col, embed, hr, img, input, link, meta, param, source, track, wbr
```

Auto-close rules (minimal, the two that matter for real pages):

- A `<p>` start tag (and most block start tags) while a `<p>` is open in scope →
  close the open `<p>` first.
- A `<li>` start tag while a `<li>` is open → close it. Same idea for `<dt>`/`<dd>`.
- An `<option>` start tag while an `<option>` is open → close it.

These are implemented as a small lookup: "starting tag X implies closing
currently-open tag(s) in set S." We keep the set short and documented rather
than encoding the full spec table. Table-related auto-closing (`<tr>`, `<td>`,
foster parenting) is **out of scope** for M1 (tables parse as plain nested
elements, which is structurally lenient but acceptable).

### 3.5 Text node coalescing

Adjacent `Character` tokens must not produce one Text node per char. On a
`Character`, the builder calls `Document::append_text(current, s)`: if the
current node's last child is already a `Text` node it appends to that string;
otherwise it creates a new `Text` node. Result: each contiguous run of text
between tags is a single Text node.

(Whitespace-only text is kept as-is in M1 — whitespace handling/normalization is
a layout concern, deferred to M4.)

---

## 4. Crate boundaries & dependencies

- **`starfish-dom`** — no dependencies. Owns `Document`, `NodeId`, `Node`,
  `NodeKind`, `Element`, `Attr`, `Doctype`, and the arena API. Pure data + tree
  ops; knows nothing about HTML parsing.
- **`starfish-html`** — depends on `starfish-dom`. Owns `Tokenizer`, `Token`,
  the tree builder, and character-reference decoding. `Attr` is re-exported from
  `starfish-dom` so token attrs move into the DOM without conversion.

`crates/html/Cargo.toml` gains:

```toml
[dependencies]
starfish-dom = { workspace = true }
```

(The workspace already declares `starfish-dom` under
`[workspace.dependencies]`; html currently has none — this edge needs to be
added during implementation.)

### Public entry point

```rust
// starfish-html
pub fn parse(html: &str) -> starfish_dom::Document;
```

One call, no config, no `Result` (the parser is infallible/lenient — malformed
input recovers rather than erroring, matching HTML). The returned `Document` has
its `Document` root populated with the parsed tree.

Re-exports for ergonomic callers: `pub use starfish_dom::{Document, NodeId};`
and `pub mod tokenizer { pub use ... Tokenizer, Token }` so tests can exercise
the tokenizer alone.

---

## 5. Edge cases & explicit non-goals

Handled (lenient recovery, no panic):

- Missing doctype; doctype with junk after the name.
- Implied `<html>`/`<head>`/`<body>` (e.g. bare `<p>hi`).
- Unclosed tags at EOF (open stack popped).
- Stray/mismatched end tags (ignored).
- Unquoted, single-quoted, double-quoted, and valueless attributes.
- Duplicate attributes (first wins).
- Self-closing syntax on void (`<br/>`) and non-void (`<div/>` → normal start).
- Comments, including unterminated comments at EOF.
- The minimal character-reference set, in text and attribute values.
- Unknown/custom element names (treated as ordinary elements).

Explicit **non-goals** for M1 (documented limitations):

- No JavaScript, no `<script>` execution, no `document.write`.
- No networking / external resource fetch.
- No RAWTEXT/RCDATA states: `<script>`, `<style>`, `<textarea>`, `<title>`
  content is parsed as markup, not raw text.
- No full named-entity table (only the ~6 entities in §2.3).
- No namespaces (SVG/MathML foreign content); no `Element` namespace field.
- No table foster-parenting, no active-formatting-elements / adoption agency
  (mis-nested `<b>`/`<i>` are not reconstructed across block boundaries).
- No DOCTYPE public/system identifiers; no quirks-mode flag.
- No node removal / mutation API beyond `append_child` (arena never frees).
- No encoding detection (input is already `&str`).
- No source-location tracking on nodes.

---

## 6. Test plan

Unit tests live in `crates/dom/src/lib.rs` (arena) and `crates/html` (tokenizer
+ parse). A small test helper serializes a `Document` to an indented S-expr-ish
string so expected trees are readable in assertions, e.g.:

```
(document
  (doctype html)
  (element html
    (element head)
    (element body
      (element p "hello"))))
```

### 6.1 `starfish-dom` arena tests

1. `create_element` + `append_child` builds parent/child/sibling links;
   `children()` returns insertion order.
2. `get_attribute` returns the first matching value; missing → `None`.
3. `tag_name` is `None` for non-elements.
4. `append_text` coalesces two consecutive text appends into one Text node, but
   creates a new node when a non-text child intervenes.

### 6.2 Tokenizer tests (snippet → token sequence)

1. `Hello` → `[Character('H'), ... , Eof]`.
2. `<div class="a">` → `StartTag{ name:"div", attrs:[("class","a")], self_closing:false }`.
3. `<br/>` → `StartTag{ name:"br", self_closing:true }`.
4. `<a href='x' disabled>` → attrs `[("href","x"), ("disabled","")]`.
5. `</p>` → `EndTag{ name:"p" }`.
6. `<!-- hi -->` → `Comment(" hi ")`.
7. `<!DOCTYPE html>` → `Doctype{ name:"html" }`.
8. `a&amp;b&lt;c&#65;&#x42;` → characters `a & b < c A B`.
9. Uppercase normalization: `<DIV CLASS=X>` → name `div`, attr `("class","x"-as-written)`
   (tag/attr **names** lowercased; attribute **values** kept verbatim — so value is `X`).
10. Unterminated comment `<!-- oops` at EOF → `Comment(" oops")` then `Eof` (no panic).

### 6.3 `parse` tree-shape tests

1. **Full doc:**
   `<!DOCTYPE html><html><head><title>T</title></head><body><p>Hi</p></body></html>`
   → `document > doctype + html > (head > title "T", body > p "Hi")`.
2. **Implied skeleton:** `<p>hi` → `document > html > (head, body > p "hi")`.
3. **Void element:** `<div><br>text</div>` → `div` has children `[br, text]`,
   `br` is a leaf, `text` is sibling of `br` (not child).
4. **Auto-close `<p>`:** `<p>one<p>two` → `body > (p "one", p "two")` (two
   sibling paragraphs, not nested).
5. **Auto-close `<li>`:** `<ul><li>a<li>b</ul>` → `ul > (li "a", li "b")`.
6. **Text coalescing across a reference:** `a&amp;b` inside `<p>` → a single
   Text node `"a&b"`.
7. **Stray end tag:** `<p>x</div>y</p>` → `p` contains text `xy` (the unmatched
   `</div>` ignored), well-formed tree, no panic.
8. **Attributes reach the DOM:** `<a href="u">L</a>` →
   `get_attribute(a, "href") == Some("u")`.
9. **Comment placement:** `<body><!--c--></body>` → `body` has one Comment child.
10. **Unclosed at EOF:** `<div><span>hi` → `div > span > "hi"`, tree closed
    cleanly at EOF.

A representative "typical static page" fixture (heading, paragraph, list, link,
image, a couple of nested divs) is parsed and its serialized shape asserted as
an end-to-end smoke test — this is the milestone's "done-when" check.

---

## 7. Implementation order (for the impl agent)

1. `starfish-dom`: types + arena API + `append_text` + serializer helper + arena
   tests.
2. `starfish-html`: `Token` + `Tokenizer` (states §2.2, refs §2.3) + tokenizer
   tests.
3. `starfish-html`: tree builder (modes §3) + `parse` + tree-shape tests.
4. Add the `starfish-dom` dependency edge to `crates/html/Cargo.toml`.
