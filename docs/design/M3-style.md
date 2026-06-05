# M3 — Style resolution design note

Scope: the `starfish-style` crate. Given a `Document` (from `starfish-dom`) and
a set of parsed `Stylesheet`s (from `starfish-css`), produce a **styled tree** —
for each DOM element, a typed `ComputedStyle` holding the resolved value of a
small, **layout-sufficient** subset of CSS properties. This is the input M4
(layout) reads.

M3 implements: selector **matching** against the DOM, the **cascade**
(origin/importance/specificity/source-order), **inheritance**, and computing a
**typed property set** from the generic `Component` values M2 produced. A
built-in **User-Agent default stylesheet** supplies default `display` (and a few
margins) for HTML elements.

Guiding rule (project "Simplicity First"): design only the property subset M4
needs to do block + inline flow layout with text and box/background painting.
No speculative properties, no per-property abstraction with a single use. Every
field below has a consumer in M4/M5.

What M3 is **not**: no `var()`, no `calc()`, no `@media` (M2 already dropped
those), no pseudo-classes/elements/attribute selectors (M2 dropped rules using
them), no flexbox/grid/float/position. See §8.

---

## 0. Inputs M3 actually receives (recap of the real M1/M2 API)

These are the exact types M3 consumes — the design is pinned to them, not to an
idealized API.

From `starfish-dom` (`crates/dom/src/lib.rs`):

- `NodeId` — `Copy + Eq + Hash` index; **`Document` exposes `parent(id)`** (line
  149), so selector matching can walk ancestors with no extra bookkeeping. No
  gap here — parent links are first-class.
- `Document::kind(id) -> &NodeKind`, with `NodeKind::{Document, Doctype,
  Element, Text, Comment}`.
- `Document::tag_name(id) -> Option<&str>` (lowercased), `get_attribute(id,
  name) -> Option<&str>` (name already lowercased; first occurrence wins),
  `first_child` / `next_sibling` / `children` / `root`.
- The `class` attribute is a single string; classes are **space-separated** and
  must be split by M3 (`get_attribute(id, "class")` → split on ASCII whitespace).

From `starfish-css` (`crates/css/src/{model,selector}.rs`):

- `Stylesheet { rules: Vec<Rule> }`, `Rule { selectors: Vec<Selector>,
  declarations: Vec<Declaration> }`.
- `Declaration { name: String /*lowercased*/, value: Value, important: bool }`.
- `Value { raw: String, components: Vec<Component> }`.
- `Component::{ Keyword(String), Number(f32), Dimension{value,unit},
  Color(Rgba), Function{name,raw_args}, Comma, Str(String), Raw(String) }`.
- `Selector { parts: Vec<SelectorPart>, specificity: Specificity }`,
  `SelectorPart::{Compound(Compound), Combinator(Combinator)}`,
  `Compound { tag: Option<String>, universal: bool, ids: Vec<String>, classes:
  Vec<String> }`, `Combinator::{Descendant, Child}`,
  `Specificity { a, b, c }` (derives `Ord`).

Important consequence: **colors and dimensions are already pre-parsed by M2.**
`Component::Color(Rgba)` arrives resolved (hex / `rgb()` / the ~16 named colors);
`Component::Dimension{value, unit}` arrives as a number + unit string (`"px"`,
`"%"`, `"em"`). M3 does **not** re-lex values — it pattern-matches `components`.
The color resolver in M2 (`crates/css/src/color.rs`) is `pub(crate)`, so M3
relies on `Component::Color` rather than calling it.

---

## 1. `ComputedStyle` — the typed property set for one element

A small struct of resolved, typed values. One per styled element (text nodes do
not get their own — see §2). All fields are populated for every element (no
`Option` per property): after the cascade + inheritance + initial-value fallback,
every property has a definite computed value.

### 1.1 Value types

