# Roadmap — Epic 33: Shadow DOM & Web Components

The first **encapsulation** epic: a shadow tree that the cascade, layout, and
paint walk as a **flattened (composed) tree** instead of the raw light-DOM child
list, `<slot>` distribution of light children into the shadow tree, scoped
styling (`:host`/`::slotted`, `<style>` encapsulation), and `customElements`
upgrades. Builds on the existing arena DOM (`crates/dom` linked-list children),
the side-table style walk (`crates/style` `style_node`, already weaves in
`::before`/`::after`), and the box-tree builder (`crates/layout`
`build_children`, already injects markers + generated content).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive:
a page with **no** shadow root must render byte-identically (existing tests + the
golden PNG unchanged).

Current state (reference): the DOM arena (`crates/dom/src/lib.rs`) stores
`Node{kind, parent, first/last_child, prev/next_sibling}`; `Document::children`
walks the `first_child`/`next_sibling` chain. The cascade (`style_node`) and the
box builder (`build_children`) both enumerate `doc.children(elem)` directly.
There is **no** notion of a shadow root, slot, host, or composed tree anywhere
(grep-confirmed). JS exposes `NodeHandle` over a `SharedDoc` with a `DomState`
identity cache; `import_subtree` deep-copies a parsed subtree into the arena (the
`innerHTML` graft path).

Design spine (locked): a shadow root is a real arena node whose subtree is the
shadow tree, but it is **NOT** linked into its host's `first_child` chain — it is
reached via a `Document` side map `host NodeId → ShadowRoot NodeId` (+ a mode
flag). So `doc.children(host)` stays light-DOM-only and every non-shadow page is
byte-identical. Cascade/layout gain a **composed-tree** walk: when an element has
a shadow root, they recurse into the shadow tree (and a `<slot>` there expands to
the host's distributed light children) instead of the light children.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E33-M1** | **Shadow attach + composed-tree render**: add a shadow root to the arena (`Document::attach_shadow(host, mode)` + `shadow_root(host)` side map; `ShadowMode{Open,Closed}`). Parse **declarative shadow DOM** `<template shadowrootmode="open\|closed">` at HTML-parse time → attach its content as the host's shadow tree (so shadow trees are testable with no JS). JS: `element.attachShadow({mode})` returns the shadow root; `element.shadowRoot` returns it (open) or `null` (closed). Cascade (`style_node`) and the box builder (`build_children`) walk the **composed tree**: a host with a shadow root renders its shadow tree; its light-DOM children are **not** rendered (no slots yet → unslotted = hidden, per spec). | `dom`, `html`, `js`, `style`, `layout` | a host with a declarative open shadow root renders the shadow content (not the light children) to PNG; `el.shadowRoot` is the root when open and `null` when closed; a page with no shadow root is byte-identical (tested + visual) | ✅ |
| **E33-M2** | **`<slot>` distribution**: light-DOM children are distributed into the shadow tree's `<slot>` elements — the default (unnamed) slot collects children with no `slot=` attribute, named slots match `slot="name"` ↔ `<slot name="name">`. A slot with no assigned nodes renders its **fallback** content (its own children). The composed-tree walk in cascade + layout expands each `<slot>` to its assigned light children (styled by their original light-DOM context, laid out at the slot's position). JS: `slot.assignedNodes()`/`assignedElements()`; `node.assignedSlot`. | `dom`, `js`, `style`, `layout` | light children with `slot="foo"` render at the matching `<slot name="foo">` position and unnamed children at the default slot; an empty slot shows its fallback; `assignedNodes()` lists the distributed nodes (tested + visual) | ☐ |
| **E33-M3** | **Scoped styling + custom elements**: a `<style>` inside a shadow root is **scoped** to that tree (its rules match only shadow-tree nodes; outer author rules don't match shadow nodes except via inheritance). `:host`/`:host(<sel>)` match the shadow host from inside; `::slotted(<sel>)` matches distributed light children. `customElements.define(name, ctor)` registers an element; matching elements are **upgraded** during script run (constructor + `connectedCallback` invoked in the run-to-quiescence pass). | `css`, `js`, `style` | a shadow `<style>{p{color}}` colors only shadow `<p>`s (not light `<p>`s) and vice-versa; `:host` styles the host; `::slotted(span)` styles distributed spans; `customElements.define` upgrades a matching element and its `connectedCallback` runs before the render snapshot (tested + visual) | ☐ |

## Non-goals (deferred)

- **Closed-mode access control** beyond `shadowRoot` returning `null` (no internal
  slot hiding from same-realm code); `delegatesFocus`, `slotAssignment:"manual"`
  (only named/automatic distribution), and `clonable`/`serializable` options.
- **Slot mutation semantics**: `slotchange` events, dynamic re-distribution
  observers, and `slot.assign()` manual assignment. Distribution is computed once
  for the render snapshot.
- **`::part()`/`::theme()`/`exportparts`**, `adoptedStyleSheets`, and constructable
  stylesheets; `:host-context()`; CSS shadow-piercing combinators (none exist).
- **Custom element lifecycle depth**: `disconnectedCallback`/`adoptedCallback`,
  `attributeChangedCallback` + `observedAttributes`, the reaction queue / upgrade
  reaction ordering subtleties, `:defined`, and form-associated custom elements
  (`ElementInternals`). Only `define` + constructor + `connectedCallback` at the
  one-shot snapshot.
- **Declarative custom elements**, `<template>` general cloning semantics beyond
  the `shadowrootmode` attach path, and nested/recursive declarative shadow roots
  beyond a single level per host.
- Event **retargeting**/composed-path across shadow boundaries, focus delegation,
  and `::slotted` deep combinators.
