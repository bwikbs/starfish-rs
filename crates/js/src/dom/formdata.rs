//! E58-M2 `FormData` — a JS-constructible native class over an ordered list of
//! `(name, value)` entries.
//!
//! `new FormData(form?)` walks the optional `<form>`'s descendant controls and
//! collects the "successful" named, non-disabled ones (per the HTML form data
//! set). Methods mirror `URLSearchParams`: `append`/`set`/`get`/`getAll`/`has`/
//! `delete` + `entries`/`keys`/`values` (returning arrays — the iterator
//! protocol is deferred). File entries / `Blob` values are out of scope.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace};
use starfish_dom::{Document, NodeId, NodeKind};

use super::node::arg_str;
use super::{method, NodeHandle};

/// Ordered `(name, value)` form-data entries. Holds no GC pointers.
#[derive(Trace, Finalize, JsData, Default)]
pub(crate) struct FormData {
    #[unsafe_ignore_trace]
    entries: Vec<(String, String)>,
}

impl Class for FormData {
    const NAME: &'static str = "FormData";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        // Optional `<form>` arg: when present (and a node), collect its data set.
        let entries = match args.first().and_then(|v| v.as_object()) {
            Some(o) => match o.downcast_ref::<NodeHandle>() {
                Some(h) => collect_form_entries(&h.shared.borrow(), h.id),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        Ok(FormData { entries })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "get", 1, fd_get);
        method(class, "getAll", 1, fd_get_all);
        method(class, "has", 1, fd_has);
        method(class, "set", 2, fd_set);
        method(class, "append", 2, fd_append);
        method(class, "delete", 1, fd_delete);
        method(class, "entries", 0, fd_entries);
        method(class, "keys", 0, fd_keys);
        method(class, "values", 0, fd_values);
        Ok(())
    }
}

/// Collect the form's successful named controls (E58-M2). Pre-order DFS over the
/// form's descendants; each `<input>`/`<select>`/`<textarea>` with a non-empty
/// `name` and no `disabled` attribute contributes one (or zero) entry.
fn collect_form_entries(doc: &Document, form: NodeId) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![form];
    let mut order = Vec::new();
    // DFS, recording document order (skip the form node itself).
    while let Some(id) = stack.pop() {
        if id != form {
            order.push(id);
        }
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    for id in order {
        let name = match doc.get_attribute(id, "name") {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        if doc.get_attribute(id, "disabled").is_some() {
            continue;
        }
        if let Some(value) = successful_value(doc, id) {
            out.push((name, value));
        }
    }
    out
}

/// The control's submitted value, or `None` when it is not successful (an
/// unchecked checkbox/radio, or a non-control element).
fn successful_value(doc: &Document, id: NodeId) -> Option<String> {
    match doc.tag_name(id) {
        Some("input") => {
            let kind = doc
                .get_attribute(id, "type")
                .unwrap_or("text")
                .to_ascii_lowercase();
            match kind.as_str() {
                "checkbox" | "radio" => {
                    if doc.get_attribute(id, "checked").is_some() {
                        Some(doc.get_attribute(id, "value").unwrap_or("on").to_string())
                    } else {
                        None
                    }
                }
                // Non-submitting input kinds are not part of the data set.
                "submit" | "reset" | "button" | "image" | "file" => None,
                _ => Some(doc.get_attribute(id, "value").unwrap_or("").to_string()),
            }
        }
        Some("textarea") => Some(textarea_value(doc, id)),
        Some("select") => Some(select_value(doc, id)),
        _ => None,
    }
}

/// A `<textarea>`'s value: the `value` attribute if present, else its text.
fn textarea_value(doc: &Document, id: NodeId) -> String {
    if let Some(v) = doc.get_attribute(id, "value") {
        return v.to_string();
    }
    let mut s = String::new();
    for c in doc.children(id) {
        if let NodeKind::Text(t) = doc.kind(c) {
            s.push_str(t);
        }
    }
    s
}

/// A `<select>`'s value: the selected `<option>` (one with a `selected`
/// attribute, else the first option) value attribute, defaulting to its text.
fn select_value(doc: &Document, id: NodeId) -> String {
    let options: Vec<NodeId> = {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = doc.children(id).into_iter().rev().collect();
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some("option") {
                out.push(n);
            }
            for c in doc.children(n).into_iter().rev() {
                stack.push(c);
            }
        }
        out
    };
    let chosen = options
        .iter()
        .find(|&&o| doc.get_attribute(o, "selected").is_some())
        .or_else(|| options.first());
    match chosen {
        Some(&o) => match doc.get_attribute(o, "value") {
            Some(v) => v.to_string(),
            None => option_text(doc, o),
        },
        None => String::new(),
    }
}

