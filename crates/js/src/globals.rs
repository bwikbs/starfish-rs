//! Minimal globals so common scripts don't `ReferenceError`: `window` aliasing
//! `globalThis`, a read-only `navigator.userAgent`, a `location.*` reflecting the
//! document `Url`, and a trivial read-only `document` stub (`title`, `URL`). NO
//! DOM bindings — those are E4-M2 (§4.2 of the design note). The `&shared` handle
//! is taken now (unused in M1) so M2 clones it into the real `document` host
//! object at the same install seam.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    js_string, object::FunctionObjectBuilder, object::ObjectInitializer, property::Attribute,
    Context, JsString, NativeFunction,
};

use crate::dom::{event, fetch, timer};
use starfish_dom::{Document, NodeKind};
use starfish_net::{ResourceLoader, Url};

/// The single UA literal the project standardizes on (placeholder).
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (compatible; starfish-rs/0.0)";

/// Register `window`/`navigator`/`location`/`document` on the global object.
/// `shared` is the in-flight `Rc<RefCell<Document>>` (read here for the
/// `document.title` probe; M2 clones it into the real DOM binding).
pub(crate) fn install(
    ctx: &mut Context,
    shared: &Rc<RefCell<Document>>,
    base: &Url,
    loader: &dyn ResourceLoader,
    sheets: Rc<Vec<starfish_css::Stylesheet>>,
) {
    // window === globalThis: register `window` as the realm's global object so
    // `window.foo` and a bare `foo` reference the same global.
    let global = ctx.global_object();
    let _ = ctx.register_global_property(js_string!("window"), global, Attribute::all());

    // navigator.userAgent (read-only).
    let navigator = ObjectInitializer::new(ctx)
        .property(
            js_string!("userAgent"),
            JsString::from(USER_AGENT),
            Attribute::READONLY,
        )
        .build();
    let _ = ctx.register_global_property(js_string!("navigator"), navigator, Attribute::all());

    // location.{href,protocol,host,pathname,origin} from `base`.
    let location = build_location(ctx, base);
    let _ = ctx.register_global_property(js_string!("location"), location, Attribute::all());

    // document: the real DOM binding (E4-M2) — a `Node` host object over the
    // arena root, plus a read-only `URL` own-property (the base href, which the
    // class can't carry). `install` registers the `Node` class + the cache.
    match crate::dom::install(ctx, shared, base, loader, sheets) {
        Ok(document) => {
            let _ = document.set(js_string!("URL"), JsString::from(base.as_str()), false, ctx);
            let _ =
                ctx.register_global_property(js_string!("document"), document, Attribute::all());
        }
        Err(_) => {
            // Should not happen, but never fail script setup: fall back to the
            // M1 read-only stub so `document.title`/`URL` still resolve.
            let document = build_document_stub(ctx, shared, base);
            let _ =
                ctx.register_global_property(js_string!("document"), document, Attribute::all());
        }
    }

    // E4-M3: timers, window-level EventTarget methods, the Event constructor.
    install_timers(ctx);
    install_window_events(ctx);
    install_event_constructor(ctx);

    // E8-M1: window.getComputedStyle(el).
    let _ = ctx.register_global_callable(
        js_string!("getComputedStyle"),
        1,
        NativeFunction::from_fn_ptr(crate::dom::computed::get_computed_style),
    );

    // E8-M2: fetch(url[, init]). (XMLHttpRequest is registered in dom::install.)
    let _ = ctx.register_global_callable(
        js_string!("fetch"),
        1,
        NativeFunction::from_fn_ptr(fetch::fetch),
    );

    // E8-M3: localStorage / sessionStorage — two independent in-memory stores
    // (READONLY global properties; the backing Vecs live in DomState, seeded by
    // dom::install above). URL/URLSearchParams are registered as classes there.
    if let Some((local_map, session_map)) = crate::dom::storage::maps(ctx) {
        let local = crate::dom::storage::build_storage_object(local_map, ctx);
        let session = crate::dom::storage::build_storage_object(session_map, ctx);
        let _ =
            ctx.register_global_property(js_string!("localStorage"), local, Attribute::READONLY);
        let _ = ctx.register_global_property(
            js_string!("sessionStorage"),
            session,
            Attribute::READONLY,
        );
    }
}

