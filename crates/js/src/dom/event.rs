//! Events: `addEventListener` / `removeEventListener` / `dispatchEvent` on every
//! node (+ `window` via `globals`), a minimal `Event` object, target-phase
//! firing with an optional cheap bubble, and inline `on*` handler compilation.
//! Listener throws are caught (non-fatal); no `GcRefCell`/`RefCell` borrow is
//! held across a JS call (listener lists are snapshotted, then run).

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    Source,
};

use super::node::arg_str;
use super::{event_target_index, wrap_node, DomState, NodeHandle, WINDOW_KEY};

// Internal own-property keys carrying the Event's mutable flags.
const PREVENTED_KEY: &str = "__sf_prevented";
const STOPPED_KEY: &str = "__sf_stopped";

// --- registration ---

fn add_for_key(key: (u32, String), cb: JsObject, ctx: &mut Context) {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        let mut map = state.listeners.borrow_mut();
        let v = map.entry(key).or_default();
        // Spec dedup: same (type, callback) registered twice → once. Identity is
        // `JsObject` pointer equality.
        if !v.iter().any(|c| c == &cb) {
            v.push(cb);
        }
    }
}

fn remove_for_key(key: &(u32, String), cb: &JsObject, ctx: &mut Context) {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(v) = state.listeners.borrow_mut().get_mut(key) {
            v.retain(|c| c != cb);
        }
    }
}

/// `node.addEventListener(type, handler)`. Non-callable handler → no-op (matches
/// the spec's null-listener case; the `{handleEvent}` object form is deferred).
pub(crate) fn add_event_listener(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let ty = arg_str(args, 0, ctx)?;
    let Some(cb) = callable(args, 1) else {
        return Ok(JsValue::undefined());
    };
    add_for_key((event_target_index(&h), ty), cb, ctx);
    Ok(JsValue::undefined())
}

/// `node.removeEventListener(type, handler)`. Identity match; unknown → no-op.
pub(crate) fn remove_event_listener(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let ty = arg_str(args, 0, ctx)?;
    if let Some(cb) = args.get(1).and_then(boa_engine::JsValue::as_object) {
        remove_for_key(&(event_target_index(&h), ty), &cb, ctx);
    }
    Ok(JsValue::undefined())
}

/// `window.addEventListener(type, handler)` — keys under `WINDOW_KEY`.
pub(crate) fn window_add_listener(
    _t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let ty = arg_str(args, 0, ctx)?;
    let Some(cb) = callable(args, 1) else {
        return Ok(JsValue::undefined());
    };
    add_for_key((WINDOW_KEY, ty), cb, ctx);
    Ok(JsValue::undefined())
}

/// `window.removeEventListener(type, handler)`.
pub(crate) fn window_remove_listener(
    _t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let ty = arg_str(args, 0, ctx)?;
    if let Some(cb) = args.get(1).and_then(boa_engine::JsValue::as_object) {
        remove_for_key(&(WINDOW_KEY, ty), &cb, ctx);
    }
    Ok(JsValue::undefined())
}

fn callable(args: &[JsValue], i: usize) -> Option<JsObject> {
    args.get(i)
        .and_then(boa_engine::JsValue::as_object)
        .filter(boa_engine::JsObject::is_callable)
}

// --- the Event object ---

