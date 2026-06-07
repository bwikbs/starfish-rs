# Roadmap — Epic 8: JavaScript web APIs

Extends the Boa-backed JS engine with the web APIs real scripts reach for. Same
per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Still a one-shot
run-to-quiescence renderer.

| Milestone | Scope | Crates | Done-when |
|-----------|-------|--------|-----------|
| **E8-M1** | DOM surface: `innerHTML` (read = serialize the subtree; write = parse the HTML fragment into nodes and replace children), `outerHTML` (read), `getComputedStyle(el)` → a read-only style object with the resolved values, `cloneNode(deep)`, `insertAdjacentHTML`, `Element.remove()`, `children`/`firstElementChild`/etc. helpers. | `js`, `dom`, `html`, `style` | A script setting `el.innerHTML` rebuilds the subtree and the render reflects it; `getComputedStyle` returns resolved values (tested + visual) |
| **E8-M2** | Networking from JS: `fetch(url)` returning a Promise resolving to a Response (`.text()`/`.json()`/`.ok`/`.status`), backed by the existing `ResourceLoader`, drained through Boa's job queue during quiescence; a minimal synchronous `XMLHttpRequest`. | `js`, `net` | A script `fetch`-ing a (local/data) URL and mutating the DOM with the result renders correctly after quiescence (tested) |
| **E8-M3** | Storage + misc: in-memory `localStorage`/`sessionStorage` (getItem/setItem/removeItem/clear/length/key), `JSON` (Boa built-in — expose/confirm), `Element.dataset`, `classList` already done, `URL`/`URLSearchParams` (basic), `console.dir`/`table` (basic). | `js` | Scripts using localStorage + JSON + dataset run and affect the DOM/render (tested) |

## Non-goals (deferred)

- Asynchronous interactivity beyond the bounded quiescence drain; WebSockets, EventSource,
  service workers, IndexedDB, Cache API, Web Workers, `import()` modules.
- `innerHTML` write running embedded `<script>`s (HTML5 says inserted scripts via innerHTML
  don't execute — keep that behavior; note), `DOMParser`/`XMLSerializer` full surface,
  `document.write`.
- Real persistence for storage (in-memory only, per render), cookies, CORS enforcement
  from `fetch` (the loader fetches; no policy), streaming bodies, `FormData`, `Blob`/`File`.
- `getComputedStyle` returning used/layout values (it returns the resolved/computed style,
  not post-layout geometry like `offsetWidth`) — note the distinction; `getBoundingClientRect`
  may be a stretch (needs layout access from JS) — defer or basic.