/// Register `setTimeout`/`setInterval`/`clearTimeout`/`clearInterval` as global
/// callables (they enqueue into the host-defined timer queue).
fn install_timers(ctx: &mut Context) {
    let _ = ctx.register_global_callable(
        js_string!("setTimeout"),
        2,
        NativeFunction::from_fn_ptr(timer::set_timeout),
    );
    let _ = ctx.register_global_callable(
        js_string!("setInterval"),
        2,
        NativeFunction::from_fn_ptr(timer::set_interval),
    );
    let _ = ctx.register_global_callable(
        js_string!("clearTimeout"),
        1,
        NativeFunction::from_fn_ptr(timer::clear_timer),
    );
    let _ = ctx.register_global_callable(
        js_string!("clearInterval"),
        1,
        NativeFunction::from_fn_ptr(timer::clear_timer),
    );
}

/// `window === globalThis`, so a bare `addEventListener('load', …)` and
/// `window.addEventListener('load', …)` both hit these global callables, keyed
/// under `WINDOW_KEY`.
fn install_window_events(ctx: &mut Context) {
    let _ = ctx.register_global_callable(
        js_string!("addEventListener"),
        2,
        NativeFunction::from_fn_ptr(event::window_add_listener),
    );
    let _ = ctx.register_global_callable(
        js_string!("removeEventListener"),
        2,
        NativeFunction::from_fn_ptr(event::window_remove_listener),
    );
}

/// Expose a minimal `Event` constructor: `new Event('x')` / `Event('x')`.
fn install_event_constructor(ctx: &mut Context) {
    let realm = ctx.realm().clone();
    let ctor = FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_fn_ptr(event::event_constructor),
    )
    .name(js_string!("Event"))
    .constructor(true)
    .build();
    let _ = ctx.register_global_property(js_string!("Event"), ctor, Attribute::all());
}

/// Read-only `location` object reflecting the document `Url`.
fn build_location(ctx: &mut Context, base: &Url) -> boa_engine::JsObject {
    let protocol = format!("{}:", base.scheme());
    let host = base.host_str().unwrap_or("").to_string();
    let pathname = base.path().to_string();
    let origin = base.origin().ascii_serialization();
    ObjectInitializer::new(ctx)
        .property(
            js_string!("href"),
            JsString::from(base.as_str()),
            Attribute::READONLY,
        )
        .property(
            js_string!("protocol"),
            JsString::from(protocol),
            Attribute::READONLY,
        )
        .property(
            js_string!("host"),
            JsString::from(host),
            Attribute::READONLY,
        )
        .property(
            js_string!("pathname"),
            JsString::from(pathname),
            Attribute::READONLY,
        )
        .property(
            js_string!("origin"),
            JsString::from(origin),
            Attribute::READONLY,
        )
        .build()
}

/// Trivial read-only `document` stub: `title` (first `<title>` text) + `URL`.
/// NOT the DOM binding (that is M2 — it replaces this stub at the same seam).
fn build_document_stub(
    ctx: &mut Context,
    shared: &Rc<RefCell<Document>>,
    base: &Url,
) -> boa_engine::JsObject {
    let title = document_title(&shared.borrow());
    ObjectInitializer::new(ctx)
        .property(
            js_string!("title"),
            JsString::from(title),
            Attribute::READONLY,
        )
        .property(
            js_string!("URL"),
            JsString::from(base.as_str()),
            Attribute::READONLY,
        )
        .build()
}

/// Text of the document's first `<title>` element, or `""`.
fn document_title(doc: &Document) -> String {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("title") {
            let mut s = String::new();
            for c in doc.children(id) {
                if let NodeKind::Text(t) = doc.kind(c) {
                    s.push_str(t);
                }
            }
            return s;
        }
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    String::new()
}
