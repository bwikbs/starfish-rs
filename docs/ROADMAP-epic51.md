# Roadmap — Epic 51: form-control & UI styling

Adds the UI styling properties missing from the form-control set (E14/E39):
`accent-color` (tints checked checkboxes/radios, range, progress), `appearance`
(strip/keep UA control chrome), and `caret-color`/`pointer-events`/`all`.

Same per-milestone pipeline. Additive: pages not using these render
byte-identically (golden + existing tests unchanged). ComputedStyle at a stack
limit — keep new fields small.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E51-M1** | **`accent-color`**: tints the accented part of form controls — a checked checkbox's box/tick, a radio's dot, the range slider fill/thumb, the `<progress>` fill, the `<meter>` fill. `auto` = the UA default. INHERITED. | `style`, `layout`, `paint` | a checked `<input type=checkbox>` / `<progress>` with `accent-color:#c00` paints red instead of the default; auto/unset is byte-identical (tested + visual) | ✅ |
| **E51-M2** | **`appearance: none\|auto`**: `appearance:none` removes UA control chrome — a checkbox/radio/button/select renders as a plain box (no tick/dot/triangle, no UA border-fill), so author CSS fully styles it. `auto` keeps the UA look. | `style`, `layout` | `appearance:none` on a checkbox renders a plain styleable box (no UA tick chrome); auto keeps it (tested + visual) | ✅ |
| **E51-M3** | **`caret-color` + `pointer-events` + `all`**: parse `caret-color` (stored, no caret painted in a static render) and `pointer-events` (parse/store); the `all` property resets every longhand to `initial`/`inherit`/`unset`. | `css`, `style` | `all: initial` resets an element's inherited+set properties to initial; `caret-color`/`pointer-events` parse without error (tested) | ☐ |

## Non-goals (deferred)

- `accent-color` exact UA tinting math (lightened variants for hover/disabled);
  a single tint of the accent fill is enough.
- `appearance` values other than `none`/`auto` (`textfield`/`menulist-button`/…),
  and `appearance:none` re-enabling via `revert`.
- `caret-color` actual caret rendering (no editing caret in a one-shot render);
  `pointer-events` hit-testing (no interaction); `all` interaction with custom
  properties / `revert-layer`.
