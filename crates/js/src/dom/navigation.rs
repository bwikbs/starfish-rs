//! E19-M3 `location` / `history` host objects. Both reflect/mutate ONLY the
//! per-render navigation state in [`DomState`] (`current_url` / `history_state`
//! / `history_length`) — never the arena, never the network. `location.hash` /
//! `location.search` setters and `history.pushState`/`replaceState` update the
//! in-memory `Url`; everything navigational (`assign`/`reload`/`back`/...) is an
//! inert no-op (no real navigation in a one-shot render).

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use starfish_net::Url;

use super::DomState;

/// Read the current navigation `Url` from `DomState` (clone). Falls back to a
/// dummy `about:blank` if state is somehow missing (never panics).
fn current_url(ctx: &mut Context) -> Url {
    ctx.realm()
        .host_defined()
        .get::<DomState>()
        .map(|s| s.current_url.borrow().clone())
        .unwrap_or_else(|| Url::parse("about:blank").unwrap())
}

/// A `location` getter closure that derives a string from the current `Url`.
fn loc_getter(
    init: &mut ObjectInitializer<'_>,
    name: &str,
    f: impl Fn(&Url) -> String + Copy + 'static,
) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure(move |_t, _a, ctx| {
        let url = current_url(ctx);
        Ok(JsString::from(f(&url)).into())
    });
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    init.accessor(js_string!(name), Some(getter), None, Attribute::all());
}

/// Mutate the current `Url` in `DomState`, then echo it (used by setters).
fn with_url_mut(ctx: &mut Context, f: impl FnOnce(&mut Url)) {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        f(&mut state.current_url.borrow_mut());
    }
}

/// Build the `location` object: read accessors over the current `Url`, settable
/// `hash`/`search`, and inert `assign`/`replace`/`reload`/`toString`.
pub(crate) fn build_location(ctx: &mut Context) -> JsObject {
    let mut init = ObjectInitializer::new(ctx);

    loc_getter(&mut init, "href", |u| u.as_str().to_string());
    loc_getter(&mut init, "protocol", |u| format!("{}:", u.scheme()));
    loc_getter(&mut init, "host", |u| match u.port() {
        Some(p) => format!("{}:{}", u.host_str().unwrap_or(""), p),
        None => u.host_str().unwrap_or("").to_string(),
    });
    loc_getter(&mut init, "hostname", |u| {
        u.host_str().unwrap_or("").to_string()
    });
    loc_getter(&mut init, "port", |u| {
        u.port().map(|p| p.to_string()).unwrap_or_default()
    });
    loc_getter(&mut init, "origin", |u| u.origin().ascii_serialization());
    loc_getter(&mut init, "pathname", |u| u.path().to_string());
    loc_getter(&mut init, "search", |u| match u.query() {
        Some(q) if !q.is_empty() => format!("?{q}"),
        _ => String::new(),
    });
    loc_getter(&mut init, "hash", |u| match u.fragment() {
        Some(f) if !f.is_empty() => format!("#{f}"),
        _ => String::new(),
    });

    // Settable hash / search (strip the leading sigil).
    settable(
        &mut init,
        "hash",
        |u| match u.fragment() {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        },
        |ctx, v| {
            let frag = v.strip_prefix('#').unwrap_or(&v);
            with_url_mut(ctx, |u| {
                u.set_fragment(if frag.is_empty() { None } else { Some(frag) });
            });
        },
    );
    settable(
        &mut init,
        "search",
        |u| match u.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        },
        |ctx, v| {
            let q = v.strip_prefix('?').unwrap_or(&v);
            with_url_mut(ctx, |u| {
                u.set_query(if q.is_empty() { None } else { Some(q) });
            });
        },
    );

    for name in ["assign", "replace", "reload"] {
        init.function(
            NativeFunction::from_fn_ptr(|_t, _a, _ctx| Ok(JsValue::undefined())),
            js_string!(name),
            1,
        );
    }
    init.function(
        NativeFunction::from_fn_ptr(|_t, _a, ctx| {
            Ok(JsString::from(current_url(ctx).as_str()).into())
        }),
        js_string!("toString"),
        0,
    );

    init.build()
}

