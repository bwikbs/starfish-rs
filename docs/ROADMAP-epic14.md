# Roadmap — Epic 14: form controls

Real rendering for HTML form controls. Today the UA sheet only gives
`input/button/select/textarea` `display: inline-block`, so `<input>` is an empty
box (no value), `<select>` paints ALL its `<option>`s inline, and there is no
native widget appearance (no checkbox tick, no field border, no placeholder).
Epic 14 renders each control with a default appearance + its current value,
treating form controls as a kind of replaced/widget element (like `<img>`): the
layout computes a used size and the painter draws the native look from the
element's type + attributes (`value`, `checked`, `placeholder`, `rows`/`cols`,
`size`, the selected `<option>`).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Pages with no form controls
must stay byte-identical (existing tests + golden PNG unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E14-M1** | **Text controls**: `<input>` text-like types (`text`/`search`/`email`/`url`/`tel`/`password`/`number`/default), `<textarea>`, and `<button>` / `<input type=button\|submit\|reset>`. UA default styling (field border + padding + white background; button bevel/grey background); render the control's value (input's `value` attribute, textarea's text content, button's label) clipped to the box; intrinsic default sizing (`size`/`cols`/`rows` → a `ch`/line-based box); `placeholder` text (grey) when the value is empty; `password` masking. | `html`, `style`, `layout`, `paint` | A text `<input value=…>` shows its value in a bordered field, a `<textarea>` shows its content, a `<button>` shows its label, placeholder shows when empty (tested + visual) | ✅ |
| **E14-M2** | **Choice controls**: `<input type=checkbox>` / `<input type=radio>` drawn natively (a box / circle, with a tick / filled dot when the `checked` attribute is present); `<select>` rendered as a single-line control showing the selected `<option>`'s text plus a dropdown arrow (the `selected` option, or the first; its `<option>`s do NOT paint inline). | `html`, `style`, `layout`, `paint` | A checked checkbox/radio renders its tick/dot, an unchecked one renders empty; a `<select>` shows only the selected option's label + an arrow (tested + visual) | ☐ |
| **E14-M3** | **State + polish**: form-state pseudo-classes (`:checked`, `:disabled`, `:enabled`, `:required`, `:read-only`) so author CSS can target controls; `disabled` default greying; `type=hidden` → not rendered; `type=color` swatch; `type=range` (track + thumb at `value`); `<option>`/`<optgroup>` never render outside their `<select>`. | `css`, `style`, `layout`, `paint` | `input:checked`/`:disabled` match + style; a disabled control greys; hidden inputs don't render; color/range render a swatch/slider (tested + visual) | ☐ |

## Non-goals (deferred)

- Interactivity: focus, typing, clicking, form submission, the value changing
  (one-shot render — controls show their initial/attribute state only).
- `<select multiple>` / `<select size>` list-box rendering, `<optgroup>` labels,
  `<datalist>`, autocomplete dropdowns.
- `type=file` (file picker), `type=date`/`time`/`datetime-local`/`month`/`week`
  native date pickers (render as a plain text field), `type=image` button.
- Spin buttons on `type=number`, the `type=search` clear button, `type=range`
  tick marks / `<output>`.
- `::placeholder` / `::-webkit-*` pseudo-element styling, `accent-color`,
  `appearance` (the `appearance: none` reset), `field-sizing`.
- Real platform-native theming (a simple consistent built-in look, not OS chrome).
- Form validation UI (`:invalid`/`:valid` bubbles), `:focus-visible`.