```rust
/// A resolved RGBA color. Re-exported from starfish-css so M3, M4, M5 agree.
pub use starfish_css::Rgba;

/// Computed length for box-model sizing/spacing.
/// `em`/`rem` are resolved to `Px` at compute time (§5.3); only these three
/// variants survive into the computed value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32), // 50% → Percent(50.0); resolved against a containing block in M4
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    None,   // also covers `hidden` for M3
    Solid,  // the only line style M5 paints; dashed/dotted/etc. fold to Solid
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify, // accepted; M4 may treat as Left initially (noted)
}

/// `font-weight`, normalized to a numeric weight. `normal`→400, `bold`→700.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontWeight(pub u16);

/// `line-height`. `normal` and unitless multipliers stay relative to font-size;
/// a length is absolute. Resolved to px against the element's own font-size in M4
/// (kept abstract here so M4 owns the final number).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,        // ~1.2 × font-size; M4 picks the factor
    Number(f32),   // unitless multiplier of font-size
    Px(f32),       // absolute length (em/rem already folded to px)
}
```

`Length::Px` is the *resolved absolute* form. `Percent` and `Auto` cannot be
resolved without layout context (the containing block), so they survive into the
computed value and are resolved by M4. This matches CSS: percentages on width/
margin are computed values that depend on layout.

### 1.2 The struct

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // box generation
    pub display: Display,

    // box dimensions
    pub width: Length,
    pub height: Length,

    // margin (TRBL)
    pub margin_top: Length,
    pub margin_right: Length,
    pub margin_bottom: Length,
    pub margin_left: Length,

    // padding (TRBL) — `auto` is invalid on padding; parsed as 0 if seen
    pub padding_top: Length,
    pub padding_right: Length,
    pub padding_bottom: Length,
    pub padding_left: Length,

    // border (TRBL widths + one shared style + one shared color for M3)
    pub border_top_width: f32,
    pub border_right_width: f32,
    pub border_bottom_width: f32,
    pub border_left_width: f32,
    pub border_style: BorderStyle, // single style for all four sides (M3 simplification)
    pub border_color: Rgba,        // single color for all four sides (M3 simplification)

    // color / background
    pub color: Rgba,            // inherited
    pub background_color: Rgba, // not inherited; initial = transparent

    // text / font
    pub font_size: f32,         // px, inherited (the anchor for em/rem/% resolution)
    pub font_weight: FontWeight,// inherited
    pub line_height: LineHeight,// inherited
    pub text_align: TextAlign,  // inherited
    pub font_family: Vec<String>, // inherited; ordered fallback list, may be empty
}
```

**Border simplification (M3):** real CSS has per-side `border-*-style` and
`border-*-color`. For M3 we keep one `border_style` and one `border_color`
shared by all sides, because the only thing M5 needs is "draw a solid box border
of this color/width." Per-side widths are kept (the box model needs them for
content-box math) but per-side style/color is deferred. The `border` shorthand
and `border-<side>-width` longhands feed these. This is documented as a known
limitation, not a silent loss.

### 1.3 Initial values and inheritance

`ComputedStyle::initial()` returns the **root's starting style** (used as the
"parent" for the document root, and as the per-property fallback when nothing
cascades). Per CSS initial values:

| Property            | Initial value                 | Inherited? |
|---------------------|-------------------------------|------------|
| `display`           | `Inline` (UA sheet overrides) | no         |
| `width` / `height`  | `Auto`                        | no         |
| `margin-*`          | `Px(0.0)`                     | no         |
| `padding-*`         | `Px(0.0)`                     | no         |
| `border-*-width`    | `0.0`                         | no         |
| `border_style`      | `None`                        | no         |
| `border_color`      | = `currentColor` → use `color`| no (uses color) |
| `color`             | `Rgba{0,0,0,255}` (black)     | **yes**    |
| `background_color`  | transparent `Rgba{0,0,0,0}`   | no         |
| `font_size`         | `16.0` px                     | **yes**    |
| `font_weight`       | `FontWeight(400)`             | **yes**    |
| `line_height`       | `LineHeight::Normal`          | **yes**    |
| `text_align`        | `TextAlign::Left`             | **yes**    |
| `font_family`       | `[]` (empty → M5 default font)| **yes**    |

Inherited set (exactly): **`color`, `font_size`, `font_weight`, `line_height`,
`text_align`, `font_family`**. Everything else resets to its initial value on
each element. `border_color`'s initial is `currentColor`: M3 resolves a missing
border color to the element's computed `color`.

```rust
impl ComputedStyle {
    /// The all-initial style. Doubles as the synthetic parent of the root.
    pub fn initial() -> ComputedStyle;

