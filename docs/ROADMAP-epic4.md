# Roadmap — Epic 4: JavaScript

Adds a JavaScript engine so `<script>`s run and can read and mutate the DOM before the
final render. Engine: **Boa** (pure-Rust). Same per-milestone agent pipeline (design →
analysis → implementation → review → verification), each landing as its own commit + push.

Model: this is still a one-shot renderer (no live interaction loop). The value of JS here
is **run-to-quiescence then render** — load the page, execute its scripts in order
(mutating the DOM, firing load events, draining bounded timers), then style + layout +
paint the resulting DOM state to PNG.

Architectural crux (decided in E4-M1): the `Document` becomes shared+mutable
(`Rc<RefCell<…>>` or equivalent) so JS host objects can mutate the same arena that layout
and paint later read.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E4-M1** | Integrate Boa; execute `<script>` (inline + external via the ResourceLoader) in document order; `console.log/warn/error` captured; minimal `window`/`globalThis` globals; the shared-`Document` ownership refactor. No DOM bindings yet (or a trivial read-only probe). | new `js`, `paint` | A page's inline + linked scripts run, console output is captured, errors are non-fatal, and the page still renders (tested) | ✅ |
| **E4-M2** | DOM bindings: `document` (getElementById, querySelector/All, createElement, createTextNode, body/documentElement), `Element`/`Node` (tagName, id, className/classList, textContent, getAttribute/setAttribute/removeAttribute, appendChild/removeChild/insertBefore, parentNode/childNodes/children), `element.style` (set inline properties). JS mutations write to the arena; the post-script DOM is what gets styled/laid-out/painted. | `js`, `dom` | A script that creates/moves/edits nodes and sets styles changes the rendered output (tested + visual) | ✅ |
| **E4-M3** | Events + timers + load sequence: `addEventListener` + `dispatchEvent` (a minimal Event), `DOMContentLoaded`/`load` fired after parse/scripts, inline `on*` handler attributes (optional), `setTimeout`/`setInterval`/`clearTimeout` drained against bounded virtual time, run-to-quiescence ordering. | `js` | Scripts using DOMContentLoaded + setTimeout mutate the DOM and the bounded timer queue drains before the final render (tested) | ✅ |

**Epic 4 complete.** 414 workspace tests, clippy clean. Scripts run, mutate the DOM (and fire events/timers), and the run-to-quiescence DOM state is what gets rendered.

## Non-goals (deferred / out of scope for this epic)

- Live interactivity (no input events, no animation loop, no rAF beyond a bounded drain).
- Network from JS (`fetch`, `XMLHttpRequest`), `localStorage`, cookies, history, navigation.
- Full DOM/HTML/CSSOM spec surface (only the pragmatic subset above); `innerHTML` write
  (HTML-parsing-into-a-node) is a stretch — note if cheap, else defer.
- Web APIs: Canvas/WebGL/Web Audio/workers/modules (`import`)/Promises-microtask subtleties
  beyond what Boa gives, `MutationObserver`, Shadow DOM, custom elements.
- Performance/JIT (Boa is a tree-walking/bytecode interpreter; fine for one-shot).
- Security sandboxing beyond Boa's own isolation; scripts are trusted (local tool).
