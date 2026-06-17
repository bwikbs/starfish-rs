//! Native form controls: text controls (E14-M1) and choice controls (E14-M2).
//! E14-M1 recognizes `<input>` (text-like types), `<textarea>`, `<button>`; E14-M2
//! adds `<input type=checkbox|radio>` and `<select>`; E14-M3 adds
//! `<input type=color>` (swatch) and `<input type=range>` (slider). All are laid
//! out as atomic replaced-style boxes (`BoxKind::FormControl`);
//! `hidden`/`file`/`image`/date-time inputs are NOT recognized (they keep their
//! default inline-block behaviour; `hidden` is also `display:none` via the UA
//! sheet).

use starfish_dom::{Document, NodeId, NodeKind};

/// E58-M1: the chrome flavor of a text-like `<input>`. `Plain` is the default
/// text/password/email/tel/url field (no extra chrome). `Number`/`Search`/
/// `DateLike` add a UA indicator drawn by the painter on top of the same text
/// field (a spinner, a rounded field, a picker indicator respectively); the
/// displayed value still comes from `input_display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFlavor {
    /// text/password/email/tel/url — a plain text field, no extra chrome.
    Plain,
    /// `type=number` — text field + up/down spinner arrows at the right.
    Number,
    /// `type=search` — a rounded text field.
    Search,
    /// `type=date|time|month|week|datetime-local` — text field + a picker indicator.
    DateLike,
}

/// A recognized native form control + which kind it is.
// E39-M1: dropped `Eq` because Progress/Meter carry `f32` (no `Eq`); the enum is
// still `PartialEq` (all `assert_eq!`/`==` uses are partial-eq).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormControl {
    /// A text-like `<input>` (text/search/email/url/tel/number/password/…). The
    /// `password` flag masks the displayed value with bullets. E58-M1: `flavor`
    /// selects the UA chrome (plain / number spinner / search / date indicator).
    TextInput { password: bool, flavor: TextFlavor },
    /// A `<textarea>`.
    TextArea,
    /// A push button: `<button>` or `<input type=button|submit|reset>`.
    Button,
    /// `<input type=checkbox>` (E14-M2). `checked` reflects the attribute.
    Checkbox { checked: bool },
    /// `<input type=radio>` (E14-M2). `checked` reflects the attribute.
    Radio { checked: bool },
    /// `<select>` (E14-M2): a single-line control showing the selected option.
    Select,
    /// `<input type=color>` (E14-M3): a swatch filled with the `value` colour.
    Color,
    /// `<input type=range>` (E14-M3): a slider (track + thumb at value).
    Range,
    /// `<progress>` (E39-M1): a determinate bar fills `value/max` of the track.
    /// `value < 0` is the indeterminate sentinel (no/invalid `value` attribute);
    /// it draws an indeterminate fill (a centered partial bar).
    Progress { value: f32, max: f32 },
    /// `<meter>` (E39-M1): a gauge filled `(value-min)/(max-min)` of the track.
    Meter { value: f32, min: f32, max: f32 },
}

