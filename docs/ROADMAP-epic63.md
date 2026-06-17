# Roadmap — Epic 63: HTML <template> & DocumentFragment

`<template>` content currently RENDERS (a bug — it must be inert). This epic makes
`<template>` not render and exposes `template.content`, adds `DocumentFragment`,
and handles `<noscript>`.

Same per-milestone pipeline. Additive where possible; the `<template>` fix is a
behavior correction (template content stops rendering) — non-template pages stay
byte-identical.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E63-M1** | **`<template>` inert + `.content`**: UA `template { display: none }` so its content is not rendered; JS `template.content` returns a fragment-like wrapper whose `children`/`querySelector` see the template's parsed children. | `style`, `js` | a page with `<template><p>x</p></template>` renders nothing for it; tmpl.content.querySelector finds the p (tested + visual) | ✅ |
| **E63-M2** | **`DocumentFragment`**: `document.createDocumentFragment()` → a fragment (nodeType 11); `appendChild`/`append` of a fragment MOVES its children into the target (the fragment empties); a fragment is not itself rendered. | `dom`, `js` | appending a fragment with 2 children to an element adds both children and empties the fragment (tested) | ✅ |
| **E63-M3** | **`<noscript>`**: since scripts run, `<noscript>` content is not rendered (UA `noscript { display: none }`); its raw contents (parsed as text in scripting-enabled mode) don't leak into layout. | `style`/`html` | a `<noscript><p>fallback</p></noscript>` renders nothing (tested + visual) | ✅ |

## Non-goals (deferred)

- A true separate "template contents document" owned by a different document —
  MVP keeps the template's children in the main arena (inert via display:none)
  and `.content` is a proxy over them; declarative `shadowrootmode` (E33) is
  unaffected.
- `DocumentFragment` as a Range/`createContextualFragment` result beyond the
  basic createDocumentFragment + move-on-append.
- `<noscript>` rendering its contents in scripting-DISABLED mode (the engine
  always runs scripts).
