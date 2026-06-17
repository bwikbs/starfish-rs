# Roadmap — Epic 71: CSS Anchor Positioning

CSS Anchor Positioning (`anchor-name`, `position-anchor`, the `anchor()` function
in inset properties, `position-area`, `anchor-size()`) is entirely absent. It lets
an absolutely-positioned element position itself relative to a named *anchor*
element's box.

Maps onto existing absolute positioning: the anchor element is laid out in normal
flow (its border-box rect is known after `layout_block`), so a pre-pass between
`layout_block` and `layout_absolutes` (`crates/layout/src/lib.rs` `layout()`)
collects `anchor-name → border-box Rect`, and `layout_abs_box`
(`crates/layout/src/block.rs`) resolves `anchor()` insets against that rect.

All anchor state lives in ONE boxed `anchor: Option<Box<AnchorData>>` field on
`ComputedStyle` (8 bytes) to respect the recursive stack-depth limit. Default
`None` → byte-identical to today.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E71-M1** | **`anchor-name` + `position-anchor` + `anchor()` insets**: `anchor-name: --x` registers the element as an anchor; `position-anchor: --x` sets the default anchor for a positioned element; `top/right/bottom/left: anchor([<name>] <side>)` (side = `top`/`right`/`bottom`/`left`/`center`/`start`/`end`/`<percentage>`) resolves to the anchor's border-box edge in the abspos containing block. A pre-pass collects the name→rect map. | `style`, `layout` | an abspos box with `position-anchor:--a; top:anchor(--a bottom); left:anchor(--a left)` sits flush under the `--a` anchor's bottom-left (tested + visual) | ✅ |
| **E71-M2** | **`position-area`**: `position-area: <row> <col>` (e.g. `top center`, `bottom right`, `start end`, single keyword like `top`) places the box in one of the 9 regions around the anchor (the implied-anchor 3×3 grid), sizing/positioning the box into that region. Equivalent to the common `anchor()` inset patterns, computed from `position-anchor`'s rect. | `style`, `layout` | `position-area: top center` puts the box centered above the anchor; `bottom right` to its lower-right (tested + visual) | ✅ |
| **E71-M3** | **`anchor-size()` + `inset-area` alias + position-try fallback (MVP)**: `width/height: anchor-size([<name>] <dim>)` sizes the box to the anchor's width/height; `inset-area` parses as an alias of `position-area`; a basic `position-try-fallbacks`/`position-try` MVP that flips the box to the opposite side when it would overflow the containing block. | `style`, `layout` | `width: anchor-size(--a width)` matches the anchor's width; an overflowing `top` placement flips to `bottom` (tested + visual) | ☐ |

## Non-goals (deferred)

- `@position-try` named custom fallback rule sets and the full `position-try-order`
  cascade; only a simple opposite-side flip-on-overflow MVP.
- `anchor()` inside `calc()` / mixed with other lengths, and multiple
  `anchor()` per inset; MVP resolves a standalone `anchor()` value per side.
- Scroll-driven anchor repositioning, `anchor-scope`, and implicit anchors via
  the `anchor` attribute / popover invoker relationships.
- `position-area` spanning keywords (`span-all`, `span-left`) beyond the basic
  3×3 region keywords.
