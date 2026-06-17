# Roadmap — Epic 57: JS DOM round 5 (CE lifecycle / node relations / Range)

Deepens the JS DOM surface: custom-element reactions (`observedAttributes` +
`attributeChangedCallback`, `disconnectedCallback`), node-relationship queries
(`contains`/`compareDocumentPosition`/`isConnected`/`getElementsByName`/
`insertAdjacentElement`/`insertAdjacentText`), and `Range`.

Same per-milestone pipeline. Additive: pages not using these APIs are
byte-identical (inert until called).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E57-M1** | **Custom-element reactions**: a custom element's static `observedAttributes` array drives `attributeChangedCallback(name, old, new)` — fired for each present observed attribute on upgrade (and on `setAttribute` during the script run); `disconnectedCallback` fires when the element is removed. Extends E33-M3 (`connectedCallback`). | `js` | a defined element with `observedAttributes=['x']` gets `attributeChangedCallback('x', null, val)` on upgrade and on a later setAttribute (tested) | ✅ |
| **E57-M2** | **Node relations + adjacent insert**: `node.contains(other)`, `compareDocumentPosition`, `isConnected`, `document.getElementsByName`, and `insertAdjacentElement`/`insertAdjacentText`. | `js` | `a.contains(b)` is true for a descendant; `el.insertAdjacentElement('beforebegin', x)` places x; `getElementsByName('q')` finds the named controls (tested) | ☐ |
| **E57-M3** | **`Range`**: `document.createRange()`, `setStart`/`setEnd`/`selectNode`/`selectNodeContents`, `collapsed`/`commonAncestorContainer`, `cloneContents`/`extractContents`/`deleteContents`, and `toString()`. | `js` | a range over an element's contents `toString()`s its text; `deleteContents()` removes the ranged nodes (tested) | ☐ |

## Non-goals (deferred)

- Custom-element upgrade reaction queue ordering subtleties, `:defined`,
  form-associated elements / `ElementInternals`, and `adoptedCallback`.
- `Range` partial-node splitting at text offsets for extract/clone beyond
  whole-node boundaries (MVP operates on node boundaries; mid-text offsets are
  best-effort), and live-range mutation tracking.
- `Selection` (no caret/selection in a static render), `compareDocumentPosition`
  exhaustive bit flags beyond CONTAINS/CONTAINED_BY/PRECEDING/FOLLOWING.
