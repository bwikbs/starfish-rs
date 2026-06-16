# Roadmap — Epic 39: forms round 2

Extends the form-control set (E14: text/checkbox/radio/select/button/color/range)
with gauges, grouping, and validity: `<progress>`/`<meter>` bars,
`<fieldset>`/`<legend>`/`<datalist>`, and the constraint-validation
pseudo-classes (`:valid`/`:invalid`/`:in-range`/`:out-of-range`/`:optional`).

Same per-milestone pipeline. Additive: pages without these render
byte-identically (golden + existing form tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E39-M1** | **`<progress>`/`<meter>`**: `FormControl::Progress{value,max}` / `Meter{value,min,max,...}`; render a UA bar (track + a filled portion proportional to value/max), `<progress>` with no value rendering an indeterminate full-ish track. | `layout`, `paint` | `<progress value=0.3>` renders a ~30%-filled bar; `<meter value=0.6>` a ~60% bar (tested + visual) | ☐ |
| **E39-M2** | **`<fieldset>`/`<legend>`/`<datalist>`**: UA `fieldset`/`legend` block with a fieldset border + padding; `<legend>` renders at the top; `<datalist>` is `display:none`. | `style` | a `<fieldset>` draws a bordered group with its `<legend>` text at the top; a `<datalist>` renders nothing (tested + visual) | ☐ |
| **E39-M3** | **Validity pseudo-classes**: `:valid`/`:invalid` (a control fails validity when `required` + empty, or `type=email`/`url` with a malformed value, or value outside `min`/`max`), `:in-range`/`:out-of-range` (numeric/range vs min/max), `:optional`. | `css`, `style` | `input:invalid` matches a `required` empty field and an out-of-range number; `:valid` matches a filled/in-range one; `:optional` matches a non-required field (tested) | ☐ |

## Non-goals (deferred)

- `<progress>`/`<meter>` low/high/optimum color zones (meter rendered as a single
  bar), and the `::-webkit-progress-*`/`::-moz-*` pseudo-elements.
- `<legend>` cut into the fieldset border (rendered above/inside, no border notch).
- `<datalist>` autocomplete UI; full HTML form submission and the
  Constraint Validation API methods (`checkValidity()` etc.).
- `:user-valid`/`:user-invalid` (interaction state), `pattern` regex for arbitrary
  patterns beyond a basic check, and `:valid` on `<form>`/`<fieldset>` aggregates.