/// Add a read+write accessor (`hash`/`search`): a getter mirroring the current
/// value plus a setter that mutates the `Url`. (Re-defines the name set by
/// `loc_getter` WITH a setter, so it must run after `loc_getter`.)
fn settable(
    init: &mut ObjectInitializer<'_>,
    name: &str,
    get: impl Fn(&Url) -> String + Copy + 'static,
    set: impl Fn(&mut Context, String) + Copy + 'static,
) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure(move |_t, _a, ctx| {
        let url = current_url(ctx);
        Ok(JsString::from(get(&url)).into())
    });
    let setter = NativeFunction::from_copy_closure(move |_t, args, ctx| {
        let v = match args.first() {
            Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
            None => String::new(),
        };
        set(ctx, v);
        Ok(JsValue::undefined())
    });
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!(name),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// `history.pushState(state, title, url)` — resolve `url` against the base, store
/// `state`, bump `length`. `replaceState` skips the length bump.
fn push_state(ctx: &mut Context, args: &[JsValue], bump_length: bool) -> JsResult<JsValue> {
    // Resolve the optional url arg against the base BEFORE borrowing state mut.
    let url_arg = match args.get(2) {
        Some(v) if v.is_string() => Some(v.to_string(ctx)?.to_std_string_escaped()),
        _ => None,
    };
    let state_val = args.first().cloned().unwrap_or(JsValue::null());
    if let Some(s) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(rel) = url_arg {
            let resolved = s.base.join(&rel).ok();
            if let Some(u) = resolved {
                *s.current_url.borrow_mut() = u;
            }
        }
        *s.history_state.borrow_mut() = state_val;
        if bump_length {
            s.history_length
                .set(s.history_length.get().saturating_add(1));
        }
    }
    Ok(JsValue::undefined())
}

/// Build the `history` object: `pushState`/`replaceState`, `state`/`length`
/// getters, and inert `back`/`forward`/`go`.
pub(crate) fn build_history(ctx: &mut Context) -> JsObject {
    let mut init = ObjectInitializer::new(ctx);

    init.function(
        NativeFunction::from_fn_ptr(|_t, args, ctx| push_state(ctx, args, true)),
        js_string!("pushState"),
        3,
    );
    init.function(
        NativeFunction::from_fn_ptr(|_t, args, ctx| push_state(ctx, args, false)),
        js_string!("replaceState"),
        3,
    );
    for name in ["back", "forward", "go"] {
        init.function(
            NativeFunction::from_fn_ptr(|_t, _a, _ctx| Ok(JsValue::undefined())),
            js_string!(name),
            0,
        );
    }

    let realm = init.context().realm().clone();
    let state_getter = NativeFunction::from_copy_closure(|_t, _a, ctx| {
        Ok(ctx
            .realm()
            .host_defined()
            .get::<DomState>()
            .map(|s| s.history_state.borrow().clone())
            .unwrap_or(JsValue::null()))
    });
    let state_getter = FunctionObjectBuilder::new(&realm, state_getter).build();
    init.accessor(
        js_string!("state"),
        Some(state_getter),
        None,
        Attribute::all(),
    );

    let len_getter = NativeFunction::from_copy_closure(|_t, _a, ctx| {
        let n = ctx
            .realm()
            .host_defined()
            .get::<DomState>()
            .map(|s| s.history_length.get())
            .unwrap_or(1);
        Ok(JsValue::from(n))
    });
    let len_getter = FunctionObjectBuilder::new(&realm, len_getter).build();
    init.accessor(
        js_string!("length"),
        Some(len_getter),
        None,
        Attribute::all(),
    );

    init.build()
}
