# Roadmap — Epic 43: JS round 3 (observers & utility APIs)

Adds the remaining common observer + utility JS APIs on top of the one-shot
run-to-quiescence model (E19-M3 MutationObserver, E19-M2 on-demand layout
geometry `getBoundingClientRect`): `ResizeObserver`, `IntersectionObserver`, and
the utilities `structuredClone` / `queueMicrotask` / `AbortController`.

In the one-shot renderer an observer fires ONCE for its initial observation
(during run-to-quiescence), with geometry from the on-demand layout; its callback
mutations are reflected in the render snapshot.

Same per-milestone pipeline. Additive: pages not using these APIs are
byte-identical (the globals exist but are inert).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E43-M1** | **`ResizeObserver`**: `new ResizeObserver(cb)` + `observe`/`unobserve`/`disconnect`; fires once during quiescence with `[{ target, contentRect:{x,y,width,height}, borderBoxSize, contentBoxSize }]` from the element's laid-out content box. | `js` | `new ResizeObserver(es=>...).observe(el)` runs the callback with `contentRect.width` = el content width (tested) | ✅ |
| **E43-M2** | **`IntersectionObserver`**: `new IntersectionObserver(cb, opts)` + `observe`/`unobserve`/`disconnect`/`takeRecords`; fires once with `[{ target, isIntersecting, intersectionRatio, boundingClientRect, intersectionRect, rootBounds }]` computed against the viewport (root=null). | `js` | observing an on-screen element reports `isIntersecting:true` with ratio>0; an off-screen (far below) element reports `isIntersecting:false` (tested) | ☐ |
| **E43-M3** | **Utilities**: `structuredClone(value)` (deep clone of JSON-ish values incl. arrays/objects/Map/Set/Date), `queueMicrotask(fn)`, and `AbortController`/`AbortSignal` (`.signal`, `.abort()`, `.aborted`, `addEventListener('abort')`). | `js` | `structuredClone({a:[1,2]})` deep-copies; `queueMicrotask` runs before the next macrotask; `AbortController.abort()` sets `signal.aborted` + fires `abort` (tested) | ☐ |

## Non-goals (deferred)

- ResizeObserver `box` option (`border-box`/`device-pixel-content-box`) beyond
  reporting both sizes; re-delivery on subsequent layout changes (one-shot only).
- IntersectionObserver `threshold` array re-fire semantics, `rootMargin`, a
  non-null `root` element, and per-threshold callbacks (single initial fire).
- `structuredClone` transferables / circular-ref via the real algorithm beyond a
  best-effort deep clone; `AbortSignal.timeout`/`.any()`.
