# Roadmap — Epic 62: CSS @scope

Adds the `@scope` at-rule (scoped styling) on top of the existing at-rule capture
machinery (`@media`/`@supports`/`@layer`/`@container` blocks in `crates/css`
`Stylesheet`, applied per-element in the cascade like `@container`).

Same per-milestone pipeline. Additive: stylesheets without `@scope` are
byte-identical (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E62-M1** | **`@scope (<root>) { rules }`**: parse a `@scope` block with a scope-root selector; its inner rules match an element only when the element matches the inner selector AND is within the scope (a descendant-or-self of an element matching `<root>`). | `css`, `style` | `@scope (.card) { p { color: red } }` colors `<p>`s inside `.card` but not p elsewhere (tested + visual) | ✅ |
| **E62-M2** | **`@scope (<root>) to (<limit>)`**: the scope ends (exclusive) at elements matching `<limit>` — an element deeper than a `<limit>` boundary is out of scope. | `css`, `style` | `@scope (.card) to (.content) { p {…} }` styles `<p>`s between `.card` and the `.content` boundary but not inside .content (tested) | ✅ |
| **E62-M3** | **`:scope` + prelude-less `@scope`**: `:scope` inside the block refers to the scope root; a bare `@scope { }` (no prelude, e.g. in a scoped context) scopes to its parent — MVP: bare `@scope` scopes to `:root`. `&` in scoped rules refers to the scope root. | `css`, `style` | `@scope (.card) { :scope { border:… } & .x {…} }` styles the root + scoped descendants (tested) | ✅ |

## Non-goals (deferred)

- `@scope` proximity-based tie-breaking (the "scoping proximity" cascade step)
  beyond normal specificity/order; weak scoping interaction with `:where`.
- Declarative `<style>`-implicit scoping roots, and `@scope` nested inside other
  conditional rules' exotic combinations.
- `@scope` donut-scope `to()` with complex multi-element limit chains beyond a
  single limit selector match.
