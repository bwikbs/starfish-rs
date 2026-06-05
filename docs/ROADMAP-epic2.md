# Roadmap — Epic 2: CSS coverage + images

Builds on the Epic-1 static pipeline (HTML→DOM→CSS→style→layout→paint→PNG). Same
agent pipeline per milestone (design → analysis → implementation → review →
verification), each landing as its own commit + push.

Target: render richer static pages — modern layout, out-of-flow boxes, images, and
visual effects — still no JavaScript, still no networking (local `src`/`<style>` only).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E2-M1** | `text-decoration` (underline/line-through/overline), list markers (`list-style-type` disc/circle/square/decimal/none + outside position), real `inline-block` (atomic inline sized by its own block layout) | `style`, `layout`, `paint` | Decorations paint; `<ul>/<ol>` show markers; an inline-block box sizes and sits in a line (tested + visual) | ✅ |
| **E2-M2** | `float`/`clear` (left/right, content flows around), `position` relative/absolute/fixed (offsets, containing block, simple paint order) | `style`, `layout`, `paint` | Floated box with wrapping text; absolutely-positioned box at offset from its containing block (tested) | ✅ |
| **E2-M3** | Flexbox: `display:flex`, `flex-direction`, `justify-content`, `align-items`, `flex-grow/shrink/basis`, `gap` | `style`, `layout` | A row/column flex container distributes children per the properties (tested) | ✅ |
| **E2-M4** | `<img>`: decode PNG/JPEG (image crate), replaced-element intrinsic size + `width`/`height`, paint the bitmap; `src` resolved relative to the input file (no network) | `dom`, `layout`, `paint` | `<img>` renders the decoded bitmap at the right box (tested + visual) | ✅ |
| **E2-M5** | Backgrounds & effects: `linear-gradient()` backgrounds, `border-radius`, `box-shadow`, `opacity` | `style`, `paint` | Gradient fill, rounded corners, drop shadow, and a faded box paint correctly (tested + visual) | ✅ |

**Epic 2 complete.** 296 workspace tests, clippy clean. Demos under `docs/examples/`.

## Non-goals (still deferred to later epics)

- JavaScript / events / DOM scripting.
- Networking / fetch / linked `<link rel=stylesheet>` / remote images.
- CSS grid, multicol, tables layout, writing-modes, transforms, transitions/animations,
  filters, blend modes, `background-image: url()` (only gradients in E2-M5), radial/conic
  gradients, `::before`/`::after` content, attribute/`:nth` selectors (unless a milestone
  needs them), italic/bidi text (text-quality epic).
