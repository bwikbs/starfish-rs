# Roadmap — Epic 30: @property (typed custom properties)

Registers custom properties via `@property`, giving `var()` a guaranteed
**initial value** (so an otherwise-unresolved `var(--x)` falls back to the
registered default instead of invalidating the declaration), an **inherits**
flag, and a declared **syntax**. Closes the design-system-token loop opened by
Epic 24 (`color-mix`/`env`/`attr`) and Epic 25 (container queries + logical
props): custom properties become first-class, typed, and defaulted.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a stylesheet with no `@property` resolves `var()` exactly as before (existing
tests + the golden PNG unchanged).

Current state (reference): custom properties (`--x`) cascade as
`Vec<Component>` (E13-M2); `var(--name[, fallback])` substitutes the declared
value, else the inline fallback, else the declaration is invalid
(crates/style `properties.rs` `substitute_vars`). `@property` is an unknown
at-rule today — skipped — so a registered property contributes no initial value.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E30-M1** | **Parse + capture `@property`**: `@property --name { syntax: "<type>"; inherits: true\|false; initial-value: <value>; }` → a `PropertyRule { name, syntax, inherits, initial_components }` on the `Stylesheet` (mirroring the `@font-face` capture). A registration missing the required `syntax`/`inherits` descriptors is dropped (kept only when well-formed, per spec — except `syntax: "*"` needs no initial value). | `css` | `@property --gap { syntax: "<length>"; inherits: false; initial-value: 8px }` is captured with name `--gap` and initial `8px`; a sheet without `@property` is byte-identical (tested) | ☐ |
| **E30-M2** | **`var()` initial-value fallback**: thread the registered initial values into the cascade so an unresolved `var(--registered)` with no inline fallback resolves to the property's `initial-value` (instead of invalidating). Inline `var(--x, fallback)` still prefers the fallback; an actually-declared `--x` still wins over the initial. | `css`, `style` | with `@property --c { syntax:"<color>"; inherits:false; initial-value: red }`, a `color: var(--c)` on an element that never sets `--c` renders red; setting `--c: blue` overrides it (tested + visual) | ☐ |
| **E30-M3** | **`inherits` + syntax-validated initial**: a non-inheriting registered property (`inherits: false`) resolves to its initial value on every element rather than inheriting the parent's computed value when unset; and a registration whose `initial-value` doesn't parse against its `syntax` (a small subset: `<length>`/`<color>`/`<number>`/`<percentage>`/`<integer>`/`*`) is dropped. | `css`, `style` | `inherits: false` keeps a child at the initial value even when the parent set the property; an `initial-value` that doesn't match `syntax` invalidates the whole `@property` (tested) | ☐ |

## Non-goals (deferred)

- The full `syntax` grammar: only the single-type keywords above plus `*`
  (universal). No multipliers (`+`/`#`), no `|` alternation, no combinations,
  no custom idents in the syntax string.
- Animating a registered custom property (typed interpolation of `var()`-backed
  values); `@property` only affects static resolution here.
- Registration via the CSS-OM `CSS.registerProperty()` JS API.
- Per-element `inherits` subtleties for properties registered multiple times
  (last `@property` for a name wins); cross-sheet ordering edge cases.
- Computed-value-time type coercion/clamping beyond accepting the initial value
  as-is once it parses against the (subset) syntax.