    /// Produce a fresh child style: inherited properties copied from `self`,
    /// everything else reset to initial. The cascade then overwrites onto this.
    fn inherit_from(&self) -> ComputedStyle;
}
```

---

## 2. Styled-tree representation

Computed styles attach to the DOM via a **side table**, not by mutating
`Document`. This keeps M1's DOM immutable/clean and lets M3 own its output.

```rust
use std::collections::HashMap;

pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
}

impl StyledTree {
    /// Computed style for a node. Panics if `id` was not styled (a bug — every
    /// element and the styled text-bearing nodes are inserted during the walk).
    pub fn computed(&self, id: NodeId) -> &ComputedStyle;

    /// Non-panicking lookup (None for Document/Doctype/Comment nodes).
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle>;
}
```

`HashMap<NodeId, ComputedStyle>` over `Vec`-indexed-by-NodeId: `NodeId`'s inner
`u32` is private (no public `.index()`), so a `Vec` keyed by it isn't reachable
without adding DOM API; `NodeId` is `Hash`, so a map is the zero-DOM-change
choice. Element counts are small; the map is fine.

**Which nodes get a `ComputedStyle`:**

- **Elements** — yes, one each (the cascade result).
- **Text nodes** — text has no CSS box of its own; it inherits everything from
  its parent element. M4 reads text styling from the **parent** element's
  `ComputedStyle`. So M3 does **not** insert a separate entry for text nodes;
  `get(text_id)` returns `None` and M4 uses `parent`. (Simpler than cloning the
  parent style into every text node. Documented so M4 knows the contract.)
- **Document / Doctype / Comment** — no style; `get` → `None`.

### Public entry point

```rust
/// Build the styled tree: walk the DOM from the root, cascade each element
/// against the UA sheet + the given author stylesheets, applying inheritance.
/// Infallible.
pub fn style_tree(doc: &Document, author_sheets: &[Stylesheet]) -> StyledTree;
```

The walk is a pre-order DFS from `doc.root()`. The synthetic parent style for
the root element is `ComputedStyle::initial()`. For each element node:

1. `base = parent_style.inherit_from()` (inherited props seeded, rest initial).
2. Cascade matching declarations onto `base` (§4) → this element's
   `ComputedStyle`.
3. Insert into `styles`, then recurse into element children passing this style
   as their `parent_style`.

Non-element children (text/comment) are skipped for insertion but text styling
is available via the parent.

The UA sheet (§6) is parsed once at the top of `style_tree` and prepended to the
cascade with `Origin::UserAgent`.

---

## 3. Selector matching

`fn matches(doc: &Document, element: NodeId, selector: &Selector) -> bool`.
Caller guarantees `element` is an `Element` node.

### 3.1 Compound match

`fn compound_matches(doc, element, c: &Compound) -> bool` — all conditions must
hold (AND):

- **tag**: if `c.tag = Some(t)`, require `doc.tag_name(element) == Some(t)`
  (both already lowercased). `c.universal` and a `None` tag impose no tag
  constraint.
- **ids**: for each id in `c.ids`, require it equals `get_attribute(element,
  "id")`. (Multiple ids in one compound — rare — all must match the single id
  attribute, so >1 distinct id never matches; that's correct.)
- **classes**: split `get_attribute(element, "class")` on ASCII whitespace into
  a set; require every class in `c.classes` is present.

Empty `class`/missing `id` → those constraints fail unless the compound asks for
none.

### 3.2 Combinator traversal (right-to-left)

`Selector.parts` is `[Compound, Combinator, Compound, …, Compound]` in source
(left-to-right) order. We match **right-to-left**, the standard efficient
direction:

1. The **rightmost** compound (the subject) must match `element`. If not →
   `false`.
2. Walk leftward over `(Combinator, Compound)` pairs. Keep a `current` node
   (initially `element`). For each combinator + the compound to its left:
   - **Descendant**: search **ancestors** of `current` (via `doc.parent`) for
     one whose element matches the left compound. If found, set `current` to it
     and continue; if the ancestor chain is exhausted → `false`.
   - **Child**: require `doc.parent(current)` to be an element matching the left
     compound. If yes, `current = parent`; else → `false`.
3. All compounds consumed with no failure → `true`.

Descendant is backtracking-free here because our selector subset has no branching
(no `:not`, no compound alternation). A simple greedy "first matching ancestor"
is **not** always complete for descendant combinators in general (e.g.
`a b c` where the nearest `b`-ancestor has no `a`-ancestor but a farther one
does). To stay correct, the descendant step uses **backtracking**: implement
right-to-left matching recursively so that on a later failure it tries the next
qualifying ancestor. Concretely:

```rust
// match parts[..=i] (i = index of a Compound) ending at `node`
fn match_from(doc, node, parts, i) -> bool {
    if !compound_matches(doc, node, parts[i].compound()) { return false; }
    if i == 0 { return true; }
    let comb = parts[i-1].combinator(); // i-1 is always a Combinator
    let left = i - 2;                    // index of the next Compound
    match comb {
        Child => doc.parent(node)
            .filter(|p| doc.tag_name(*p).is_some())
            .map_or(false, |p| match_from(doc, p, parts, left)),
        Descendant => {
            let mut anc = doc.parent(node);
            while let Some(a) = anc {
                if doc.tag_name(a).is_some() && match_from(doc, a, parts, left) {
                    return true;
                }
                anc = doc.parent(a);
            }
            false
        }
    }
}
```

Entry: `match_from(doc, element, &selector.parts, last_compound_index)`. This is
fully correct for the M2 selector subset and only as deep as the selector is
long. (Combinator::Child is now actually honored — M2 left it as "may treat like
descendant"; M3 implements it properly since it's trivial here.)

A `Selector` whose `parts` is empty never matches (won't occur — M2 drops empty
selectors).

---

## 4. The cascade

For one element, collect every declaration that applies, order them by CSS
cascade precedence, and apply lowest-precedence first so the winner overwrites.

### 4.1 Origin

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin { UserAgent, Author } // no User/Animation/Transition origins in M3
```

