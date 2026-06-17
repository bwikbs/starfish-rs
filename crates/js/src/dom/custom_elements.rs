//! E33-M3 custom elements (MVP): `customElements.define` + upgrade of existing
//! matching elements with their `connectedCallback`.
//!
//! Scope: `define(name, ctor)` records the constructor in [`DomState`] and, at
//! define time, walks the light DOM for already-parsed elements whose tag equals
//! `name`, invoking each constructor's `connectedCallback` (with the element
//! wrapper as `this`) during the one-shot script run — i.e. before the render
//! snapshot. `get(name)` returns the stored ctor (or undefined). Upgrade only
//! covers elements present at define time.
//!
//! E57-M1 extends the upgrade path with custom-element reactions:
//! `observedAttributes` + `attributeChangedCallback` (on upgrade and on
//! `setAttribute`/`removeAttribute` during the run) and `disconnectedCallback`
//! (on removal). Still deferred: reaction-queue ordering subtleties, `:defined`,
//! `adoptedCallback`, and constructor-bound `this`.

use boa_engine::object::ObjectInitializer;
use boa_engine::{js_string, Context, JsObject, JsString, JsValue, NativeFunction};

use super::{wrap_node, DomState};
use starfish_dom::NodeId;

/// A valid custom-element name must contain a `-` (lenient MVP check; the full
/// spec name grammar is not enforced).
fn is_valid_name(name: &str) -> bool {
    name.contains('-')
}

/// Call `ctor.prototype.<cb_name>` (if present + callable) with the element
/// `id` wrapped as `this` and `args`. A throwing callback is swallowed so one
/// bad component does not abort the upgrade pass / render (mirrors event
/// dispatch). Returns `Ok(())` (no-op) when the callback is absent. // E57-M1
fn call_proto_callback(
    id: NodeId,
    ctor: &JsObject,
    cb_name: &str,
    args: &[JsValue],
    ctx: &mut Context,
) -> boa_engine::JsResult<()> {
    let proto = ctor.get(js_string!("prototype"), ctx)?;
    let Some(proto) = proto.as_object() else {
        return Ok(());
    };
    let cb = proto.get(JsString::from(cb_name), ctx)?;
    let Some(cb) = cb.as_object().filter(|o| o.is_callable()) else {
        return Ok(());
    };
    let el = wrap_node(id, ctx)?;
    let _ = cb.call(&JsValue::from(el), args, ctx);
    Ok(())
}

/// Read the ctor's static `observedAttributes` as a `Vec<String>` (lowercased
/// to match arena attr names). Non-array / missing → empty. // E57-M1
fn read_observed_attributes(ctor: &JsObject, ctx: &mut Context) -> Vec<String> {
    let Ok(val) = ctor.get(js_string!("observedAttributes"), ctx) else {
        return Vec::new();
    };
    let Some(arr) = val.as_object().filter(|o| o.is_array()) else {
        return Vec::new();
    };
    let len = arr
        .get(js_string!("length"), ctx)
        .ok()
        .and_then(|v| v.to_length(ctx).ok())
        .unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..len {
        if let Ok(item) = arr.get(i, ctx) {
            if let Ok(s) = item.to_string(ctx) {
                out.push(s.to_std_string_escaped().to_ascii_lowercase());
            }
        }
    }
    out
}

/// Upgrade one element: per the spec, fire `attributeChangedCallback(name,
/// null, value)` for each currently-present observed attribute, THEN
/// `connectedCallback`. // E57-M1
fn upgrade_element(id: NodeId, ctor: &JsObject, ctx: &mut Context) -> boa_engine::JsResult<()> {
    let observed = read_observed_attributes(ctor, ctx);
    for name in observed {
        let current = {
            let host = ctx.realm().host_defined();
            let Some(state) = host.get::<DomState>() else {
                return Ok(());
            };
            let v = state.doc.borrow().get_attribute(id, &name).map(str::to_owned);
            v
        };
        if let Some(value) = current {
            let args = [
                JsString::from(name).into(),
                JsValue::null(),
                JsString::from(value).into(),
            ];
            call_proto_callback(id, ctor, "attributeChangedCallback", &args, ctx)?;
        }
    }
    call_proto_callback(id, ctor, "connectedCallback", &[], ctx)
}

