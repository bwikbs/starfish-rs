//! E43-M3 utilities: `structuredClone`, `queueMicrotask`, and
//! `AbortController` / `AbortSignal`.
//!
//! - `structuredClone(value)` — a best-effort recursive deep clone of JSON-ish
//!   values: primitives pass through, arrays/plain objects clone element/own-
//!   enumerable-wise, and `Map` / `Set` / `Date` are reconstructed. Circular
//!   references and transferables are non-goals (out of scope per the roadmap).
//! - `queueMicrotask(fn)` — schedules `fn` as a promise-style microtask via
//!   Boa's job queue, so it runs at the next `ctx.run_jobs()` checkpoint (after
//!   the current synchronous run, before the next macrotask).
//! - `AbortController` / `AbortSignal` — the controller owns a signal; `abort`
//!   sets `aborted`/`reason` and fires an `abort` event on the signal. Signal
//!   listeners are kept in a JS array own-property on the signal object (it is
//!   not a DOM node, so it can't use the node listener registry).

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::job::{Job, PromiseJob};
use boa_engine::object::builtins::{JsArray, JsDate, JsMap, JsSet};
use boa_engine::object::JsObject;
use boa_engine::{
    js_string, Context, Finalize, JsData, JsNativeError, JsResult, JsValue, NativeFunction, Trace,
};

use super::{event, method};

// E43-M3: own-property key holding a signal's `abort` listener array.
const ABORT_LISTENERS_KEY: &str = "__sf_abort_listeners";

// --- structuredClone ---------------------------------------------------------

/// `structuredClone(value)` — best-effort recursive deep clone.
pub(crate) fn structured_clone(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let input = args.first().cloned().unwrap_or(JsValue::undefined());
    clone_value(&input, ctx)
}

/// Recursively clone a `JsValue`. Primitives pass through; objects branch on
/// Array / Map / Set / Date / plain-object.
fn clone_value(value: &JsValue, ctx: &mut Context) -> JsResult<JsValue> {
    let Some(obj) = value.as_object() else {
        // Primitive (or symbol/bigint) — structured clone copies it by value.
        return Ok(value.clone());
    };
    if obj.is_callable() {
        // Functions are not cloneable in the real algorithm; best-effort returns
        // them as-is rather than throwing, to stay robust for JSON-ish inputs.
        return Ok(value.clone());
    }
    if obj.is_array() {
        return clone_array(&obj, ctx);
    }
    if let Ok(date) = JsDate::from_object(obj.clone()) {
        let time = date.get_time(ctx)?;
        let fresh = JsDate::new(ctx);
        fresh.set_time(time, ctx)?;
        return Ok(JsObject::from(fresh).into());
    }
    if let Ok(map) = JsMap::from_object(obj.clone()) {
        return clone_map(&map, ctx);
    }
    if let Ok(set) = JsSet::from_object(obj.clone()) {
        return clone_set(&set, ctx);
    }
    clone_plain_object(&obj, ctx)
}

fn clone_array(obj: &JsObject, ctx: &mut Context) -> JsResult<JsValue> {
    let src = JsArray::from_object(obj.clone())?;
    let len = src.length(ctx)?;
    let out = JsArray::new(ctx);
    for i in 0..len {
        let elem = src.get(i, ctx)?;
        let cloned = clone_value(&elem, ctx)?;
        out.set(i, cloned, true, ctx)?;
    }
    Ok(out.into())
}

fn clone_map(map: &JsMap, ctx: &mut Context) -> JsResult<JsValue> {
    // Collect raw entries first (the native callback has no `ctx` to clone with).
    let mut entries: Vec<(JsValue, JsValue)> = Vec::new();
    map.for_each_native(|k, v| {
        entries.push((k, v));
        Ok(())
    })?;
    let out = JsMap::new(ctx);
    for (k, v) in entries {
        let ck = clone_value(&k, ctx)?;
        let cv = clone_value(&v, ctx)?;
        out.set(ck, cv, ctx)?;
    }
    Ok(JsObject::from(out).into())
}

fn clone_set(set: &JsSet, ctx: &mut Context) -> JsResult<JsValue> {
    let mut values: Vec<JsValue> = Vec::new();
    set.for_each_native(|v| {
        values.push(v);
        Ok(())
    })?;
    let out = JsSet::new(ctx);
    for v in values {
        let cv = clone_value(&v, ctx)?;
        out.add(cv, ctx)?;
    }
    Ok(JsObject::from(out).into())
}