/// Build a fresh `Event` object: `type`, `target`/`currentTarget` (filled at
/// dispatch), `bubbles`, `defaultPrevented` (accessor), plus `preventDefault` /
/// `stopPropagation`. The mutable flags live as internal own properties.
pub(crate) fn build_event(ty: &str, bubbles: bool, ctx: &mut Context) -> JsObject {
    let prevent = NativeFunction::from_fn_ptr(|this, _a, ctx| {
        if let Some(o) = this.as_object() {
            let _ = o.set(js_string!(PREVENTED_KEY), true, false, ctx);
        }
        Ok(JsValue::undefined())
    });
    let stop = NativeFunction::from_fn_ptr(|this, _a, ctx| {
        if let Some(o) = this.as_object() {
            let _ = o.set(js_string!(STOPPED_KEY), true, false, ctx);
        }
        Ok(JsValue::undefined())
    });
    let get_prevented = NativeFunction::from_fn_ptr(|this, _a, ctx| {
        let prevented = this
            .as_object()
            .and_then(|o| o.get(js_string!(PREVENTED_KEY), ctx).ok())
            .map(|v| v.to_boolean())
            .unwrap_or(false);
        Ok(JsValue::from(prevented))
    });

    let mut init = ObjectInitializer::new(ctx);
    let realm = init.context().realm().clone();
    let getter = boa_engine::object::FunctionObjectBuilder::new(&realm, get_prevented).build();
    init.property(js_string!("type"), JsString::from(ty), Attribute::READONLY)
        .property(js_string!("target"), JsValue::null(), Attribute::all())
        .property(
            js_string!("currentTarget"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(js_string!("bubbles"), bubbles, Attribute::READONLY)
        .property(js_string!(PREVENTED_KEY), false, Attribute::all())
        .property(js_string!(STOPPED_KEY), false, Attribute::all())
        .function(prevent, js_string!("preventDefault"), 0)
        .function(stop, js_string!("stopPropagation"), 0)
        .accessor(
            js_string!("defaultPrevented"),
            Some(getter),
            None,
            Attribute::all(),
        );
    init.build()
}

/// The global `Event` constructor: `new Event('x')` / `Event('x')`.
pub(crate) fn event_constructor(
    _new_target: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let ty = arg_str(args, 0, ctx)?;
    Ok(build_event(&ty, true, ctx).into())
}

fn flag(event: &JsObject, key: &str, ctx: &mut Context) -> bool {
    event
        .get(js_string!(key), ctx)
        .map(|v| v.to_boolean())
        .unwrap_or(false)
}

// --- dispatch ---

/// `node.dispatchEvent(event)`. Sets `target`, fires target-phase listeners,
/// then optionally bubbles up the parent chain (honoring `stopPropagation`).
/// Returns `!defaultPrevented`.
pub(crate) fn dispatch_event(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let event = match args.first().and_then(boa_engine::JsValue::as_object) {
        Some(o) => o,
        None => {
            return Err(JsNativeError::typ()
                .with_message("dispatchEvent: argument is not an Event")
                .into())
        }
    };
    let ty = event
        .get(js_string!("type"), ctx)?
        .to_string(ctx)?
        .to_std_string_escaped();

    let target = this.clone();
    event.set(js_string!("target"), target, false, ctx)?;

    fire_at(&h, &ty, &event, this, ctx);

    if flag(&event, "bubbles", ctx) {
        let mut cur = h.shared.borrow().parent(h.id);
        while let Some(pid) = cur {
            if flag(&event, STOPPED_KEY, ctx) {
                break;
            }
            let ph = NodeHandle {
                shared: h.shared.clone(),
                id: pid,
            };
            let pwrapper: JsValue = wrap_node(pid, ctx)?.into();
            fire_at(&ph, &ty, &event, &pwrapper, ctx);
            cur = h.shared.borrow().parent(pid);
        }
    }

    Ok(JsValue::from(!flag(&event, PREVENTED_KEY, ctx)))
}

/// Fire `(target, type)` listeners + the inline `on<type>` handler on a SNAPSHOT
/// of the listener list (safe under remove-during-dispatch). `current` is the
/// `this`/`currentTarget` for this node. Throws are caught (non-fatal).
pub(crate) fn fire_at(
    h: &NodeHandle,
    ty: &str,
    event: &JsObject,
    current: &JsValue,
    ctx: &mut Context,
) {
    fire_for_index(event_target_index(h), ty, event, current, ctx);
    run_inline_handler(h, ty, event, current, ctx);
}

/// Fire listeners registered under an explicit registry index (used for `window`
/// lifecycle events, which have no `NodeHandle`).
pub(crate) fn fire_for_index(
    index: u32,
    ty: &str,
    event: &JsObject,
    current: &JsValue,
    ctx: &mut Context,
) {
    let listeners: Vec<JsObject> = match ctx.realm().host_defined().get::<DomState>() {
        Some(state) => state
            .listeners
            .borrow()
            .get(&(index, ty.to_string()))
            .cloned()
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let _ = event.set(js_string!("currentTarget"), current.clone(), false, ctx);
    for cb in listeners {
        let _ = cb.call(current, &[event.clone().into()], ctx);
    }
}

/// Compile + run an element's `on<type>` attribute as `function(event){ … }`.
/// Compile error / throw → caught, non-fatal.
pub(crate) fn run_inline_handler(
    h: &NodeHandle,
    ty: &str,
    event: &JsObject,
    current: &JsValue,
    ctx: &mut Context,
) {
    let attr = format!("on{ty}");
    let body = h
        .shared
        .borrow()
        .get_attribute(h.id, &attr)
        .map(str::to_string);
    let Some(body) = body.filter(|b| !b.trim().is_empty()) else {
        return;
    };
    let src = format!("(function(event){{ {body}\n}})");
    if let Ok(f) = ctx.eval(Source::from_bytes(src.as_bytes())) {
        if let Some(f) = f.as_object().filter(boa_engine::JsObject::is_callable) {
            let _ = event.set(js_string!("currentTarget"), current.clone(), false, ctx);
            let _ = f.call(current, &[event.clone().into()], ctx);
        }
    }
}
