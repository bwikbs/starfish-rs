# Roadmap — Epic 29: selector & pseudo-class expansion

Fills the remaining gaps in selector matching: the **type-indexed structural
pseudos** (`:first-of-type`/`:last-of-type`/`:only-of-type` + the `:nth-last-*`
counterparts), the **`of <selector>` argument** to `:nth-child`/`:nth-last-child`
plus the case-sensitivity flag (`[attr=v s]`), and the **link/UI pseudos**
(`:any-link`/`:link`, `:default`, `:placeholder-shown`, `:scope`, `:lang()`).
All are matching-time additions on top of the existing selector engine; the box
tree, cascade, and layout are unaffected.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
selectors not using the new pseudos keep matching identically (existing tests +
the golden PNG unchanged).

Current state (reference): the selector model (crates/css `selector.rs`) already
has `:first-child`/`:last-child`/`:only-child`, `:nth-child`/`:nth-of-type`
(An+B), `:root`/`:empty`, `:is`/`:where`/`:has`/`:not`, the form-state pseudos,
and attribute selectors with the `i` (case-insensitive) flag. Matching lives in
crates/style `matching.rs`. There is **no** `:*-of-type` (beyond `:nth-of-type`),
no `:nth-last-*`, no `:nth-child(… of S)`, no `s` attribute flag, and no
`:any-link`/`:default`/`:placeholder-shown`/`:scope`/`:lang()`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E29-M1** | **Type-indexed + last structural pseudos**: `:first-of-type`, `:last-of-type`, `:only-of-type`, and `:nth-last-child(An+B)` / `:nth-last-of-type(An+B)` (counting from the end). Reuse the sibling-index machinery, counting only same-tag siblings for the `*-of-type` forms and from the last sibling for the `:nth-last-*` forms. | `css`, `style` | `li:last-of-type` matches the last `<li>` among siblings even with trailing non-`li`s; `:nth-last-child(2)` matches the second-from-last child; `:only-of-type` matches a sole element of its tag (tested) | ✅ |
| **E29-M2** | **`of S` argument + case-sensitivity flag**: `:nth-child(An+B of <selector-list>)` / `:nth-last-child(… of S)` — index only among siblings matching `S`; and the `[attr=value s]` case-SENSITIVE attribute flag (mirroring the existing `i`). | `css`, `style` | `:nth-child(1 of .item)` matches the first `.item` sibling (skipping non-`.item`s); `[data-x=A s]` matches `A` but not `a` (tested) | ✅ |
| **E29-M3** | **Link + UI pseudos**: `:any-link`/`:link` (any element with an `href`, i.e. `<a href>`/`<area href>`), `:default` (a checked checkbox/radio or a `<option selected>` / the default submit button), `:placeholder-shown` (an empty `<input>`/`<textarea>` with a `placeholder`), `:scope` (matches the scoping root — the document element here), and `:lang(xx)` (the element's/ancestor's `lang` matches `xx` or `xx-*`). | `css`, `style` | `a:link` matches `<a href>` but not a bare `<a>`; `:placeholder-shown` matches an empty placeholdered input; `input:default` matches a pre-checked box; `:lang(en)` matches under `lang="en-US"` (tested + visual) | ✅ |

## Non-goals (deferred)

- `:nth-child(… of S)` where `S` itself contains combinators or `:has()` (only
  compound/simple `S` is indexed; complex `S` falls back to never-match).
- Interactive/temporal pseudos that need a live UA state: `:hover`, `:focus`,
  `:focus-within`, `:focus-visible`, `:active`, `:visited`, `:target`,
  `:target-within` (still `NeverMatch`).
- `:user-invalid`/`:valid`/`:invalid`/`:in-range`/`:out-of-range` form-validation
  pseudos; `:autofill`; `:modal`; `:picture-in-picture`.
- `:lang()` with comma lists / wildcard ranges (`:lang(*-CH)`); the full BCP-47
  extended filtering — only the simple `xx` / `xx-*` prefix match.
- `:dir()`, `:host`/`:host-context`/`::slotted` (no shadow DOM), `:state()`.
