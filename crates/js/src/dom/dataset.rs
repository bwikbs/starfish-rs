//! E8-M3 `element.dataset` — a plain `JsObject` built fresh per `.dataset`
//! access (the `el.style`/`el.classList` precedent) exposing the element's
//! `data-*` attributes with camelCase keys. Each existing key is an accessor
//! (getter reads, setter writes the attribute); a non-standard `set(name,value)`
//! helper adds NEW keys (`el.dataset.set('newKey','v')` → `data-new-key`), since
//! assigning an arbitrary new camelCase property would need exotic named-property
//! machinery (deferred). READ is a per-access snapshot.

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsResult, JsString, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};
use starfish_dom::NodeKind;

use super::node::arg_str;
use super::NodeHandle;

/// Capture for a single dataset accessor: the element handle + the kebab `data-*`
/// attribute name. A Trace-able newtype (holds no Gc pointers) so it can be a
/// `from_copy_closure_with_captures` capture (the closure stays `Copy`).
#[derive(Clone, Finalize)]
struct DataProp(NodeHandle, String);

// SAFETY: holds only a `NodeHandle` (all `unsafe_ignore_trace` fields) + a
// `String` — no Boa `Gc` pointers — nothing to trace.
unsafe impl Trace for DataProp {
    boa_gc::empty_trace!();
}

/// `data-foo-bar` (the part AFTER `data-`) → `fooBar`: each `-x` (hyphen + ASCII
/// letter) becomes the uppercased letter; everything else passes through.
fn attr_to_camel(rest: &str) -> String {
    let mut out = String::with_capacity(rest.len());
    let mut up = false;
    for c in rest.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `fooBar` → `data-foo-bar`: each ASCII uppercase `X` becomes `-x`.
fn camel_to_attr(name: &str) -> String {
    let mut out = String::from("data-");
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Add a `get`/`set` accessor for one existing `data-*` key: getter reads the
/// attribute, setter writes it, both over the captured `NodeHandle` + the kebab
/// attribute name.
fn accessor_data_prop(init: &mut ObjectInitializer<'_>, camel: &str, handle: NodeHandle, attr: String) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        |_t, _a, cap: &DataProp, _ctx| {
            let doc = cap.0.shared.borrow();
            let v = doc.get_attribute(cap.0.id, &cap.1).unwrap_or("").to_string();
            Ok(JsString::from(v).into())
        },
        DataProp(handle.clone(), attr.clone()),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        |_t, args, cap: &DataProp, ctx| {
            let v = arg_str(args, 0, ctx)?;
            cap.0.shared.borrow_mut().set_attribute(cap.0.id, &cap.1, &v);
            Ok(JsValue::undefined())
        },
        DataProp(handle, attr),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!(camel),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// Build the `el.dataset` object: one accessor per existing `data-*` attribute
/// (camelCase key) + a `set(name, value)` helper for adding new keys.
pub(crate) fn dataset_object(h: NodeHandle, ctx: &mut Context) -> JsResult<JsValue> {
    // Snapshot the element's current data-* attributes as (camelProp, kebabAttr).
    let pairs: Vec<(String, String)> = {
        let doc = h.shared.borrow();
        match doc.kind(h.id) {
            NodeKind::Element(e) => e
                .attrs
                .iter()
                .filter(|a| a.name.starts_with("data-") && a.name.len() > 5)
                .map(|a| (attr_to_camel(&a.name[5..]), a.name.clone()))
                .filter(|(camel, _)| !camel.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };

    let mut init = ObjectInitializer::new(ctx);
    for (camel, attr) in pairs {
        accessor_data_prop(&mut init, &camel, h.clone(), attr);
    }
    // Non-standard `set(name, value)` escape-hatch for adding NEW keys.
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cap: &NodeHandle, ctx| {
                let name = arg_str(args, 0, ctx)?;
                let value = arg_str(args, 1, ctx)?;
                cap.shared
                    .borrow_mut()
                    .set_attribute(cap.id, &camel_to_attr(&name), &value);
                Ok(JsValue::undefined())
            },
            h,
        ),
        js_string!("set"),
        2,
    );

    Ok(init.build().into())
}