/// Recognize a native form control + its kind, else `None`.
/// `hidden`/`file`/`image`/date-time inputs → `None` (out of scope; they keep
/// their inline-block rendering).
pub fn form_control_kind(doc: &Document, id: NodeId) -> Option<FormControl> {
    match doc.tag_name(id)? {
        "textarea" => Some(FormControl::TextArea),
        "button" => Some(FormControl::Button),
        "select" => Some(FormControl::Select),
        // E39-M1: gauges. A finite `value` is determinate; a missing/invalid
        // `value` makes <progress> indeterminate (sentinel `-1.0`). <meter>
        // defaults value=0.
        "progress" => {
            let max = num_attr(doc, id, "max").filter(|&m| m > 0.0).unwrap_or(1.0);
            let value = num_attr(doc, id, "value").unwrap_or(-1.0);
            Some(FormControl::Progress { value, max })
        }
        "meter" => {
            let min = num_attr(doc, id, "min").unwrap_or(0.0);
            let max = num_attr(doc, id, "max").unwrap_or(1.0);
            let value = num_attr(doc, id, "value").unwrap_or(0.0);
            Some(FormControl::Meter { value, min, max })
        }
        "input" => {
            // `type` is case-preserved in the DOM → compare case-insensitively.
            let ty = doc.get_attribute(id, "type").unwrap_or("text");
            // button-like.
            if ty.eq_ignore_ascii_case("button")
                || ty.eq_ignore_ascii_case("submit")
                || ty.eq_ignore_ascii_case("reset")
            {
                return Some(FormControl::Button);
            }
            if ty.eq_ignore_ascii_case("password") {
                return Some(FormControl::TextInput {
                    password: true,
                    flavor: TextFlavor::Plain,
                });
            }
            // Choice controls render natively (E14-M2): self-drawn at 13×13.
            if ty.eq_ignore_ascii_case("checkbox") {
                return Some(FormControl::Checkbox {
                    checked: doc.get_attribute(id, "checked").is_some(),
                });
            }
            if ty.eq_ignore_ascii_case("radio") {
                return Some(FormControl::Radio {
                    checked: doc.get_attribute(id, "checked").is_some(),
                });
            }
            // E14-M3: native swatch / slider controls.
            if ty.eq_ignore_ascii_case("color") {
                return Some(FormControl::Color);
            }
            if ty.eq_ignore_ascii_case("range") {
                return Some(FormControl::Range);
            }
            // E58-M1: number/search/date-like are text fields with extra UA
            // chrome (spinner / rounded field / picker indicator).
            if ty.eq_ignore_ascii_case("number") {
                return Some(FormControl::TextInput {
                    password: false,
                    flavor: TextFlavor::Number,
                });
            }
            if ty.eq_ignore_ascii_case("search") {
                return Some(FormControl::TextInput {
                    password: false,
                    flavor: TextFlavor::Search,
                });
            }
            if ty.eq_ignore_ascii_case("date")
                || ty.eq_ignore_ascii_case("time")
                || ty.eq_ignore_ascii_case("datetime-local")
                || ty.eq_ignore_ascii_case("month")
                || ty.eq_ignore_ascii_case("week")
            {
                return Some(FormControl::TextInput {
                    password: false,
                    flavor: TextFlavor::DateLike,
                });
            }
            // Other non-text input types are out of scope → not a control.
            // (`hidden` is removed by the UA `display:none` rule, so it never
            // produces a FormControl box.)
            if ty.eq_ignore_ascii_case("hidden")
                || ty.eq_ignore_ascii_case("file")
                || ty.eq_ignore_ascii_case("image")
            {
                return None;
            }
            // text/email/url/tel/(empty)/unknown → plain text input.
            Some(FormControl::TextInput {
                password: false,
                flavor: TextFlavor::Plain,
            })
        }
        _ => None,
    }
}

/// The text + whether it is placeholder text (grey) for a text `<input>`.
/// A non-empty `value` shows the value (masked with `•` for `password`); else the
/// `placeholder` (or empty) is shown grey.
pub fn input_display(doc: &Document, id: NodeId, password: bool) -> (String, bool) {
    let value = doc.get_attribute(id, "value").unwrap_or("");
    if !value.is_empty() {
        if password {
            ("\u{2022}".repeat(value.chars().count()), false)
        } else {
            (value.to_string(), false)
        }
    } else {
        let placeholder = doc.get_attribute(id, "placeholder").unwrap_or("");
        (placeholder.to_string(), true)
    }
}

/// The displayed value of a `<textarea>`: its plain-text descendant content.
/// (M1: the tokenizer has no RCDATA state, so a `<` inside the textarea parses as
/// a tag; `collect_text` recovers tag-free text only — real RCDATA is out of
/// scope.)
pub fn textarea_value(doc: &Document, id: NodeId) -> String {
    let mut s = String::new();
    collect_text(doc, id, &mut s);
    s
}