/// E57-M1 reaction: after mutating `name` on element `id` via
/// `setAttribute`/`removeAttribute`, if `id`'s tag is a defined custom element
/// observing `name`, fire `attributeChangedCallback(name, old, new)`. `old`/`new`
/// are `None` for absent. No-op otherwise.
pub(crate) fn react_attribute_changed(
    ctx: &mut Context,
    id: NodeId,
    name: &str,
    old: Option<String>,
    new: Option<String>,
) {
    let name = name.to_ascii_lowercase();
    let ctor = {
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return;
        };
        let tag = match state.doc.borrow().tag_name(id) {
            Some(t) => t.to_owned(),
            None => return,
        };
        let found = state.custom_elements.borrow().get(&tag).cloned();
        match found {
            Some(c) => c,
            None => return,
        }
    };
    if !read_observed_attributes(&ctor, ctx).iter().any(|a| a == &name) {
        return;
    }
    let to_val = |o: Option<String>| o.map(|s| JsString::from(s).into()).unwrap_or(JsValue::null());
    let args = [JsString::from(name).into(), to_val(old), to_val(new)];
    let _ = call_proto_callback(id, &ctor, "attributeChangedCallback", &args, ctx);
}

/// E57-M1 reaction: after detaching element `id` from the tree, if its tag is a
/// defined custom element, fire `disconnectedCallback()`. No-op otherwise.
/// (MVP: only the directly-removed element, not deep descendants.)
pub(crate) fn react_disconnected(ctx: &mut Context, id: NodeId) {
    let ctor = {
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return;
        };
        let tag = match state.doc.borrow().tag_name(id) {
            Some(t) => t.to_owned(),
            None => return,
        };
        let found = state.custom_elements.borrow().get(&tag).cloned();
        match found {
            Some(c) => c,
            None => return,
        }
    };
    let _ = call_proto_callback(id, &ctor, "disconnectedCallback", &[], ctx);
}

/// Build the global `customElements` object: `define` / `get` / `whenDefined`.
pub(crate) fn build(ctx: &mut Context) -> JsObject {
    let mut init = ObjectInitializer::new(ctx);

    // define(name, ctor) -> undefined
    init.function(
        NativeFunction::from_fn_ptr(define),
        js_string!("define"),
        2,
    );
    // get(name) -> ctor | undefined
    init.function(NativeFunction::from_fn_ptr(get), js_string!("get"), 1);
    // whenDefined(name) -> undefined  (MVP: never resolves; non-blocking)
    init.function(
        NativeFunction::from_fn_ptr(when_defined),
        js_string!("whenDefined"),
        1,
    );

    init.build()
}

/// `customElements.define(name, ctor)`.
fn define(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> boa_engine::JsResult<JsValue> {
    let name = match args.first() {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped().to_ascii_lowercase(),
        None => return Ok(JsValue::undefined()),
    };
    let Some(ctor) = args.get(1).and_then(JsValue::as_object) else {
        return Ok(JsValue::undefined());
    };
    if !is_valid_name(&name) {
        return Ok(JsValue::undefined());
    }

    // Record the constructor (last definition wins; no `:defined`/redefine guard).
    {
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return Ok(JsValue::undefined());
        };
        state
            .custom_elements
            .borrow_mut()
            .insert(name.clone(), ctor.clone());
    }

    // Collect matching element ids, then DROP the doc borrow before invoking any
    // JS callback (connectedCallback can mutate the doc, e.g. attachShadow).
    let matches: Vec<NodeId> = {
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return Ok(JsValue::undefined());
        };
        let doc = state.doc.borrow();
        (0..doc.node_count())
            .map(NodeId::from_index)
            .filter(|&id| doc.tag_name(id) == Some(name.as_str()))
            .collect()
    };

    for id in matches {
        upgrade_element(id, &ctor, ctx)?;
    }
    Ok(JsValue::undefined())
}

/// `customElements.get(name)` -> the stored constructor, or undefined.
fn get(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> boa_engine::JsResult<JsValue> {
    let name = match args.first() {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped().to_ascii_lowercase(),
        None => return Ok(JsValue::undefined()),
    };
    let host = ctx.realm().host_defined();
    let Some(state) = host.get::<DomState>() else {
        return Ok(JsValue::undefined());
    };
    let ctor = state.custom_elements.borrow().get(&name).cloned();
    Ok(ctor
        .map(JsValue::from)
        .unwrap_or_else(JsValue::undefined))
}

/// `customElements.whenDefined(name)` — MVP stub: returns undefined (the crate
/// has no Promise-construction helper to mirror, and a never-resolving Promise
/// would be a worse lie). Non-blocking either way.
fn when_defined(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    Ok(JsValue::undefined())
}
