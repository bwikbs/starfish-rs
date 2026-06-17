# Roadmap — Epic 58: forms round 3 (input types / FormData / validation API)

Rounds out forms (E14 controls, E39 progress/meter + validity pseudos): more
`<input type>` rendering, the `FormData` API + `form.elements`, and the
Constraint Validation API methods.

Same per-milestone pipeline. Additive: existing controls render byte-identically;
the new JS APIs are inert until called.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E58-M1** | **input-type chrome**: `type=number` renders an up/down spinner; `type=search` a rounded field (+ a clear-affordance hint); `type=date`/`time`/`month`/`week`/`datetime-local` render a text-like field showing the value + a small picker indicator; `email`/`tel`/`url` render as text. Distinct `FormControl` kinds where useful. | `layout`, `paint` | a `<input type=number value=3>` paints with up/down spinner arrows; a `type=date` shows the value + an indicator; default text inputs unchanged (tested + visual) | ✅ |
| **E58-M2** | **`FormData` + `form.elements`**: `new FormData(form)` collects the form's named, non-disabled successful controls (name→value, multiple for checkboxes/select-multiple); `append`/`get`/`getAll`/`has`/`delete`/`set`/`entries`/`keys`/`values`. `form.elements` lists the form's controls. | `js` | `new FormData(f).get('q')` returns the named input's value; `f.elements.length` counts the controls (tested) | ☐ |
| **E58-M3** | **Constraint Validation API**: `el.validity` (a ValidityState with `valueMissing`/`typeMismatch`/`rangeUnderflow`/`rangeOverflow`/`patternMismatch`/`valid`), `checkValidity()`/`reportValidity()` (bool), `setCustomValidity(msg)` + `validationMessage`, and `willValidate`. | `js` | a `required` empty input has `validity.valueMissing===true` and `checkValidity()===false`; `setCustomValidity('x')` makes it invalid with that message (tested) | ☐ |

## Non-goals (deferred)

- Native date/time PICKERS (calendar popups) and number-input step buttons doing
  actual increment (no interaction); spinner/indicator are visual chrome only.
- `FormData` file entries / `Blob` values and `multipart/form-data` encoding;
  actual form SUBMISSION/navigation (one-shot renderer).
- `:user-valid`/`:user-invalid`, `reportValidity` UI bubbles, and
  `ValidityState` flags beyond the listed set (`stepMismatch`/`tooLong`/`tooShort`/
  `badInput` parsed-or-false MVP).
