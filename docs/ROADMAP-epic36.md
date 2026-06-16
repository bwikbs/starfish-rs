# Roadmap — Epic 36: interactive HTML elements

Renders the interactive disclosure/dialog/popover elements at their one-shot
state: `<details>`/`<summary>` (open vs collapsed), `<dialog>` (+ the global
`hidden` attribute), and the Popover API (`popover` attribute + `:popover-open`
+ JS `showPopover`/`hidePopover`/`togglePopover`). The engine is a one-shot
renderer (scripts run to quiescence, then a snapshot), so "interactive" state is
whatever the `open`/attribute/JS-call leaves in the DOM at snapshot time.

Same per-milestone pipeline. Additive: pages not using these elements render
byte-identically (golden + existing tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E36-M1** | **`<details>`/`<summary>`**: UA `details`/`summary` are block; a disclosure triangle marker on the summary; a `<details>` renders its `<summary>` (or a synthesized default "Details" when absent) always, and its remaining children ONLY when the `open` attribute is present. | `style`, `layout` | a closed `<details>` shows just the summary + ▸ marker; an `open` one shows the summary + content + ▾ marker; no `<details>` is byte-identical (tested + visual) | ✅ |
| **E36-M2** | **`<dialog>` + `hidden`**: UA rules so a `<dialog>` without `open` and any element with the `hidden` attribute are `display:none`; an `open` dialog renders as a centered, bordered, padded block (UA dialog styling). | `style` | `<dialog open>` renders as a bordered centered box; a plain `<dialog>` and any `[hidden]` element render nothing; a page with neither is byte-identical (tested + visual) | ✅ |
| **E36-M3** | **Popover API**: `popover` attribute (`[popover]` hidden until open), a `:popover-open` pseudo-class, and JS `element.showPopover()`/`hidePopover()`/`togglePopover()` toggling an internal open flag reflected by `:popover-open`. | `css`, `style`, `js` | `el.showPopover()` in a script makes a `[popover]` element render (matched by `:popover-open`); without the call it is hidden (tested + visual) | ☐ |

## Non-goals (deferred)

- `<details name>` exclusive-accordion grouping; the `open` toggle on click
  (no input events in a one-shot renderer — only the parsed/JS state renders).
- `<dialog>` top-layer / `showModal()` modal semantics beyond a centered box,
  `::backdrop`, `inert`, and focus trapping.
- `popovertarget`/`popovertargetaction` button activation (no clicks);
  popover light-dismiss, `:popover-open` auto vs manual popover nuances, and the
  top-layer stacking for popovers.