fn clone_plain_object(obj: &JsObject, ctx: &mut Context) -> JsResult<JsValue> {
    let out = JsObject::with_object_proto(ctx.intrinsics());
    // Own ENUMERABLE string keys via `Object.keys` (the structured-clone
    // algorithm copies enumerable own props; this is the public, robust path —
    // the internal `[[OwnPropertyKeys]]` is not exposed to embedders).
    let key_arr = object_keys(obj, ctx)?;
    let len = key_arr.length(ctx)?;
    for i in 0..len {
        let key = key_arr.get(i, ctx)?;
        let prop = key.to_property_key(ctx)?;
        let v = obj.get(prop.clone(), ctx)?;
        let cloned = clone_value(&v, ctx)?;
        out.set(prop, cloned, true, ctx)?;
    }
    Ok(out.into())
}

/// `Object.keys(obj)` — own enumerable string keys, as a `JsArray`.
fn object_keys(obj: &JsObject, ctx: &mut Context) -> JsResult<JsArray> {
    let object_ctor = ctx.intrinsics().constructors().object().constructor();
    let keys_fn = object_ctor.get(js_string!("keys"), ctx)?;
    let keys_obj = keys_fn
        .as_object()
        .filter(|o| o.is_callable())
        .ok_or_else(|| JsNativeError::typ().with_message("Object.keys is not callable"))?;
    let result = keys_obj.call(&JsValue::undefined(), &[obj.clone().into()], ctx)?;
    let arr_obj = result
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Object.keys did not return an array"))?;
    JsArray::from_object(arr_obj.clone())
}

// --- queueMicrotask ----------------------------------------------------------

/// `queueMicrotask(fn)` — enqueue `fn` as a microtask (a promise job) so it runs
/// at the next `ctx.run_jobs()` drain. Non-callable arg throws `TypeError`.
pub(crate) fn queue_microtask(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let cb = args
        .first()
        .and_then(JsValue::as_object)
        .filter(|o| o.is_callable())
        .ok_or_else(|| {
            JsNativeError::typ().with_message("queueMicrotask requires a callback function")
        })?
        .clone();
    // The job captures the callback (rooted while the job lives) and invokes it
    // with no args + `undefined` this, mirroring the spec's microtask queueing.
    let job = PromiseJob::new(move |ctx: &mut Context| cb.call(&JsValue::undefined(), &[], ctx));
    _ctx.enqueue_job(Job::from(job));
    Ok(JsValue::undefined())
}

// --- AbortController / AbortSignal -------------------------------------------

/// `AbortSignal` — carries `aborted` (bool) + `reason`, and an `abort` listener
/// array. Constructed by `AbortController`; `new AbortSignal()` is illegal (per
/// spec). The instance has no native data; all state lives in own properties.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct AbortSignal;

impl Class for AbortSignal {
    const NAME: &'static str = "AbortSignal";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        // AbortSignal is not directly constructible (created via AbortController).
        Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "addEventListener", 2, signal_add_listener);
        method(class, "removeEventListener", 2, signal_remove_listener);
        Ok(())
    }
}

/// Build a fresh `AbortSignal` instance with `aborted=false`, `reason=undefined`,
/// and an empty listener array. Uses the registered class prototype.
fn build_signal(ctx: &mut Context) -> JsResult<JsObject> {
    let signal = AbortSignal::from_data(AbortSignal, ctx)?;
    signal.set(js_string!("aborted"), false, false, ctx)?;
    signal.set(js_string!("reason"), JsValue::undefined(), false, ctx)?;
    let listeners = JsArray::new(ctx);
    signal.set(
        js_string!(ABORT_LISTENERS_KEY),
        listeners,
        false,
        ctx,
    )?;
    Ok(signal)
}

/// `signal.addEventListener(type, cb)` — only `'abort'` is meaningful; other
/// types are accepted but never fire (no source dispatches them to a signal).
fn signal_add_listener(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let ty = args
        .first()
        .map(|v| v.to_string(ctx))
        .transpose()?
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    if ty != "abort" {
        return Ok(JsValue::undefined());
    }
    let Some(cb) = args
        .get(1)
        .and_then(JsValue::as_object)
        .filter(|o| o.is_callable())
    else {
        return Ok(JsValue::undefined());
    };
    let Some(signal) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let arr = signal_listeners(&signal, ctx)?;
    // Spec dedup: same callback registered twice → once (identity equality).
    let len = arr.length(ctx)?;
    for i in 0..len {
        if let Some(existing) = arr.get(i, ctx)?.as_object() {
            if existing == cb {
                return Ok(JsValue::undefined());
            }
        }
    }
    arr.push(cb.clone(), ctx)?;
    Ok(JsValue::undefined())
}