/// The label shown on a button. `<button>` → its text content; `<input>` → its
/// `value` if non-empty, else the default `"Submit"`/`"Reset"` for those types,
/// else empty.
pub fn control_label(doc: &Document, id: NodeId) -> String {
    if doc.tag_name(id) == Some("button") {
        let mut s = String::new();
        collect_text(doc, id, &mut s);
        return s;
    }
    // <input type=button|submit|reset>
    let value = doc.get_attribute(id, "value").unwrap_or("");
    if !value.is_empty() {
        return value.to_string();
    }
    let ty = doc.get_attribute(id, "type").unwrap_or("text");
    if ty.eq_ignore_ascii_case("submit") {
        "Submit".to_string()
    } else if ty.eq_ignore_ascii_case("reset") {
        "Reset".to_string()
    } else {
        String::new()
    }
}

/// Concatenate the plain text of `id`'s descendants (DOM order). `Text` nodes
/// push their content; element children recurse.
fn collect_text(doc: &Document, id: NodeId, out: &mut String) {
    for child in doc.children(id) {
        match doc.kind(child) {
            NodeKind::Text(t) => out.push_str(t),
            NodeKind::Element(_) => collect_text(doc, child, out),
            _ => {}
        }
    }
}

/// The text shown in the closed `<select>`'s line: the selected `<option>`'s
/// label (E14-M2). The chosen option is the first with a `selected` attribute,
/// else the first option (HTML's default-selected rule). Its label is the
/// `label` attribute if present, else its trimmed text content. Empty `<select>`
/// (no options) → `""`.
pub fn selected_option_text(doc: &Document, select_id: NodeId) -> String {
    let mut options = Vec::new();
    collect_options(doc, select_id, &mut options);
    let chosen = options
        .iter()
        .find(|&&id| doc.get_attribute(id, "selected").is_some())
        .or_else(|| options.first());
    match chosen {
        None => String::new(),
        Some(&id) => option_label(doc, id),
    }
}

/// E72-M2: a single visible row of a listbox `<select>`, in document order.
/// A `Group` is an `<optgroup>`'s `label` (a bold, non-selectable header row);
/// an `Option` is a selectable `<option>` row, `indented` when it lives inside
/// an `<optgroup>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectRow {
    /// An `<optgroup label=...>` header row (the label text).
    Group(String),
    /// An `<option>` row: its node, whether it is `selected`, and whether it is
    /// indented (nested under an `<optgroup>`).
    Option {
        id: NodeId,
        selected: bool,
        indented: bool,
    },
}

