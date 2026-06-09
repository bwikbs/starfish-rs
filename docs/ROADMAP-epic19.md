# Roadmap — Epic 19: JS DOM & web-API expansion

Broadens the JavaScript surface: the ergonomic DOM-manipulation APIs scripts
actually use (`classList`, element traversal, modern insertion methods),
layout-dependent geometry queries (`getBoundingClientRect`, `offsetWidth` …,
which force an on-demand layout mid-script — the seam deferred since Epic 12),
and the event-loop/observer/navigation APIs (`requestAnimationFrame`,
`MutationObserver`, `history`/`location`, `matchMedia`).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. The render output is still
the run-to-quiescence DOM painted once, so new APIs that don't change the DOM must
leave existing pages byte-identical (existing tests + the golden PNG unchanged).

Current state (reference): the Boa engine runs `<script>`s with DOM bindings
(document/Element/Node, querySelector(All), createElement, get/setAttribute,
`element.style`, `innerHTML`/`outerHTML`/`getComputedStyle`/`cloneNode`/
`insertAdjacentHTML`), events (addEventListener/dispatchEvent, DOMContentLoaded/
load), bounded virtual-time `setTimeout`/`setInterval`, `fetch`/`XMLHttpRequest`,
`localStorage`/`sessionStorage`/`URL`/`URLSearchParams`/`JSON`/`dataset`. There is
**no** `classList`, no element-only traversal accessors (`children`,
`firstElementChild`, `nextElementSibling`, `closest`, `matches`), no modern
insertion (`append`/`prepend`/`before`/`after`/`replaceWith`/`remove` on the full
set), **no layout-dependent geometry** (`getBoundingClientRect`/`offsetWidth`/
`clientWidth`/`scrollWidth` — the JS realm has no FontDb and layout runs only once
after scripts), no `requestAnimationFrame`, no `MutationObserver`, no `history`/
`location` mutation, no `matchMedia`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E19-M1** | **classList + traversal/manipulation**: `element.classList` (`add`/`remove`/`toggle`/`contains`/`replace`/`item`/`length`, reflecting `class`); element-only traversal (`children`, `firstElementChild`/`lastElementChild`, `nextElementSibling`/`previousElementSibling`, `parentElement`, `childElementCount`); `closest(sel)`, `matches(sel)` (reuse the selector matcher); modern insertion (`append`/`prepend`/`before`/`after`/`replaceWith` accepting nodes or strings, `el.remove()`); `getAttributeNames`, `hasAttribute`, `toggleAttribute`. | `js` | A script using `classList.toggle`, `closest`, `nextElementSibling`, and `append`/`before` mutates the DOM and the result renders (tested) | ✅ |
| **E19-M2** | **Layout geometry (on-demand layout)**: make a layout pass runnable DURING script execution — build (or reuse) a FontDb in the JS realm and lay out the current DOM on demand — so `getBoundingClientRect()` (x/y/width/height/top/left/right/bottom), `offsetWidth`/`offsetHeight`/`offsetTop`/`offsetLeft`/`offsetParent`, `clientWidth`/`clientHeight`, and `scrollWidth`/`scrollHeight` return real laid-out values; cache the layout and invalidate it on DOM/style mutation (mutation-version keyed, like the getComputedStyle memo). | `js`, `layout`, `paint` | `el.getBoundingClientRect()` and `el.offsetWidth` return correct geometry mid-script, recomputed after a mutation (tested) | ✅ |
| **E19-M3** | **Event loop, observers & navigation**: `requestAnimationFrame(cb)` (run in the bounded virtual-time loop alongside setTimeout, with a synthetic timestamp); `MutationObserver` (`observe`/`disconnect`/`takeRecords`, childList/attributes/characterData + subtree, records delivered as microtasks); `history.pushState`/`replaceState` + `location` (href/pathname/search/hash read + `hash`/`search` write updating the URL, no navigation); `matchMedia(q)` → `MediaQueryList` (`matches` against the render viewport, reusing the `@media` evaluator). | `js` | `requestAnimationFrame` fires, a `MutationObserver` reports a childList change, `matchMedia('(max-width:600px)').matches` reflects the viewport, `history.pushState` updates `location` (tested) | ☐ |

## Non-goals (deferred)

- Real interactive scrolling / a live event loop (the render is still one-shot,
  run-to-quiescence); `requestAnimationFrame` is sampled in virtual time, not a
  real 60fps loop; `IntersectionObserver`/`ResizeObserver` real callbacks.
- `getBoundingClientRect` accounting for scroll offsets / transforms beyond the
  laid-out box; `getClientRects()` per-line rects; sub-pixel snapping nuances.
- `offsetParent` full positioned-ancestor walk edge cases; `scrollIntoView`,
  `scrollTo`, element scroll positions (no scrollable viewport).
- `MutationObserver` `attributeOldValue`/`characterDataOldValue` full record
  fidelity, observer reentrancy ordering subtleties; `MutationRecord` complete
  field set beyond the common ones.
- `history` real session stack / `popstate` navigation, `location.assign`/
  `replace`/`reload` actually navigating, `pushState` URL same-origin checks.
- `matchMedia` change listeners firing on viewport change (one-shot render has a
  fixed viewport); full media-feature coverage beyond the existing `@media` set.
- `Node`-level APIs not in common use (`compareDocumentPosition`, `normalize`,
  ranges/selection, `TreeWalker`/`NodeIterator`).
