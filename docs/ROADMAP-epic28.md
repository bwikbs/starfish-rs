# Roadmap — Epic 28: CSS Nesting

Native CSS nesting (CSS Nesting Module): rules written inside other rules, with
`&` referring to the parent. The engine flattens nested rules into ordinary
top-level rules at parse time by composing the parent and child selector text,
so the cascade, specificity, and matching machinery are untouched — nesting is a
purely syntactic front-end transform.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a stylesheet with no nested rules must parse byte-identically (existing tests +
the golden PNG unchanged).

Current state (reference): `parse_qualified_rule` (crates/css `parser.rs`)
collects a prelude up to `{`, then `parse_declaration_block` reads only
declarations to the matching `}` — a `{` inside the block (a nested rule) is not
recognized; its tokens are consumed as junk declarations. So `.a { .b { … } }`
today drops the `.b` rule entirely. There is **no** `&`, no nested at-rules.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E28-M1** | **Basic `&` nesting + flattening**: `parse_declaration_block` recognizes a nested qualified rule (a run of tokens up to a `{` that isn't a declaration's `value`), captures its prelude + block, and the parser flattens it into a top-level rule whose selector is the child prelude with each `&` replaced by the parent selector (parenthesized as `:is(…)` when the parent is a selector list). The parent rule keeps its own declarations. One level deep. | `css` | `.card { color: red; & .title { color: blue } }` yields two rules: `.card { color: red }` and `.card .title { color: blue }` (the `&` form `&.active`, `& > p` compose correctly); a non-nested sheet is byte-identical (tested) | ✅ |
| **E28-M2** | **Implicit-`&` + multi-level nesting**: a nested rule whose selector doesn't contain `&` is treated as a descendant (`&` prepended), so `.card { .title { … } }` ≡ `.card { & .title { … } }`. Nesting recurses to arbitrary depth (flattening is applied bottom-up), and a nested rule may itself carry both declarations and further nested rules. Relative selectors (`> p`, `+ li`) at the start of a nested selector get the implicit `&`. | `css` | `.a { .b { .c { color: red } } }` flattens to `.a .b .c { color: red }`; `.list { & > li { color: red; &:hover { … } } }` composes both levels (tested) | ✅ |
| **E28-M3** | **Nested at-rules**: `@media`/`@supports`/`@container` blocks nested inside a style rule — the inner rules flatten against the enclosing selector and the at-rule condition is hoisted to a top-level conditional block (reusing the existing `MediaBlock`/`SupportsBlock`/`ContainerBlock` capture + the source-order interleave). `.card { @media (width >= 400px) { color: red } }` ≡ `@media (width >= 400px) { .card { color: red } }`. | `css`, `style` | a `@media` nested in a rule applies its declarations to the enclosing selector only when the query matches (tested + visual) | ☐ |

## Non-goals (deferred)

- The `@nest` legacy at-rule prefix (obsolete; only the bare nesting syntax).
- Nesting that changes specificity semantics beyond the `:is()` wrapping rule
  (e.g. the exact `&`-vs-`:is()` specificity nuance for compound parents is
  approximated by the `:is()` flattening).
- Nested `@layer`/`@scope`/`@keyframes`; declarations appearing *after* a nested
  rule are still collected (the spec allows it), but their cascade order versus
  the nested block is approximated by source order.
- CSSOM round-tripping (the engine has no serialization); nesting only affects
  the parsed rule set used for the cascade.
