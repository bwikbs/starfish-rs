# Roadmap — Epic 52: CSSOM & DOM traversal (JS)

Adds read-side CSSOM and DOM-traversal JS APIs on top of the existing js crate
(E8/E19 DOM bindings, DomState.author_sheets): `document.styleSheets` +
`CSSStyleSheet.cssRules`, `NodeIterator`/`TreeWalker`, and `DOMParser`/
`XMLSerializer`.

Same per-milestone pipeline. Additive: pages not using these APIs are
byte-identical (the globals exist but are inert).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E52-M1** | **`document.styleSheets` + `CSSStyleSheet.cssRules`**: `document.styleSheets` is a list of `CSSStyleSheet` (from the collected author sheets); each exposes `cssRules` → `CSSStyleRule`s with `selectorText` and a `cssText`/`style.cssText`. Read-only MVP. | `js` | `document.styleSheets.length`, `document.styleSheets[0].cssRules[0].selectorText` read the parsed author CSS (tested) | ☐ |
| **E52-M2** | **`NodeIterator` / `TreeWalker`**: `document.createNodeIterator(root, whatToShow)` / `createTreeWalker(root, whatToShow, filter)` with `nextNode`/`previousNode` (+ TreeWalker `parentNode`/`firstChild`/`nextSibling`), honoring `whatToShow` (SHOW_ELEMENT/SHOW_TEXT/SHOW_ALL) and a function/`acceptNode` filter. | `js` | `createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT)` walks only element nodes via nextNode (tested) | ☐ |
| **E52-M3** | **`DOMParser` + `XMLSerializer`**: `new DOMParser().parseFromString(html, "text/html")` → a Document whose `body`/`querySelector` work; `new XMLSerializer().serializeToString(node)` → markup. | `js` | parseFromString builds a queryable tree; serializeToString round-trips an element to HTML (tested) | ☐ |

## Non-goals (deferred)

- CSSOM write side (`insertRule`/`deleteRule`, mutating `style.cssText` to
  restyle), `@media`/`@keyframes` rule objects beyond style rules, and
  `CSSRule` type constants beyond style rules.
- `NodeFilter.FILTER_REJECT` vs `FILTER_SKIP` subtree semantics (MVP treats
  reject==skip), and live TreeWalker `currentNode` mutation edge cases.
- `DOMParser` for `text/xml`/SVG documents (HTML only), and `XMLSerializer`
  XML-namespace fidelity.