/// An `<option>`'s text content (its descendant Text payloads).
fn option_text(doc: &Document, id: NodeId) -> String {
    let mut s = String::new();
    let mut stack: Vec<NodeId> = doc.children(id).into_iter().rev().collect();
    while let Some(n) = stack.pop() {
        if let NodeKind::Text(t) = doc.kind(n) {
            s.push_str(t);
        }
        for c in doc.children(n).into_iter().rev() {
            stack.push(c);
        }
    }
    s.trim().to_string()
}

fn with_fd<R>(this: &JsValue, f: impl FnOnce(&FormData) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    let data = obj
        .downcast_ref::<FormData>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    Ok(f(&data))
}

fn with_fd_mut<R>(this: &JsValue, f: impl FnOnce(&mut FormData) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    let mut data = obj
        .downcast_mut::<FormData>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    Ok(f(&mut data))
}

fn fd_get(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_fd(this, |d| {
        match d.entries.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => JsString::from(v.as_str()).into(),
            None => JsValue::null(),
        }
    })
}

fn fd_get_all(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let values: Vec<JsValue> = with_fd(this, |d| {
        d.entries
            .iter()
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| JsString::from(v.as_str()).into())
            .collect()
    })?;
    Ok(JsArray::from_iter(values, ctx).into())
}

fn fd_has(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_fd(this, |d| {
        JsValue::from(d.entries.iter().any(|(k, _)| *k == name))
    })
}

fn fd_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let value = arg_str(args, 1, ctx)?;
    with_fd_mut(this, |d| {
        d.entries.retain(|(k, _)| *k != name);
        d.entries.push((name, value));
    })?;
    Ok(JsValue::undefined())
}

fn fd_append(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let value = arg_str(args, 1, ctx)?;
    with_fd_mut(this, |d| d.entries.push((name, value)))?;
    Ok(JsValue::undefined())
}

fn fd_delete(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_fd_mut(this, |d| d.entries.retain(|(k, _)| *k != name))?;
    Ok(JsValue::undefined())
}

/// `entries()` → an array of `[name, value]` pairs (MVP; not a live iterator).
fn fd_entries(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let pairs = with_fd(this, |d| d.entries.clone())?;
    let items: Vec<JsValue> = pairs
        .into_iter()
        .map(|(k, v)| {
            let pair = [JsString::from(k).into(), JsString::from(v).into()];
            JsArray::from_iter(pair, ctx).into()
        })
        .collect();
    Ok(JsArray::from_iter(items, ctx).into())
}

/// `keys()` → an array of the entry names, in order.
fn fd_keys(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let keys: Vec<JsValue> = with_fd(this, |d| {
        d.entries
            .iter()
            .map(|(k, _)| JsString::from(k.as_str()).into())
            .collect()
    })?;
    Ok(JsArray::from_iter(keys, ctx).into())
}

/// `values()` → an array of the entry values, in order.
fn fd_values(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let values: Vec<JsValue> = with_fd(this, |d| {
        d.entries
            .iter()
            .map(|(_, v)| JsString::from(v.as_str()).into())
            .collect()
    })?;
    Ok(JsArray::from_iter(values, ctx).into())
}
