# M2 — CSS → stylesheet model design note

Scope: a hand-rolled CSS tokenizer + parser (`starfish-css`) that turns a CSS
source string (a `<style>` block body or an external stylesheet) into an
in-memory **stylesheet model** — a list of rules, each rule being a list of
selectors plus a list of declarations. We target a pragmatic subset of the CSS
Syntax Level 3 + Selectors Level 3 specs — enough to later style a typical
static page — and explicitly defer the rest.

M2 only **parses** into a data model. No cascade, no specificity-based winner
selection, no computed values, no selector *matching* against a DOM. Those are
M3. The one forward-looking thing M2 computes is each selector's **specificity**
`(a, b, c)`, because it is a pure function of the selector text and M3 will need
it — computing it at parse time is cheap and keeps the selector self-describing.

Guiding rule (project "Simplicity First"): design only what M2 needs, plus the
specificity field M3 consumes. No speculative property typing, no abstraction
that has a single use. Every type below has a consumer in M2 or directly in M3.

---

## 1. Stylesheet data model

A stylesheet is a flat owned tree of plain structs — no arena. Unlike the DOM
(a cyclic graph traversed by index), a stylesheet is a simple acyclic
parent→child ownership chain (`Stylesheet` owns `Rule`s, a `Rule` owns its
`Selector`s and `Declaration`s). `Vec<T>` ownership is the natural fit; there are
no back-links and nothing keys into it by id. So we just nest `Vec`s.

```rust
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

pub struct Rule {
    pub selectors: Vec<Selector>,       // the comma-separated selector list
    pub declarations: Vec<Declaration>, // the { ... } block
}

pub struct Declaration {
    pub name: String,    // property name, lowercased, e.g. "color"
    pub value: Value,
    pub important: bool, // trailing `!important`
}
```

### 1.1 Value representation — decision

**Keep both: a raw trimmed string AND a small typed enum for the common cases.**
The `Value` is:

```rust
pub struct Value {
    /// The declaration value verbatim (trimmed, comments stripped, `!important`
    /// removed). Always present. This is the source of truth.
    pub raw: String,
    /// A best-effort typed interpretation of `raw` for the common shapes.
    /// `Component::Raw` when we don't specialize it. Never lossy: re-joining the
    /// components reproduces the value's meaning.
    pub components: Vec<Component>,
}

pub enum Component {
    /// Bare identifier / keyword: `block`, `auto`, `inherit`, `solid`.
    Keyword(String),
    /// <number> with no unit: `1.5`, `0`, `400`.
    Number(f32),
    /// <length>/<percentage> etc: value + unit. `12px` → (12.0, "px"),
    /// `50%` → (50.0, "%"), `1.5em` → (1.5, "em").
    Dimension { value: f32, unit: String },
    /// Color literal: `#rgb`/`#rrggbb`, `rgb()/rgba()`, or a named color we
    /// recognize. Stored pre-resolved to RGBA.
    Color(Rgba),
    /// A function we kept but did not specialize, e.g. `url(...)`,
    /// `calc(...)`, `linear-gradient(...)`. `name` lowercased, `raw_args`
    /// is the verbatim text between the parens.
    Function { name: String, raw_args: String },
    /// A `,` between top-level components (for comma lists like font stacks).
    Comma,
    /// A quoted string: `"Helvetica Neue"`.
    Str(String),
    /// Anything we did not classify — preserves the token text so M3 can decide.
    Raw(String),
}