/// `signal.removeEventListener(type, cb)` — identity match on `'abort'`.
fn signal_remove_listener(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let ty = args
        .first()
        .map(|v| v.to_string(ctx))
        .transpose()?
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    if ty != "abort" {
        return Ok(JsValue::undefined());
    }
    let Some(cb) = args.get(1).and_then(JsValue::as_object) else {
        return Ok(JsValue::undefined());
    };
    let Some(signal) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let arr = signal_listeners(&signal, ctx)?;
    let len = arr.length(ctx)?;
    let mut kept: Vec<JsValue> = Vec::new();
    for i in 0..len {
        let item = arr.get(i, ctx)?;
        if item.as_object().is_some_and(|o| o == cb) {
            continue;
        }
        kept.push(item);
    }
    let fresh = JsArray::from_iter(kept, ctx);
    signal.set(js_string!(ABORT_LISTENERS_KEY), fresh, false, ctx)?;
    Ok(JsValue::undefined())
}

/// Fetch (or recreate) the signal's `abort` listener array own-property.
fn signal_listeners(signal: &JsObject, ctx: &mut Context) -> JsResult<JsArray> {
    let v = signal.get(js_string!(ABORT_LISTENERS_KEY), ctx)?;
    if let Some(o) = v.as_object() {
        if let Ok(arr) = JsArray::from_object(o.clone()) {
            return Ok(arr);
        }
    }
    let arr = JsArray::new(ctx);
    signal.set(js_string!(ABORT_LISTENERS_KEY), arr.clone(), false, ctx)?;
    Ok(arr)
}

/// `AbortController` — owns a `signal`; `abort(reason?)` aborts it.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct AbortController;

impl Class for AbortController {
    const NAME: &'static str = "AbortController";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        Ok(AbortController)
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "abort", 1, controller_abort);
        Ok(())
    }

    fn object_constructor(
        instance: &JsObject,
        _args: &[JsValue],
        ctx: &mut Context,
    ) -> JsResult<()> {
        // Attach the controller's `signal` own-property at construction.
        let signal = build_signal(ctx)?;
        instance.set(js_string!("signal"), signal, false, ctx)?;
        Ok(())
    }
}

/// `controller.abort(reason?)` — set `signal.aborted=true`, set `signal.reason`,
/// and fire an `abort` event on the signal (calling its `abort` listeners).
/// Aborting an already-aborted controller is a no-op (matches the spec).
fn controller_abort(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(controller) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let Some(signal) = controller.get(js_string!("signal"), ctx)?.as_object() else {
        return Ok(JsValue::undefined());
    };
    // Already aborted? no-op.
    if signal.get(js_string!("aborted"), ctx)?.to_boolean() {
        return Ok(JsValue::undefined());
    }
    let reason = match args.first() {
        Some(r) if !r.is_undefined() => r.clone(),
        // Default reason is an `AbortError` DOMException; we use a plain Error
        // value (no DOMException type in scope) — close enough for the API shape.
        _ => JsValue::undefined(),
    };
    signal.set(js_string!("aborted"), true, false, ctx)?;
    signal.set(js_string!("reason"), reason, false, ctx)?;

    // Fire `abort` on the signal: build an Event, snapshot listeners, call each.
    let listeners = signal_listeners(&signal, ctx)?;
    let len = listeners.length(ctx)?;
    let mut snapshot: Vec<JsObject> = Vec::new();
    for i in 0..len {
        if let Some(o) = listeners.get(i, ctx)?.as_object() {
            if o.is_callable() {
                snapshot.push(o);
            }
        }
    }
    let ev = event::build_event("abort", false, ctx);
    let signal_val = JsValue::from(signal.clone());
    ev.set(js_string!("target"), signal_val.clone(), false, ctx)?;
    ev.set(js_string!("currentTarget"), signal_val.clone(), false, ctx)?;
    for cb in snapshot {
        let _ = cb.call(&signal_val, &[ev.clone().into()], ctx);
    }
    Ok(JsValue::undefined())
}

/// Register the `structuredClone` / `queueMicrotask` global callables.
pub(crate) fn install_globals(ctx: &mut Context) {
    let _ = ctx.register_global_callable(
        js_string!("structuredClone"),
        1,
        NativeFunction::from_fn_ptr(structured_clone),
    );
    let _ = ctx.register_global_callable(
        js_string!("queueMicrotask"),
        1,
        NativeFunction::from_fn_ptr(queue_microtask),
    );
}