/// E72-M2: the listbox ROWS of `select_id` in document order, distinguishing
/// group-label rows from option rows. A direct `<option>` child → an
/// `Option { indented: false }`; an `<optgroup>` → a `Group(label)` row followed
/// by its child `<option>`s as `Option { indented: true }`. All option/optgroup
/// elements are included (the UA `display:none` on options is overridden in a
/// listbox conceptually).
pub fn select_listbox_rows_list(doc: &Document, select_id: NodeId) -> Vec<SelectRow> {
    let mut out = Vec::new();
    for child in doc.children(select_id) {
        match doc.tag_name(child) {
            Some("option") => out.push(SelectRow::Option {
                id: child,
                selected: option_is_selected(doc, child),
                indented: false,
            }),
            Some("optgroup") => {
                let label = doc.get_attribute(child, "label").unwrap_or("").to_string();
                out.push(SelectRow::Group(label));
                for opt in doc.children(child) {
                    if doc.tag_name(opt) == Some("option") {
                        out.push(SelectRow::Option {
                            id: opt,
                            selected: option_is_selected(doc, opt),
                            indented: true,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// E72-M1: the row count when a `<select>` renders as a multi-row LISTBOX,
/// else `None` (a single-line closed dropdown). A select is a listbox when its
/// `size` attribute is an integer > 1, OR it has the boolean `multiple`
/// attribute. Rows = the `size` value if present (and ≥ 1), else (for
/// `multiple` with no/invalid size) the structured row count (options + group
/// labels, E72-M2) clamped to `[1, 20]`.
pub fn select_listbox_rows(doc: &Document, id: NodeId) -> Option<u32> {
    if doc.tag_name(id) != Some("select") {
        return None;
    }
    let size = doc
        .get_attribute(id, "size")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1);
    let multiple = doc.get_attribute(id, "multiple").is_some();
    match size {
        Some(n) if n > 1 => Some(n),
        // size=1 (or absent) with multiple → list every row, options + group
        // labels (E72-M2), clamped.
        _ if multiple => {
            let n = select_listbox_rows_list(doc, id).len() as u32;
            Some(n.clamp(1, 20))
        }
        _ => None,
    }
}

/// E72-M1: collect the `<option>` descendants of `select_id` (DOM order),
/// public for the painter's listbox rows. Recurses through `<optgroup>`.
pub fn collect_select_options(doc: &Document, select_id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    collect_options(doc, select_id, &mut out);
    out
}

/// E72-M1: an `<option>`'s displayed label (public for the painter).
pub fn select_option_label(doc: &Document, id: NodeId) -> String {
    option_label(doc, id)
}

/// E72-M1: whether an `<option>` is selected (has the `selected` attribute).
pub fn option_is_selected(doc: &Document, id: NodeId) -> bool {
    doc.get_attribute(id, "selected").is_some()
}

/// Depth-first collect the `<option>` descendants of `select_id` in DOM order,
/// recursing through `<optgroup>` (which groups options).
fn collect_options(doc: &Document, id: NodeId, out: &mut Vec<NodeId>) {
    for child in doc.children(id) {
        match doc.tag_name(child) {
            Some("option") => out.push(child),
            Some("optgroup") => collect_options(doc, child, out),
            _ => {}
        }
    }
}

/// An `<option>`'s displayed label: its `label` attribute if present, else its
/// trimmed text content.
fn option_label(doc: &Document, id: NodeId) -> String {
    if let Some(label) = doc.get_attribute(id, "label") {
        return label.to_string();
    }
    let mut s = String::new();
    collect_text(doc, id, &mut s);
    s.trim().to_string()
}

/// The `(value, min, max)` of an `<input type=range>` (E14-M3). `min` defaults to
/// 0, `max` to 100, `value` to the midpoint `(min+max)/2`; `value` is clamped to
/// the `[min(min,max), max(min,max)]` range so a reversed/out-of-range attribute
/// stays drawable.
pub fn range_values(doc: &Document, id: NodeId) -> (f32, f32, f32) {
    // Finite only: a `NaN`/`inf` attribute falls back to the default so the thumb
    // position can never become non-finite (which would render undefined geometry).
    let num = |name: &str| {
        doc.get_attribute(id, name)
            .and_then(|s| s.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
    };
    let min = num("min").unwrap_or(0.0);
    let max = num("max").unwrap_or(100.0);
    let value = num("value").unwrap_or((min + max) / 2.0);
    let (lo, hi) = (min.min(max), min.max(max));
    (value.clamp(lo, hi), min, max)
}

/// The 0..1 fraction of `value` between `min` and `max` (E14-M3). `0` when the
/// range is empty/reversed (`max <= min`).
pub fn range_fraction(value: f32, min: f32, max: f32) -> f32 {
    if max <= min {
        0.0
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }
}

// E39-M1: a finite numeric attribute (else `None`); shared by progress/meter.
fn num_attr(doc: &Document, id: NodeId, name: &str) -> Option<f32> {
    doc.get_attribute(id, name)
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_html::parse;

    fn find(doc: &Document, tag: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some(tag) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no <{tag}>");
    }

    fn kind_of(html: &str, tag: &str) -> Option<FormControl> {
        let doc = parse(html);
        form_control_kind(&doc, find(&doc, tag))
    }

    #[test]
    fn input_types_map_to_kinds() {
        assert_eq!(
            kind_of("<input>", "input"),
            Some(FormControl::TextInput {
                password: false,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=text>", "input"),
            Some(FormControl::TextInput {
                password: false,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=email>", "input"),
            Some(FormControl::TextInput {
                password: false,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=password>", "input"),
            Some(FormControl::TextInput {
                password: true,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=button>", "input"),
            Some(FormControl::Button)
        );
        assert_eq!(
            kind_of("<input type=submit>", "input"),
            Some(FormControl::Button)
        );
        assert_eq!(
            kind_of("<input type=reset>", "input"),
            Some(FormControl::Button)
        );
    }

    #[test]
    fn uppercase_type_is_case_insensitive() {
        assert_eq!(
            kind_of("<input type=TEXT>", "input"),
            Some(FormControl::TextInput {
                password: false,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=PASSWORD>", "input"),
            Some(FormControl::TextInput {
                password: true,
                flavor: TextFlavor::Plain
            })
        );
        assert_eq!(
            kind_of("<input type=Submit>", "input"),
            Some(FormControl::Button)
        );
    }

    #[test]
    fn textarea_and_button_kinds() {
        assert_eq!(
            kind_of("<textarea></textarea>", "textarea"),
            Some(FormControl::TextArea)
        );
        assert_eq!(
            kind_of("<button>Go</button>", "button"),
            Some(FormControl::Button)
        );
    }

    #[test]
    fn unsupported_input_types_are_none() {
        assert_eq!(kind_of("<input type=hidden>", "input"), None);
        assert_eq!(kind_of("<input type=file>", "input"), None);
        assert_eq!(kind_of("<input type=image>", "input"), None);
        assert_eq!(kind_of("<div></div>", "div"), None);
    }

    // E58-M1: number/search/date-like map to text-input flavors with chrome.
    #[test]
    fn input_type_flavors_map_to_kinds() {
        let flavor = |html: &str| match kind_of(html, "input") {
            Some(FormControl::TextInput { password: false, flavor }) => Some(flavor),
            _ => None,
        };
        assert_eq!(flavor("<input type=number value=5>"), Some(TextFlavor::Number));
        assert_eq!(flavor("<input type=NUMBER>"), Some(TextFlavor::Number));
        assert_eq!(flavor("<input type=search>"), Some(TextFlavor::Search));
        assert_eq!(flavor("<input type=date value='2026-01-01'>"), Some(TextFlavor::DateLike));
        assert_eq!(flavor("<input type=time>"), Some(TextFlavor::DateLike));
        assert_eq!(flavor("<input type=month>"), Some(TextFlavor::DateLike));
        assert_eq!(flavor("<input type=week>"), Some(TextFlavor::DateLike));
        assert_eq!(flavor("<input type=datetime-local>"), Some(TextFlavor::DateLike));
        // email/tel/url stay plain (no extra chrome).
        assert_eq!(flavor("<input type=email>"), Some(TextFlavor::Plain));
        assert_eq!(flavor("<input type=tel>"), Some(TextFlavor::Plain));
        assert_eq!(flavor("<input type=url>"), Some(TextFlavor::Plain));
    }

    #[test]
    fn color_and_range_map_to_kinds() {
        // E14-M3: color/range are now recognized native controls.
        assert_eq!(
            kind_of("<input type=color>", "input"),
            Some(FormControl::Color)
        );
        assert_eq!(
            kind_of("<input type=range>", "input"),
            Some(FormControl::Range)
        );
        assert_eq!(
            kind_of("<input type=COLOR>", "input"),
            Some(FormControl::Color)
        );
    }

    #[test]
    fn range_values_and_fraction() {
        let vals = |html: &str| {
            let doc = parse(html);
            range_values(&doc, find(&doc, "input"))
        };
        // Defaults: min 0, max 100, value midpoint 50.
        assert_eq!(vals("<input type=range>"), (50.0, 0.0, 100.0));
        // Explicit value within range.
        assert_eq!(
            vals("<input type=range min=0 max=10 value=3>"),
            (3.0, 0.0, 10.0)
        );
        // Value clamped to [min,max].
        assert_eq!(
            vals("<input type=range min=0 max=10 value=99>"),
            (10.0, 0.0, 10.0)
        );
        // Reversed min/max: value clamped to the ordered span.
        assert_eq!(
            vals("<input type=range min=10 max=0 value=5>"),
            (5.0, 10.0, 0.0)
        );

        // range_fraction: linear, clamped, and 0 for empty/reversed spans.
        assert_eq!(range_fraction(0.0, 0.0, 100.0), 0.0);
        assert_eq!(range_fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(range_fraction(100.0, 0.0, 100.0), 1.0);
        assert_eq!(range_fraction(5.0, 10.0, 0.0), 0.0);
        assert_eq!(range_fraction(5.0, 5.0, 5.0), 0.0);
    }

    #[test]
    fn progress_and_meter_map_to_kinds() {
        // E39-M1: <progress value=0.3 max=1> → Progress{0.3,1.0}.
        assert_eq!(
            kind_of("<progress value=0.3 max=1>", "progress"),
            Some(FormControl::Progress {
                value: 0.3,
                max: 1.0
            })
        );
        // No value → indeterminate sentinel (-1.0); default max 1.0.
        assert_eq!(
            kind_of("<progress>", "progress"),
            Some(FormControl::Progress {
                value: -1.0,
                max: 1.0
            })
        );
        // <meter value=0.6> → Meter{0.6, min 0, max 1}.
        assert_eq!(
            kind_of("<meter value=0.6>", "meter"),
            Some(FormControl::Meter {
                value: 0.6,
                min: 0.0,
                max: 1.0
            })
        );
        // <meter> explicit min/max.
        assert_eq!(
            kind_of("<meter min=0 max=10 value=5>", "meter"),
            Some(FormControl::Meter {
                value: 5.0,
                min: 0.0,
                max: 10.0
            })
        );
    }

    #[test]
    fn choice_controls_map_to_kinds() {
        // E14-M2: checkbox/radio/select are recognized.
        assert_eq!(
            kind_of("<input type=checkbox>", "input"),
            Some(FormControl::Checkbox { checked: false })
        );
        assert_eq!(
            kind_of("<input type=checkbox checked>", "input"),
            Some(FormControl::Checkbox { checked: true })
        );
        assert_eq!(
            kind_of("<input type=radio>", "input"),
            Some(FormControl::Radio { checked: false })
        );
        assert_eq!(
            kind_of("<input type=radio checked>", "input"),
            Some(FormControl::Radio { checked: true })
        );
        assert_eq!(
            kind_of("<input type=CHECKBOX>", "input"),
            Some(FormControl::Checkbox { checked: false })
        );
        assert_eq!(
            kind_of("<select></select>", "select"),
            Some(FormControl::Select)
        );
    }

    #[test]
    fn selected_option_text_rules() {
        let pick = |html: &str| {
            let doc = parse(html);
            selected_option_text(&doc, find(&doc, "select"))
        };
        // No selected attr → first option.
        assert_eq!(pick("<select><option>A<option>B</select>"), "A");
        // Explicit selected wins over order.
        assert_eq!(pick("<select><option>A<option selected>B</select>"), "B");
        // label attr overrides text content.
        assert_eq!(pick("<select><option label='Lbl'>text</select>"), "Lbl");
        // Text is trimmed.
        assert_eq!(
            pick("<select><option>  spaced  </option></select>"),
            "spaced"
        );
        // optgroup is walked through.
        assert_eq!(
            pick("<select><optgroup label='g'><option selected>Inner</option></optgroup></select>"),
            "Inner"
        );
        // Empty select → "".
        assert_eq!(pick("<select></select>"), "");
    }

    #[test]
    fn select_listbox_rows_rule() {
        let rows = |html: &str| {
            let doc = parse(html);
            select_listbox_rows(&doc, find(&doc, "select"))
        };
        // size > 1 → listbox of that many rows.
        assert_eq!(rows("<select size=4></select>"), Some(4));
        // multiple (no size) → option count clamped.
        assert_eq!(
            rows("<select multiple><option>A<option>B<option>C</select>"),
            Some(3)
        );
        // multiple with no options → clamped to >=1.
        assert_eq!(rows("<select multiple></select>"), Some(1));
        // plain / size=1 → single-line dropdown (not a listbox).
        assert_eq!(rows("<select></select>"), None);
        assert_eq!(rows("<select size=1></select>"), None);
        // multiple + explicit size wins.
        assert_eq!(
            rows("<select multiple size=2><option>A<option>B<option>C</select>"),
            Some(2)
        );
    }

    #[test]
    fn select_listbox_rows_list_groups_and_indents() {
        // E72-M2: optgroups produce Group rows; their options are indented.
        let doc = parse(
            "<select size=6>\
             <optgroup label=A><option>1<option>2></optgroup>\
             <optgroup label=B><option>3></optgroup></select>",
        );
        let rows = select_listbox_rows_list(&doc, find(&doc, "select"));
        // Shape: Group(A), Option(indented), Option(indented), Group(B), Option(indented).
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], SelectRow::Group("A".to_string()));
        assert!(matches!(rows[1], SelectRow::Option { indented: true, selected: false, .. }));
        assert!(matches!(rows[2], SelectRow::Option { indented: true, .. }));
        assert_eq!(rows[3], SelectRow::Group("B".to_string()));
        assert!(matches!(rows[4], SelectRow::Option { indented: true, .. }));

        // A select with only direct options → all Option{indented:false}, no Group.
        let doc = parse("<select size=3><option selected>X<option>Y</select>");
        let rows = select_listbox_rows_list(&doc, find(&doc, "select"));
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], SelectRow::Option { indented: false, selected: true, .. }));
        assert!(matches!(rows[1], SelectRow::Option { indented: false, selected: false, .. }));
    }

    #[test]
    fn input_display_value_placeholder_password() {
        let doc = parse("<input value='hi'>");
        let id = find(&doc, "input");
        assert_eq!(input_display(&doc, id, false), ("hi".to_string(), false));

        let doc = parse("<input placeholder='name'>");
        let id = find(&doc, "input");
        assert_eq!(input_display(&doc, id, false), ("name".to_string(), true));

        let doc = parse("<input type=password value='abc'>");
        let id = find(&doc, "input");
        assert_eq!(
            input_display(&doc, id, true),
            ("\u{2022}\u{2022}\u{2022}".to_string(), false)
        );
    }

    #[test]
    fn textarea_value_reads_plain_text() {
        let doc = parse("<textarea>hello world</textarea>");
        let id = find(&doc, "textarea");
        assert_eq!(textarea_value(&doc, id), "hello world");
    }

    #[test]
    fn control_label_defaults() {
        let doc = parse("<button>Go</button>");
        assert_eq!(control_label(&doc, find(&doc, "button")), "Go");

        let doc = parse("<input type=submit>");
        assert_eq!(control_label(&doc, find(&doc, "input")), "Submit");

        let doc = parse("<input type=reset>");
        assert_eq!(control_label(&doc, find(&doc, "input")), "Reset");

        let doc = parse("<input type=submit value='Send'>");
        assert_eq!(control_label(&doc, find(&doc, "input")), "Send");
    }
}