#[derive(Clone, Copy)]
pub struct Rgba { pub r: u8, pub g: u8, pub b: u8, pub a: u8 }
```

**How much typing in M2 vs deferred to M3 — justification.**

- We do **not** map property names to typed property structs (no
  `Display::Block`, no `LengthOrPercentage`). That mapping is the *cascade /
  computed-value* job (M3), it is per-property (hundreds of properties), and
  doing it in M2 would duplicate the table M3 must own anyway. M2 stays
  property-agnostic: it tokenizes the value into generic CSS component values
  and hands them over.
- We **do** classify the four shapes that are trivially recognizable from the
  token stream alone and that *every* property consumer wants pre-chewed:
  identifiers, numbers, dimensions/percentages, and colors — plus structural
  separators (comma, string, function). This is cheap (it falls out of the
  tokenizer for free) and saves M3 from re-lexing every value. Crucially it is
  **never lossy**: `raw` is always kept, and unrecognized input becomes
  `Component::Raw(text)`, so M3 can always fall back to the string.
- Net: M2 answers "what tokens is this value made of," M3 answers "what does
  property `X` mean by these tokens." Clean seam, no duplicated property table.

The `raw` string exists so that (a) M3 can re-parse a value with
property-specific rules if the generic split was unhelpful, and (b) tests and
debugging have a stable, readable handle on a value.

Color parsing in M2 is limited to: `#rgb`, `#rrggbb` (and `#rgba`/`#rrggbbaa`
hex with alpha), `rgb(r,g,b)` / `rgba(r,g,b,a)` with integer or percent
channels, and a **small** named-color set (the ~16 CSS basic colors:
black/white/red/green/blue/gray/silver/maroon/yellow/olive/lime/aqua/teal/navy/fuchsia/purple).
The full X11 named-color table (~148 names) is a flat lookup that M3 can extend;
M2 keeps the short list. Unrecognized color-ish text stays `Keyword`/`Raw`, not
an error.

---

## 2. Selector model

### 2.1 Types

