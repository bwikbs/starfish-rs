# Roadmap — Epic 42: CSS values round 3 (math / env / @counter-style)

Extends the value layer: more CSS math functions (`crates/style/src/calc.rs`
already does `calc`/`min`/`max`/`clamp`/`round`/`mod`/`rem`), the `env()`
function, and `@counter-style` custom counters (feeding the existing
`counters.rs` formatter used by list markers).

Same per-milestone pipeline. Additive: values not using these render
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E42-M1** | **Math functions**: `abs`/`sign`/`sqrt`/`pow`/`hypot`/`exp`/`log` and the trig set `sin`/`cos`/`tan`/`asin`/`acos`/`atan`/`atan2` (angle args in deg/rad/grad/turn) plus `pi`/`e` constants. Folded to a constant when args are basis-free (percent-bearing args → unsupported, leniently dropped). | `style` | `calc(sin(30deg) * 100px)` resolves to 50px; `calc(sqrt(16) * 10px)` to 40px; abs(-5px) to 5px (tested) | ✅ |
| **E42-M2** | **`env()`**: `env(<name> [, <fallback>])` resolves a UA environment variable; known insets (`safe-area-inset-*`, `titlebar-area-*`) resolve to 0, an unknown name uses the fallback (or drops the declaration). Works wherever `var()` does. | `style` | `padding: env(safe-area-inset-top, 12px)` uses 12px (no device inset); env() resolves anywhere var does, insets to 0 (tested) | ✅ |
| **E42-M3** | **`@counter-style`**: parse `@counter-style <name> { system; symbols; additive-symbols; suffix; prefix; ... }` (systems `cyclic`/`fixed`/`symbolic`/`alphabetic`/`numeric`); `list-style-type: <name>` formats markers with it. | `css`, `style` | a `@counter-style box { system: cyclic; symbols: "▪"; }` + `list-style-type: box` renders that symbol as the marker; a numeric custom style counts in its symbol set (tested + visual) | ☐ |

## Non-goals (deferred)

- `@counter-style` `range`, `pad`, `negative`, `speak-as`, `fallback`, and
  `extends` systems; `symbols()` inline function.
- `env()` with multiple comma-separated names / the 4-value inset syntax, and
  any actual non-zero device inset values.
- Math: `calc()`-nesting of these into percent-dependent expressions (only
  constant folding); `round()` strategies are already done; type checking of
  angle-vs-number beyond what's needed to fold.
