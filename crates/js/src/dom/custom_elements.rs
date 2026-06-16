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
//! Non-goals (per the roadmap): the constructor is NOT invoked with a bound
//! `this`; `disconnectedCallback`/`attributeChangedCallback`/`observedAttributes`,
//! elements created after `define`, `:defined`, and `whenDefined` blocking are
//! deferred.

use boa_engine::object::ObjectInitializer;
use boa_engine::{js_string, Context, JsObject, JsValue, NativeFunction};

use super::{wrap_node, DomState};
use starfish_dom::NodeId;

/// A valid custom-element name must contain a `-` (lenient MVP check; the full
/// spec name grammar is not enforced).
fn is_valid_name(name: &str) -> bool {
    name.contains('-')
}

/// Run `ctor.prototype.connectedCallback` (if any) with the element wrapped as
/// `this`. A single throwing callback is swallowed so one bad component does not
/// abort the upgrade pass / render (mirrors event-listener dispatch).
fn upgrade_element(id: NodeId, ctor: &JsObject, ctx: &mut Context) -> boa_engine::JsResult<()> {
    let proto = ctor.get(js_string!("prototype"), ctx)?;
    let Some(proto) = proto.as_object() else {
        return Ok(());
    };
    let cb = proto.get(js_string!("connectedCallback"), ctx)?;
    let Some(cb) = cb.as_object().filter(|o| o.is_callable()) else {
        return Ok(());
    };
    let el = wrap_node(id, ctx)?;
    let _ = cb.call(&JsValue::from(el), &[], ctx);
    Ok(())
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