```rust
pub struct Selector {
    /// The compound selectors joined by combinators, in source order.
    /// `div .item` → [Compound(div), Descendant, Compound(.item)] is *not* how
    /// we store it; instead we interleave: see `parts`.
    pub parts: Vec<SelectorPart>,
    pub specificity: Specificity,
}

pub enum SelectorPart {
    Compound(Compound),
    Combinator(Combinator),
}

pub enum Combinator {
    Descendant, // whitespace, e.g. `div p`
    Child,      // `>` (stubbed — parsed & stored; M3 may treat like descendant)
}

/// A compound selector: one element of the chain, e.g. `div.item#main`.
/// All simple selectors that apply to the *same* element, with no combinator
/// between them.
pub struct Compound {
    /// Type selector: Some("div"), or None when only `*`/class/id/etc.
    pub tag: Option<String>,
    /// `*` present. (`*` and a tag are mutually exclusive in source; if a tag
    /// is given, universal is implied-false.)
    pub universal: bool,
    pub ids: Vec<String>,      // `#a#b` is legal-but-weird; we keep all
    pub classes: Vec<String>,  // `.x.y` → ["x", "y"]
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Specificity {
    pub a: u32, // #id count
    pub b: u32, // .class count (+ attr + pseudo-class — none in M2)
    pub c: u32, // type/tag count (+ pseudo-element — none in M2)
}
```

A `Selector` is a single complex selector (one entry of the comma list). The
`Rule.selectors: Vec<Selector>` holds the whole list: `a, b.c` → two
`Selector`s. Storing combinators interleaved in `parts` (rather than e.g. a
linked list of compounds + combinator-between) keeps both representable in one
flat `Vec` and trivial to iterate left-to-right; M3's matcher walks `parts`
right-to-left.

`universal` is kept distinct from "tag is None" only so a bare `*` is
representable as an explicit compound (it has specificity `(0,0,0)`), versus a
compound that only has a class (`tag: None, universal: false`).

### 2.2 Specificity — computed at parse time

`Specificity { a, b, c }` per CSS Selectors §16 / Level 3:

- `a` = number of ID selectors (`#id`).
- `b` = number of class selectors `.cls` (also attribute selectors and
  pseudo-classes — **none parsed in M2**, so effectively just classes).
- `c` = number of type selectors (tag names) and pseudo-elements
  (**no pseudo-elements in M2**, so just tags).

`*` (universal) contributes nothing. Combinators contribute nothing. The value
is the per-component sum over **all** compounds in the complex selector. We store
it as three separate `u32` fields (not a packed integer) so M3 can compare
lexicographically `(a, b, c)` via the derived `Ord` and there is no overflow /
base-ambiguity to reason about. Comparison/ordering is M3's concern; M2 only
fills the numbers in.

Example: `ul li.active#first` → one id, one class, two tags → `(1, 1, 2)`.

### 2.3 Subset & non-goals (selectors)

Supported in M2:

- Type / tag: `div`
- Universal: `*`
- Class: `.x`
- Id: `#y`
- Compound: `div.x#y` (any combination, same element)
- Descendant combinator: ` ` (whitespace)
- Selector lists: `a, b`
- `>` child combinator — **stubbed**: it is cheap to lex (`>` delim) so we parse
  and store it as `Combinator::Child` rather than dropping the rule. M2 does not
  *match*, so the only obligation is to not lose the rule. (M3 may initially
  treat Child like Descendant; noted, not required.)

Explicit **non-goals** (a rule using these is handled per §4 error recovery —
the rule is **dropped**, not crashed):

- Pseudo-classes (`:hover`, `:first-child`, `:not()`) and pseudo-elements
  (`::before`).
- Attribute selectors (`[type="text"]`).
- Adjacent/general sibling combinators (`+`, `~`).
- Namespaces (`svg|rect`), `:is()`/`:where()`, nesting (`&`).

When the selector parser hits one of these tokens mid-selector, it marks the
*current selector* invalid; per spec, **one invalid selector in a list
invalidates the whole rule** — so the rule is dropped (lenient, no panic). This
is intentionally conservative for M2; relaxing it (drop only the bad selector)
can come later if needed.

---

## 3. CSS tokenizer

A character-driven scanner over the input `&str`, producing CSS Syntax L3
tokens, pulled one at a time by the parser. Comments `/* … */` are skipped
during tokenization (never surfaced as tokens). The pragmatic token subset:

```rust
pub enum Token {
    Ident(String),                 // `div`, `color`, `block`  (case preserved here;
                                   //   parser lowercases names where appropriate)
    Function(String),              // `rgb(`  → Function("rgb")  (ident immediately
                                   //   followed by `(`)
    AtKeyword(String),             // `@media` → AtKeyword("media")
    Hash(String),                  // `#main`, `#fff`  (stores text after `#`)
    Str(String),                   // "..." or '...' (decoded, quotes stripped)
    Number(f32),                   // `12`, `1.5`, `-3`, `+0.5`
    Percentage(f32),               // `50%`  (stores 50.0)
    Dimension { value: f32, unit: String }, // `12px`, `1.5em`
    Delim(char),                   // a single non-token char: `.`, `*`, `>`, `+`,
                                   //   `~`, `!`, `=`, `/`, etc.
    Whitespace,                    // a run of whitespace, collapsed to one token
    Colon,                         // `:`
    Semicolon,                     // `;`
    Comma,                         // `,`
    LeftBrace,  RightBrace,        // `{`  `}`
    LeftParen,  RightParen,        // `(`  `)`
    LeftBracket, RightBracket,     // `[`  `]`  (lexed so attr selectors can be
                                   //   skipped cleanly; not otherwise used in M2)
    Eof,
}
```

Notes:

- **Ident-like sequences** start with a letter, `_`, `-`, or a non-ASCII byte;
  continue with letters/digits/`_`/`-`. A `-` followed by a digit at the start
  is a number, not an ident (sign handling below).
- **`Function`** is emitted when an ident is immediately followed by `(`. The
  `(` is consumed as part of producing `Function(name)` (so the parser sees
  `Function("rgb")` then the arguments then `RightParen`).
- **`Hash`** — `#` then ident-chars. We keep the raw text; the parser/value
  layer decides if it's an id selector (`#main`) or a hex color (`#fff`).
- **Numbers** — optional leading `+`/`-`, integer and/or fraction (`.5`, `1.`,
  `1.5`), optional `e±NN` exponent (lenient; accept if present). After the
  numeric part: `%` → `Percentage`; an ident-start char → `Dimension{value,
  unit}` (unit lowercased); otherwise → `Number`. Parsed into `f32` (sufficient
  for CSS lengths/colors; we are not doing sub-ULP-accurate math).
- **Strings** — `"`/`'` delimited; support the same minimal escapes the HTML
  side uses is *not* needed — CSS escapes are rare in static pages, so M2
  handles `\"`/`\'`/`\\` and otherwise copies bytes verbatim. An unterminated
  string at EOF closes at EOF (lenient, no panic).
- **Whitespace** — runs (space, tab, newline, form feed) collapse to a single
  `Whitespace` token. It is significant in selectors (descendant combinator) and
  insignificant elsewhere; the parser decides.
- **Comments** — `/* … */` consumed and discarded inside the tokenizer at any
  point a token could start. Unterminated comment at EOF → consume to EOF
  (lenient).
- **`Delim(char)`** is the catch-all for any single character that is not its
  own token: `.`, `*`, `>`, `+`, `~`, `!`, `=`, `/`, `|`, `@`-not-keyword, etc.
  The parser interprets delims by context (`.` before ident = class; `*` =
  universal; `!` before `important` = important flag).

Tokenizer struct sketch (mirrors the M1 HTML tokenizer shape):

```rust
pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize, // byte offset
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self;
    /// Pull the next token. Returns Token::Eof at end (idempotent).
    pub fn next_token(&mut self) -> Token;
}
```

**Out of M2 tokenizer:** `url(` unquoted-URL state (we lex `url` as a normal
`Function`, args read verbatim up to the matching `)` — fine for `url("x")` and
`url(x)` alike since the parser grabs raw args), CDO/CDC (`<!-- -->`) tokens,
unicode-range tokens, and full CSS escape (`\26` hex escapes) handling.

---

## 4. Parser

The parser consumes the token stream and builds a `Stylesheet`. It implements
the CSS Syntax L3 grammar for a **list of rules**, restricted to qualified rules
(selectors + `{ declarations }`) and gracefully-skipped at-rules. It is
**lenient and never panics** — malformed input recovers, mirroring CSS error
handling.

### 4.1 Top level — list of rules

Loop until `Eof`, skipping `Whitespace`:

- **`AtKeyword`** → parse-and-skip an at-rule (§4.4).
- **`RightBrace`** / stray closing tokens at top level → ignore (lenient).
- Otherwise → parse a **qualified rule** (§4.2). If it parses to a valid rule
  with ≥1 declaration *and* ≥1 valid selector, push it; if the prelude
  (selectors) is invalid, the whole rule is dropped but its `{…}` block is still
  consumed so parsing resyncs.

### 4.2 Qualified rule

```
qualified-rule = <prelude:tokens up to `{`> <block:`{` ... `}`>
```

1. Collect prelude tokens until the first top-level `{` (or `Eof` → discard,
   it's a malformed trailing rule).
2. Parse the prelude as a **selector list** (§4.3). If parsing fails (invalid
   selector token, empty), remember "prelude invalid."
3. Parse the brace block as a **declaration list** (§4.5).
4. If prelude valid and selector list non-empty → emit `Rule { selectors,
   declarations }`. Else drop (block already consumed in step 3 → resynced).

`{` and `}` nesting inside the prelude is balanced when scanning so a `{` inside
e.g. a function in the prelude (won't happen in our subset, but cheap) doesn't
mis-terminate.

### 4.3 Selector list parsing

Split the prelude token stream on top-level `Comma` into complex selectors. For
each complex selector:

- Skip leading/trailing `Whitespace`.
- Walk tokens, building `Compound`s separated by combinators:
  - `Ident` → set `compound.tag` (lowercased). (A second tag in one compound is
    invalid → mark selector invalid.)
  - `Delim('*')` → `compound.universal = true`.
  - `Delim('.')` then `Ident` → push class.
  - `Hash(text)` → push id (a `Hash` in selector position is always an id; hex
    colors only occur in *values*).
  - `Whitespace` between two compounds → push `Combinator::Descendant`, start a
    new compound. (Whitespace adjacent to an explicit `>` combinator is
    absorbed.)
  - `Delim('>')` → push `Combinator::Child`, start new compound.
  - **Anything else** (`Colon` → pseudo, `LeftBracket` → attribute,
    `Delim('+'|'~')` → sibling, `Function` → functional pseudo) → **mark the
    whole selector list invalid** (§2.3 rationale) and stop.
- After building, compute `Specificity` by summing ids/classes/tags across all
  compounds.

An empty compound with a dangling combinator (e.g. `div >` `{}`) → invalid.

### 4.4 At-rules — handling

M2 does **not** implement any at-rule semantics. On an `AtKeyword`:

- Consume tokens until either a top-level `;` (statement at-rule, e.g.
  `@import …;` `@charset …;`) → discard, or a top-level `{ … }` block (block
  at-rule, e.g. `@media`, `@font-face`, `@keyframes`) → consume the balanced
  block and discard **everything**, including any nested rules.

So `@media screen { p { color: red } }` contributes **zero** rules to the
stylesheet in M2. This is a documented limitation: media-query'd and
font-face'd rules are silently dropped. (Lifting this later means, for
`@media`, recursing into the block's rule list under a guard — out of scope
now.) The key property is that at-rules are *skipped cleanly* so the surrounding
top-level rules still parse.

### 4.5 Declaration block & declarations

Inside `{ … }`, parse a `;`-separated list of declarations:

```
declaration = <ident:name> `:` <value-tokens> [ `!` `important` ]
```

For each declaration (tokens up to the next top-level `;` or the closing `}`):

1. Skip `Whitespace`. Expect `Ident` → property name (lowercased). If the first
   non-whitespace token isn't an ident → **bad declaration**, skip to next `;`
   (or `}`).
2. Skip `Whitespace`, expect `Colon`. Missing → bad declaration, skip to `;`.
3. Collect the **value tokens** up to the top-level `;`/`}`. Balanced parens
   keep `;` inside a function from terminating (defensive).
4. Detect a trailing `! important` (delim `!` + ident `important`,
   case-insensitive, whitespace-tolerant) at the end of the value tokens; set
   `important = true` and strip it from the value.
5. Build the `Value`: `raw` = the value token span re-serialized & trimmed;
   `components` = classify each value token (§1.1). An **empty** value → bad
   declaration, skip.
6. Push the `Declaration`.

**Error recovery rules (the whole point of "lenient"):**

- **Bad declaration** (no ident, no colon, empty value, junk before colon) →
  discard that declaration only, skip to the next top-level `;`, continue with
  the next declaration. The rest of the block survives.
- **Bad rule** (invalid prelude) → drop the rule but still consume its `{…}` so
  the stream resyncs at the following rule.
- **Unbalanced braces / EOF mid-block** → close at EOF, emit what was parsed so
  far (lenient).
- Never panic, never return `Err` to the caller (entry point is infallible).

### 4.6 Value re-serialization for `raw`

`raw` is produced by concatenating the value tokens' source text with
whitespace tokens collapsed to single spaces and trimmed. Simplest correct
approach: have the tokenizer (or parser) track the byte span `[start, end)` of
the value run in the original input and slice it, then trim and collapse
internal whitespace. Slicing the original `&str` is cheaper and more faithful
than re-printing tokens, so **prefer span-slicing** for `raw`; `components` are
still built from the tokens. (Implementation note: this means the tokenizer
should expose, or the parser should track, byte offsets — a small addition to
the struct sketch in §3.)

---

## 5. Crate boundary & entry point

`starfish-css` has **no dependency on `starfish-dom`** in M2. Selectors are pure
data (strings + counts); nothing here matches against a DOM node. The DOM↔
selector coupling (matching, cascade) is entirely M3's `starfish-style` crate,
which will depend on *both* `starfish-dom` and `starfish-css`. Keeping M2
DOM-free keeps the dependency graph a DAG and lets the CSS parser be tested in
complete isolation.

`crates/css/Cargo.toml` stays dependency-free for M2 (no `starfish-dom`,
no third-party crates — the tokenizer and `f32` parsing are hand-rolled).

### Public entry point

```rust
// starfish-css
pub fn parse_stylesheet(css: &str) -> Stylesheet;
```

One call, no config, no `Result` — infallible/lenient, matching CSS's
error-recovery model. The returned `Stylesheet` contains exactly the
well-formed rules (with at-rules dropped and bad rules/declarations skipped).

Re-exports / module layout for ergonomic callers and isolated tests:

```rust
pub use model::{Stylesheet, Rule, Declaration, Value, Component, Rgba};
pub use selector::{Selector, SelectorPart, Compound, Combinator, Specificity};
pub mod tokenizer { pub use ... Tokenizer, Token; } // exposed for tokenizer tests
```

---

## 6. Edge cases & explicit non-goals

Handled (lenient recovery, no panic):

- Empty stylesheet / only comments / only whitespace → `Stylesheet { rules: [] }`.
- Rule with no declarations `p {}` → emitted with empty `declarations` (or
  dropped — **decision: emit it**, since the selectors are valid; M3 ignores
  empty rules harmlessly). *(If the impl prefers dropping empties, document it;
  emitting is the simpler invariant.)*
- Trailing `;` / missing final `;` before `}` (`p{color:red}`) → fine.
- Bad declaration in the middle of a good block → only that declaration dropped.
- Unknown property names → kept (M2 doesn't validate property names; M3 decides).
- Unknown / vendor-prefixed values (`-webkit-…`) → kept as `Keyword`/`Raw`.
- `!important` with odd spacing (`color:red ! important`) → detected.
- Comments anywhere, including inside a value (`color:/* c */red`) → stripped.
- Multiple classes/ids in a compound (`.a.b`, `#x#y`) → all kept.
- Unterminated string / comment / block at EOF → closed at EOF.
- At-rules (`@media`, `@import`, `@font-face`, …) → cleanly skipped, surrounding
  rules unaffected.

Explicit **non-goals** for M2 (documented limitations):

- No cascade, no specificity *comparison*/winner selection, no inheritance, no
  computed values, no shorthand expansion (`margin: …` is not split). (M3.)
- No selector **matching** against a DOM. (M3.)
- No property-name → typed-property mapping; values stay generic component
  lists. (M3.)
- No pseudo-classes/elements, attribute selectors, sibling combinators,
  namespaces, `:is()/:where()`, CSS nesting (rules using them are dropped).
- No `@media`/`@supports`/`@font-face`/`@keyframes` *semantics* (skipped).
- No `calc()` evaluation, no `var()`/custom-property resolution (kept as
  `Function`/`Raw`).
- No `url()` fetching/resolution; `url(...)` kept verbatim. (Networking is
  out of the whole epic.)
- No full named-color table (16 basic colors only) and no `hsl()`/modern color
  syntax in M2.
- No full CSS escape (`\xxxxxx`) or unicode-range handling.
- No source-location/line tracking on rules (a byte span may be tracked
  internally for `raw` slicing but is not exposed).

---

## 7. Test plan

Unit tests live in `crates/css/src/...` next to each layer: tokenizer tests,
selector-parse tests, and end-to-end `parse_stylesheet` tests. A small helper to
build readable assertions over selectors/declarations is fine (e.g. format a
`Selector` back to a canonical string).

### 7.1 Tokenizer tests (snippet → token sequence)

1. `div` → `[Ident("div"), Eof]`.
2. `.foo` → `[Delim('.'), Ident("foo"), Eof]`.
3. `#bar` → `[Hash("bar"), Eof]`.
4. `12px` → `[Dimension{12.0,"px"}, Eof]`; `50%` → `[Percentage(50.0)]`;
   `1.5` → `[Number(1.5)]`; `-3` → `[Number(-3.0)]`.
5. `rgb(0,0,0)` → `[Function("rgb"), Number(0), Comma, Number(0), Comma,
   Number(0), RightParen, Eof]`.
6. `"Helvetica Neue"` → `[Str("Helvetica Neue"), Eof]`.
7. `a /* c */ b` → `[Ident("a"), Whitespace, Ident("b"), Eof]` (comment dropped,
   surrounding whitespace preserved as needed).
8. `> + ~ * !` → all `Delim` of the respective chars (plus whitespace).
9. `{ } ( ) ; : ,` → the matching structural tokens.
10. Unterminated string `"abc` at EOF → `[Str("abc"), Eof]` (no panic).
11. Unterminated comment `/* abc` at EOF → `[Eof]` (no panic).

### 7.2 Selector-parse tests (selector text → parts + specificity)

1. `*` → one compound `universal`, specificity `(0,0,0)`.
2. `div` → tag `div`, `(0,0,1)`.
3. `.item` → class `item`, `(0,1,0)`.
4. `#main` → id `main`, `(1,0,0)`.
5. `div.item#main` → one compound (tag+class+id), `(1,1,1)`.
6. `ul li` → `[Compound(ul), Descendant, Compound(li)]`, `(0,0,2)`.
7. `ul li.active#first` → `(1,1,2)`.
8. `a, b.c` (as a rule prelude) → two selectors, specificities `(0,0,1)` and
   `(0,1,1)`.
9. `div > p` → `[Compound(div), Child, Compound(p)]`, `(0,0,2)` (child stub
   stored).
10. **Invalid** `a:hover` → selector list invalid → the rule is dropped (assert
    `parse_stylesheet("a:hover{color:red}")` yields zero rules).
11. **Invalid** `input[type=text]` → rule dropped.
12. `.a.b` → two classes, `(0,2,0)`.

### 7.3 `parse_stylesheet` tests (CSS → rules/declarations)

1. `p { color: red; }` → 1 rule, selector `p`, 1 declaration
   `{name:"color", value.raw:"red", important:false}`, and
   `value.components == [Color(rgba red)]` (named-color recognized).
2. `p { color: red }` (no trailing `;`) → same as above.
3. `p {}` → 1 rule, empty declarations (per §6 decision).
4. `h1, h2 { margin: 0; }` → 1 rule, two selectors, one declaration; value
   `components == [Number(0.0)]`.
5. `.box { width: 50%; padding: 10px 20px; }` → 2 declarations; `width` →
   `[Dimension/Percentage(50%)]`; `padding` → `[Dimension(10px), Whitespace?,
   Dimension(20px)]` (raw `"10px 20px"`).
6. `a { color: #ff0000 } ` → component `Color(255,0,0,255)`; and `#f00` form
   equals the same color.
7. `b { color: rgb(0, 128, 255); }` → `Color(0,128,255,255)`.
8. `p { color: red !important; }` → `important == true`, `raw == "red"`.
9. **Bad declaration recovery:** `p { color red; font-size: 12px; }` (missing
   colon on first) → 1 rule with **one** declaration `font-size:12px`
   (the bad one skipped).
10. **At-rule skip:** `@media screen { p { color: red } } div { color: blue }`
    → exactly **one** rule (`div`), the `@media` block fully dropped.
11. **At-rule statement skip:** `@import "x.css"; p { color: red }` → one rule
    (`p`).
12. **Comment stripping in value:** `p { color: /* x */ blue; }` →
    `raw == "blue"`.
13. **Empty / whitespace / comment-only** input → `rules.is_empty()`.
14. **Multiple rules + nested compound + descendant:**
    `nav ul li a { text-decoration: none } .btn { display: block }` → 2 rules;
    first selector `(0,0,4)`, second `(0,1,0)`.
15. A representative small stylesheet fixture (a handful of rules covering
    type/class/id/descendant selectors and color/length/keyword values) is
    parsed and the full rule list asserted — the milestone's "done-when" smoke
    test.

---

## 8. Implementation order (for the impl agent)

1. `model` + `selector` + `tokenizer` type definitions (no logic) — make the
   data model concrete first.
2. `Tokenizer` (§3) + tokenizer tests (§7.1). Track byte offsets for value-span
   slicing (§4.6).
3. Selector parser (§4.3) + `Specificity` computation + selector tests (§7.2).
4. Declaration/value parser (§4.5–4.6) incl. `Component` classification + color
   parsing.
5. Top-level rule loop + at-rule skipping + error recovery (§4.1, §4.4) +
   `parse_stylesheet` + end-to-end tests (§7.3).
