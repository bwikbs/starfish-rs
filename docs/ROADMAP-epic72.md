# Roadmap — Epic 72: form controls round 4 (listbox `<select>`, optgroup, file input)

`<select>` always renders as a single-line dropdown field (selected option + arrow);
`<select size=N>` / `<select multiple>` do NOT render as a multi-row listbox,
`<optgroup>` labels aren't shown, and `<input type=file>` is unrecognized (no box).
This round adds the listbox presentation, optgroup labels, and the file-input
button.

Form controls are atomic `BoxKind::FormControl` boxes painted by
`emit_form_control`/`emit_select` (`crates/paint/src/display.rs`); their kind comes
from `form_control_kind` (`crates/layout/src/form.rs`). Listbox needs the select
box sized to N rows (layout) + per-row painting (paint).

Same per-milestone pipeline. Additive: a plain single `<select>` and existing
controls render byte-identically (golden + E14/E39/E51 form tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E72-M1** | **listbox `<select size=N>` / `<select multiple>`**: a select with `size>1` or `multiple` renders as a bordered listbox of `size` (or option-count) rows; each `<option>` is painted as a row, the selected option(s) highlighted (blue bg / white text). The control box height = rows × row-height. | `layout`, `paint` | `<select size=4>` with 4 options paints a 4-row list with the selected row highlighted (tested + visual) | ✅ |
| **E72-M2** | **`<optgroup>` labels in the listbox**: `<optgroup label="…">` paints a bold, non-selectable group-label row; its `<option>`s are indented under it. The row count + height account for the label rows. | `layout`, `paint` | a listbox with two `<optgroup>`s shows bold labels with indented options (tested + visual) | ☐ |
| **E72-M3** | **`<input type=file>`**: recognized as a form control; renders a UA "Choose File" push button followed by a "No file chosen" (or the `value`/filename) label. | `layout`, `paint` | `<input type=file>` paints a button + filename label, sized like a button (tested + visual) | ☐ |

## Non-goals (deferred)

- Real option selection interaction / scrolling within an overflowing listbox
  (paints the first `size` rows; long lists clip — documented).
- Multi-select highlighting of more than the `selected` attribute's options
  beyond painting each `selected` option highlighted.
- `<input type=file multiple>` chip UI and actual file picking; the button is
  static chrome.
- Native OS dropdown popup for the closed single `<select>` (unchanged).