### 4.2 Collect matched declarations

For the element, iterate UA sheet then each author sheet **in order**, keeping a
running `source_order: usize` that increments per declaration across all sheets
(so author sheets, coming after UA, naturally get higher order). For each rule
whose **any** selector matches `element` (use the **max specificity among that
rule's matching selectors**), push one entry per declaration in the rule:

```rust
struct MatchedDecl<'a> {
    origin: Origin,
    specificity: Specificity,
    source_order: usize,
    declaration: &'a Declaration, // gives name, value, important
}
```

### 4.3 Sort key (ascending = applied first → last wins)

Cascade order, lowest precedence first:

1. **Origin × importance.** Precedence (low→high):
   `UA-normal < Author-normal < Author-important < UA-important`.
   (UA-`!important` beats author-`!important` per spec; in practice the UA sheet
   uses no `!important`, but the ordering is encoded for correctness.)
2. **Specificity** `(a, b, c)` ascending (uses `Specificity`'s derived `Ord`).
3. **Source order** ascending (later wins ties).

Implement as: map each `MatchedDecl` to a sort tuple
`(origin_importance_rank, specificity, source_order)` and `sort_by_key`. Then
apply each declaration in sorted order onto the element's `ComputedStyle`
(seeded by §2 step 1). Last write wins — exactly the cascade.

### 4.4 Inheritance + initial fallback

Because the base style was produced by `inherit_from(parent)` (inherited props
already carry the parent's computed value, non-inherited props already hold their
initial value), the cascade simply overwrites whatever matched. Any property no
declaration touched keeps the correct inherited-or-initial value. No separate
"resolve inherit/initial" pass is needed for the M3 subset (we don't support the
explicit `inherit`/`initial`/`unset` keywords — see §8; if a value's component is
the keyword `inherit`, the property handler leaves the base value untouched,
which for inherited props already equals the parent — a cheap correct
approximation, noted).

### 4.5 Inline `style=""` attribute

**Optional, recommended deferred.** An inline `style="…"` attribute is, in the
cascade, an author declaration with specificity above any selector. Supporting it
needs a small parse of the declaration list (M2's declaration parser isn't
exposed for a bare block, but a `style="a:b;c:d"` body can be wrapped as
`* { a:b; c:d }` and run through `parse_stylesheet`, or M2 can expose a
`parse_declaration_block`). For M3 scope: **out**, listed as the first easy
follow-up. If included later: collect the element's inline decls last with a
synthetic specificity of `(1,0,0,0)`-style rank above selectors.

---

## 5. Property parsing (Declaration → typed fields)

A per-property dispatcher maps `declaration.name` to a handler that reads
`declaration.value.components` (falling back to `value.raw` when needed) and
writes the typed field(s). Unknown property names are ignored (no error).

```rust
fn apply_declaration(style: &mut ComputedStyle, decl: &Declaration, parent_font_size: f32);
```

`parent_font_size` is needed because `em` resolves against the **parent's**
font-size for most properties but against the **element's own** font-size once
set; for the M3 subset we resolve `em` on `font-size` against the parent, and
`em` on lengths against the element's already-computed font-size (font-size is
applied first if present — see §5.3 ordering note).

### 5.1 Value helpers

- `as_length(components) -> Option<Length>`:
  - `[Dimension{value, unit:"px"}]` → `Px(value)`.
  - `[Dimension{value, unit:"%"}]` → `Percent(value)`.
  - `[Dimension{value, unit:"em"}]` → `Px(value * em_basis)` (§5.3).
  - `[Dimension{value, unit:"rem"}]` → `Px(value * root_font_size)` (§5.3).
  - `[Number(0.0)]` → `Px(0.0)` (unitless zero is a valid length).
  - `[Keyword("auto")]` → `Auto`.
  - else → `None` (ignored).
- `as_color(components) -> Option<Rgba>`: `[Color(rgba)]` → that; also accept
  `[Keyword("transparent")]` → `Rgba{0,0,0,0}`. (Named/hex already arrive as
  `Color` from M2.)
- `as_px(components) -> Option<f32>`: like `as_length` but only `Px`-resolvable
  forms (for border widths / font-size).

### 5.2 Per-property handlers (the subset)

| Property name(s)              | Handler                                                                 |
|-------------------------------|-------------------------------------------------------------------------|
| `display`                     | keyword → `block`/`inline`/`inline-block`/`none`; else ignore           |
| `width`, `height`             | `as_length` → field                                                     |
| `margin-top/right/bottom/left`| `as_length` → field                                                     |
| `margin`                      | **shorthand**, 1–4 lengths (§5.4) → four margin fields                  |
| `padding-*` / `padding`       | same as margin; `auto`→treat as `Px(0)` (padding can't be auto)         |
| `border-top/right/bottom/left-width` | `as_px` (also keywords thin=1/medium=3/thick=5) → side width     |
| `border-width`                | shorthand 1–4 px → four widths                                          |
| `border-style`                | keyword: `solid`→Solid, `none`/`hidden`→None, anything-else→Solid       |
| `border-color`                | `as_color` → `border_color`                                             |
| `border`                      | **shorthand** (§5.5): any of width/style/color tokens, sets all 4 widths + style + color |
| `color`                       | `as_color` → `color`                                                    |
| `background-color`, `background` | `as_color` → `background_color` (for `background`, take the first color component; ignore images/positions) |
| `font-size`                   | `as_px` (px/em/rem/number-as-px); keywords deferred → ignore            |
| `font-weight`                 | `bold`→700, `normal`→400, `Number(n)`→`n as u16`; `bolder`/`lighter`→700/400 approx |
| `line-height`                 | `Number(n)`→`Number(n)`; `Dimension px/em/rem`→`Px`; `normal`→`Normal`; `%`→`Number(pct/100)` |
| `text-align`                  | keyword → `left`/`right`/`center`/`justify`                             |
| `font-family`                 | collect `Str`/`Keyword` components split on `Comma` → `Vec<String>` (join multi-keyword family names with spaces) |

Anything not in this table is ignored. `text-decoration`, `vertical-align`,
`position`, `float`, etc. are **not** in M3.

### 5.3 `em`/`rem` resolution — decision

- `rem` resolves against the **root element's** computed `font-size`. M3 tracks
  the root font-size (the `<html>`/root computed `font_size`, default 16px).
- `em` on **`font-size` itself** resolves against the **parent's** computed
  `font-size` (`em_basis = parent_font_size`). This is in scope and cheap.
- `em` on **other lengths** resolves against the **element's own** computed
  `font-size`. To make this deterministic, the dispatcher **applies `font-size`
  first** (scan declarations for `font-size` before others in the same cascade
  pass, or two-pass: font-size, then the rest). Documented as in-scope.

Percent on `font-size` → relative to parent font-size; computed to px.

### 5.4 `margin`/`padding` shorthand (1–4 values)

Split `components` on whitespace-separated lengths (M2 already left them as
adjacent `Dimension`/`Number`/`Keyword` components, no `Comma`):

- 1 value `a` → all four = `a`.
- 2 values `v h` → top=bottom=`v`, right=left=`h`.
- 3 values `t h b` → top=`t`, right=left=`h`, bottom=`b`.
- 4 values `t r b l` → top, right, bottom, left in order.

Each token goes through `as_length` (single-component). A token that doesn't
parse → the whole shorthand is ignored (lenient: leave base values).

### 5.5 `border` shorthand (minimal)

`border: <width> || <style> || <color>` in any order, applied to all four sides:

- A `Dimension`/`Number` or width-keyword → all four `border_*_width`.
- A style keyword (`solid`/`none`/`hidden`/…) → `border_style`.
- A `Color` (or `transparent`) → `border_color`.

Missing pieces keep their current (base/initial) value. Per-side `border-<side>`
shorthands (e.g. `border-top: …`) are **deferred** (only `border` and
`border-<side>-width` longhands in M3).

### 5.6 `font-size` keywords

`small`/`medium`/`large`/`x-large`… absolute-size keywords are **out** for M3
(ignored). M3 supports numeric `font-size` only (px/em/rem/%/unitless-as-px).
Noted as a limitation.

---

## 6. User-Agent default stylesheet

Provides the structural defaults the cascade needs (chiefly `display`, plus a
few default margins so block layout looks right). **Decision: store it as a CSS
string and parse it with `starfish_css::parse_stylesheet` at the start of
`style_tree`.** This reuses all of M2, keeps the defaults human-readable/auditable
in one literal, and avoids a second hand-built `Stylesheet` construction path.
Cost (parsing a ~30-line string per `style_tree` call) is negligible.

```rust
const UA_CSS: &str = r#"
html, body, div, p, section, article, header, footer, nav, main, aside,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dd, blockquote, pre, table,
figure, figcaption, address, hr, form { display: block }

span, a, b, i, em, strong, small, code, label, abbr, cite, q, sub, sup,
u, s, mark, br { display: inline }

img, button, input, select, textarea { display: inline-block }

head, title, meta, link, style, script, base { display: none }

body   { margin: 8px }
p      { margin: 16px 0 }
h1     { margin: 21px 0; font-size: 32px; font-weight: bold }
h2     { margin: 19px 0; font-size: 24px; font-weight: bold }
h3     { margin: 18px 0; font-size: 18px; font-weight: bold }
h4     { margin: 21px 0; font-weight: bold }
h5     { margin: 22px 0; font-size: 13px; font-weight: bold }
h6     { margin: 24px 0; font-size: 11px; font-weight: bold }
ul, ol { margin: 16px 0; padding-left: 40px }
b, strong { font-weight: bold }
"#;
```

Notes:

- This is a pragmatic subset of the WHATWG UA sheet — enough for the M4 page,
  not a faithful copy. `padding-left` on lists assumes the `padding-<side>`
  longhand handler (§5.2); if not implementing per-side padding longhands in M3,
  drop it (lists just won't indent). **Decision: include `padding-left` and the
  `padding-<side>` longhand** since it's the same code path as `margin-<side>`.
- `display:none` on `head` subtree means M4 never lays out head metadata.
- `font-weight: bold` here resolves to `FontWeight(700)`.
- All UA rules carry `Origin::UserAgent` and no `!important`, so any author rule
  (even a tag selector of equal specificity) wins by source order — correct.

---

## 7. Crate boundary & dependencies

`starfish-style` depends on **both** `starfish-dom` and `starfish-css`.

`crates/style/Cargo.toml`:

```toml
[dependencies]
starfish-dom = { workspace = true }
starfish-css = { workspace = true }
```

(Both are already declared under `[workspace.dependencies]` in the root
`Cargo.toml`; only the two edges above need adding — the manifest currently has
an empty `[dependencies]`.)

Module layout for `crates/style/src/`:

```
lib.rs        // style_tree(), StyledTree, re-exports
computed.rs   // ComputedStyle, Length/Display/... value types, initial()/inherit_from()
matching.rs   // matches(), compound_matches(), match_from()
cascade.rs    // Origin, MatchedDecl, collect + sort + apply
properties.rs // apply_declaration() + per-property handlers + value helpers
ua.rs         // UA_CSS const + a cached parse
```

Re-exports from `lib.rs`:

```rust
pub use computed::{
    ComputedStyle, Display, Length, BorderStyle, TextAlign, FontWeight, LineHeight,
};
pub use starfish_css::Rgba;
pub use starfish_dom::NodeId;
```

---

## 8. Edge cases & explicit non-goals

Handled:

- Element with no matching rules → all-initial style (inherited props from
  parent). Bare `<p>hi` styles `p` block via the UA sheet.
- `class` with multiple/space-collapsed classes; missing `id`/`class`.
- Cascade tie-breaking by specificity then source order; UA vs author origin;
  `!important`.
- `Child` combinator matched correctly (M2 only stored it).
- Unitless `0` as a length; `auto` on width/height/margin.
- `border-color` defaulting to `currentColor` (= computed `color`).
- Text nodes: styled via parent (no own entry).
- Unknown properties / unparseable values → ignored, base value kept (lenient,
  never panics).

Explicit **non-goals** for M3 (documented limitations):

- No explicit `inherit`/`initial`/`unset`/`revert` keyword handling (a stray
  `inherit` leaves the base value, which equals the parent for inherited props —
  approximation, noted in §4.4).
- No `var()` / custom properties, no `calc()` (M2 keeps them as
  `Function`/`Raw`; M3 ignores → base value).
- No `@media`/`@supports` (M2 already dropped them).
- No pseudo-classes/elements, attribute selectors, sibling combinators (M2
  dropped rules using them, so they never reach M3).
- Per-side `border-*-style` / `border-*-color` and `border-<side>` shorthands —
  single shared `border_style`/`border_color` only (§1.2).
- `font-size` absolute/relative size **keywords** (`small`…`larger`) — numeric
  only (§5.6).
- No inline `style=""` (optional follow-up, §4.5).
- No box-model keywords beyond the listed subset (`box-sizing`,
  `text-decoration`, `vertical-align`, `position`, `float`, `overflow`,
  `white-space`, etc.).
- `TextAlign::Justify` / `LineHeight::Normal` final numbers are M4's call; M3
  carries the typed intent only.

---

## 9. Test plan

Unit tests in `crates/style/src/…`, structured DOM+CSS → expected computed
values. A small helper builds a `Document` (via `starfish-html::parse` or the
DOM arena API directly) and a `&[Stylesheet]` from `parse_stylesheet`, runs
`style_tree`, and asserts fields of `computed(id)`.

### 9.1 Selector matching

1. Tag/universal/id/class compound matches and non-matches against a built DOM.
2. Multi-class: element `class="a b c"` matches `.a.c`, not `.a.d`.
3. Descendant `div p` matches a `p` nested any depth under a `div`; fails with no
   `div` ancestor.
4. **Descendant backtracking:** `a b c` where the nearest `b`-ancestor lacks an
   `a`-ancestor but a farther `b`-ancestor has one → matches (proves §3.2
   backtracking).
5. Child `div > p` matches only a direct child `p`, not a grandchild.
6. Specificity of the matching rule is taken as the **max** over a rule's
   matching selectors.

### 9.2 Cascade tie-breaking

1. Two author rules same specificity, different source order → later wins.
2. Higher specificity beats lower regardless of order (`#id` beats `.cls` beats
   tag).
3. `!important` author beats normal author of higher specificity.
4. UA `display:block` for `div` overridden by author `div { display:inline }`
   (author wins by origin/source order at equal specificity).

### 9.3 Inheritance

1. `color` set on `body` is inherited by a nested `span` with no own `color`.
2. `background-color` set on `body` is **not** inherited by the child (child gets
   transparent).
3. `font-size: 20px` on parent; child `em` length (`margin: 2em`) → `Px(40)`.
4. `font-size` in `%` inherits/resolves against parent px.

### 9.4 Property parsing

1. `margin: 10px` → all four sides `Px(10)`; `margin: 1px 2px` → T/B=1, L/R=2;
   3-val and 4-val forms.
2. `padding: auto` → `Px(0)` (padding can't be auto).
3. `width: 50%` → `Percent(50)`; `width: auto` → `Auto`; `height: 0` → `Px(0)`.
4. `border: 2px solid red` → all widths `2.0`, style `Solid`, color red.
5. `font-weight: bold` → `FontWeight(700)`; `font-weight: 300` → `FontWeight(300)`.
6. `line-height: 1.5` → `Number(1.5)`; `line-height: 20px` → `Px(20)`;
   `line-height: normal` → `Normal`.
7. `font-family: "Helvetica Neue", Arial, sans-serif` →
   `["Helvetica Neue","Arial","sans-serif"]`.
8. `color: #00ff00` (arrives as `Color`) → green; `background-color: transparent`
   → `Rgba{0,0,0,0}`.
9. Unknown property `zoom: 2` and unparseable `width: bogus` → ignored, base kept.

### 9.5 UA sheet + end-to-end smoke

1. `<div><span>hi</span></div>` (no author CSS) → `div` display `Block`, `span`
   display `Inline`, both inheriting black `color` / 16px font.
2. `head`/`title`/`meta` get `display: None`.
3. `<p>` gets UA `margin: 16px 0` and block display; author `p { color: blue }`
   layered on top → blue color, UA margins retained.
4. A representative small page (heading, paragraph, list, link, nested divs) with
   a small author stylesheet → assert a handful of computed values across the
   tree. This is the milestone's "done-when" check.

---

## 10. Implementation order (for the impl agent)

1. `computed.rs`: value types + `ComputedStyle` + `initial()` + `inherit_from()`
   + the inheritance table. Unit-test `initial`/`inherit_from`.
2. `matching.rs`: `compound_matches` + recursive `match_from` + `matches`.
   Tests §9.1.
3. `properties.rs`: value helpers + per-property handlers + shorthands. Tests
   §9.4.
4. `cascade.rs`: `Origin`, `MatchedDecl` collection, sort key, apply loop.
   Tests §9.2.
5. `ua.rs` + `lib.rs`: `UA_CSS`, `style_tree` DFS wiring inheritance + cascade,
   `StyledTree`/`computed`. Tests §9.3, §9.5.
6. Add the two dependency edges to `crates/style/Cargo.toml`.
```
