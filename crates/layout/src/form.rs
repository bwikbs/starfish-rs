//! Native text form controls (E14-M1): recognize `<input>` (text-like types),
//! `<textarea>`, `<button>` and resolve the text they display. They are laid out
//! as atomic replaced-style boxes (`BoxKind::FormControl`); `select`/`checkbox`/
//! `radio`/`hidden` and other non-text types are NOT recognized here (they keep
//! their default inline-block behaviour — M2 territory).

use starfish_dom::{Document, NodeId, NodeKind};

/// A recognized native text form control + which kind it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormControl {
    /// A text-like `<input>` (text/search/email/url/tel/number/password/…). The
    /// `password` flag masks the displayed value with bullets.
    TextInput { password: bool },
    /// A `<textarea>`.
    TextArea,
    /// A push button: `<button>` or `<input type=button|submit|reset>`.
    Button,
}

/// Recognize a native text form control + its kind, else `None`.
/// `select`/`checkbox`/`radio`/`hidden`/`file`/`image`/`color`/`range`/date-time
/// inputs → `None` (M2 / out of scope; they keep their inline-block rendering).
pub fn form_control_kind(doc: &Document, id: NodeId) -> Option<FormControl> {
    match doc.tag_name(id)? {
        "textarea" => Some(FormControl::TextArea),
        "button" => Some(FormControl::Button),
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
                return Some(FormControl::TextInput { password: true });
            }
            // Non-text input types render natively in M2 → not a text control.
            if ty.eq_ignore_ascii_case("checkbox")
                || ty.eq_ignore_ascii_case("radio")
                || ty.eq_ignore_ascii_case("hidden")
                || ty.eq_ignore_ascii_case("file")
                || ty.eq_ignore_ascii_case("image")
                || ty.eq_ignore_ascii_case("color")
                || ty.eq_ignore_ascii_case("range")
                || ty.eq_ignore_ascii_case("date")
                || ty.eq_ignore_ascii_case("time")
                || ty.eq_ignore_ascii_case("datetime-local")
                || ty.eq_ignore_ascii_case("month")
                || ty.eq_ignore_ascii_case("week")
            {
                return None;
            }
            // text/search/email/url/tel/number/(empty)/unknown → text input.
            Some(FormControl::TextInput { password: false })
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
            Some(FormControl::TextInput { password: false })
        );
        assert_eq!(
            kind_of("<input type=text>", "input"),
            Some(FormControl::TextInput { password: false })
        );
        assert_eq!(
            kind_of("<input type=email>", "input"),
            Some(FormControl::TextInput { password: false })
        );
        assert_eq!(
            kind_of("<input type=password>", "input"),
            Some(FormControl::TextInput { password: true })
        );
        assert_eq!(kind_of("<input type=button>", "input"), Some(FormControl::Button));
        assert_eq!(kind_of("<input type=submit>", "input"), Some(FormControl::Button));
        assert_eq!(kind_of("<input type=reset>", "input"), Some(FormControl::Button));
    }

    #[test]
    fn uppercase_type_is_case_insensitive() {
        assert_eq!(
            kind_of("<input type=TEXT>", "input"),
            Some(FormControl::TextInput { password: false })
        );
        assert_eq!(
            kind_of("<input type=PASSWORD>", "input"),
            Some(FormControl::TextInput { password: true })
        );
        assert_eq!(kind_of("<input type=Submit>", "input"), Some(FormControl::Button));
    }

    #[test]
    fn textarea_and_button_kinds() {
        assert_eq!(kind_of("<textarea></textarea>", "textarea"), Some(FormControl::TextArea));
        assert_eq!(kind_of("<button>Go</button>", "button"), Some(FormControl::Button));
    }

    #[test]
    fn non_text_controls_are_none() {
        assert_eq!(kind_of("<input type=checkbox>", "input"), None);
        assert_eq!(kind_of("<input type=radio>", "input"), None);
        assert_eq!(kind_of("<input type=hidden>", "input"), None);
        assert_eq!(kind_of("<input type=file>", "input"), None);
        assert_eq!(kind_of("<input type=color>", "input"), None);
        assert_eq!(kind_of("<input type=range>", "input"), None);
        assert_eq!(kind_of("<input type=date>", "input"), None);
        assert_eq!(kind_of("<select></select>", "select"), None);
        assert_eq!(kind_of("<div></div>", "div"), None);
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
        assert_eq!(input_display(&doc, id, true), ("\u{2022}\u{2022}\u{2022}".to_string(), false));
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
