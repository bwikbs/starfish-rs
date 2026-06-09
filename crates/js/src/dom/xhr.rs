//! E8-M2 minimal **synchronous** `XMLHttpRequest`.
//!
//! A native Boa class carrying mutable per-instance request/response state. The
//! load is performed synchronously inside `send()` against the borrowed
//! `ResourceLoader` (same `loader_and_base` mechanism as `fetch`), so by the time
//! `send()` returns every field is populated and the next script line can read
//! `responseText`/`status`.
//!
//! Deferred (M2 non-goals): the `async` flag is accepted and ignored (always
//! synchronous); no events (`onreadystatechange`/`onload`/…), no intermediate
//! `readyState` transitions (jumps to 4), no `responseType`/`response`/
//! `responseXML`, no request body upload, no `setRequestHeader` effect, no
//! `abort()`/`timeout`/`withCredentials`. A failed load is lenient: it sets a
//! status (0/404/the HTTP status), `readyState=4`, `responseText=""`, and does
//! NOT throw.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace};
use starfish_net::LoadError;

use super::node::arg_str;
use super::{accessor, loader_and_base, method};

/// Native data for an `XMLHttpRequest` instance. Holds no GC pointers → every
/// field is `unsafe_ignore_trace`d.
#[derive(Trace, Finalize, JsData, Default)]
pub(crate) struct Xhr {
    #[unsafe_ignore_trace]
    method: String,
    #[unsafe_ignore_trace]
    url: String,
    #[unsafe_ignore_trace]
    ready_state: u16,
    #[unsafe_ignore_trace]
    status: u16,
    #[unsafe_ignore_trace]
    status_text: String,
    #[unsafe_ignore_trace]
    response_text: String,
    #[unsafe_ignore_trace]
    content_type: Option<String>,
}

impl Class for Xhr {
    const NAME: &'static str = "XMLHttpRequest";
    const LENGTH: usize = 0;

    fn data_constructor(
        _new_target: &JsValue,
        _args: &[JsValue],
        _context: &mut Context,
    ) -> JsResult<Self> {
        Ok(Xhr::default())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "open", 2, open);
        method(class, "send", 1, send);
        method(class, "getResponseHeader", 1, get_response_header);
        accessor(class, "readyState", get_ready_state, None);
        accessor(class, "status", get_status, None);
        accessor(class, "statusText", get_status_text, None);
        accessor(class, "responseText", get_response_text, None);
        Ok(())
    }
}

/// Run `f` with a mutable borrow of `this`'s `Xhr` data, throwing `TypeError`
/// if `this` is not an `XMLHttpRequest`. The owned `JsObject` (a cheap Gc clone)
/// is kept alive in scope so the `downcast_mut` borrow is valid for `f`.
fn with_xhr<R>(this: &JsValue, f: impl FnOnce(&mut Xhr) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not an XMLHttpRequest"))?;
    let mut data = obj
        .downcast_mut::<Xhr>()
        .ok_or_else(|| JsNativeError::typ().with_message("not an XMLHttpRequest"))?;
    Ok(f(&mut data))
}

/// `open(method, url[, async])` — store method (uppercased) + url; `async` is
/// accepted and ignored (always synchronous). `readyState = 1` (OPENED).
fn open(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let method = arg_str(args, 0, ctx)?.to_ascii_uppercase();
    let url = arg_str(args, 1, ctx)?;
    with_xhr(this, |xhr| {
        xhr.method = method;
        xhr.url = url;
        xhr.ready_state = 1;
    })?;
    Ok(JsValue::undefined())
}

/// `send([body])` — resolve the URL against the base, fetch synchronously,
/// populate the response fields, set `readyState = 4`. `body` is ignored (M2).
/// A failed load is lenient (status 0/404/HTTP, no throw).
fn send(this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let raw_url = with_xhr(this, |xhr| xhr.url.clone())?;

    let (loader, base) = match loader_and_base(ctx) {
        Some(lb) => lb,
        None => return finish(this, 0, "", "", None),
    };
    let url = match base.join(&raw_url) {
        Ok(u) => u,
        Err(_) => return finish(this, 0, "", "", None),
    };

    match loader.fetch(&url) {
        Ok(res) => {
            let text = String::from_utf8_lossy(&res.bytes).into_owned();
            finish(this, 200, "OK", &text, res.content_type)
        }
        Err(LoadError::Http { status, .. }) => finish(this, status, "", "", None),
        Err(LoadError::NotFound(_)) => finish(this, 404, "Not Found", "", None),
        Err(_) => finish(this, 0, "", "", None),
    }
}

/// Write the response fields into the instance and set `readyState = 4`.
fn finish(
    this: &JsValue,
    status: u16,
    status_text: &str,
    response_text: &str,
    content_type: Option<String>,
) -> JsResult<JsValue> {
    with_xhr(this, |xhr| {
        xhr.status = status;
        xhr.status_text = status_text.to_string();
        xhr.response_text = response_text.to_string();
        xhr.content_type = content_type;
        xhr.ready_state = 4;
    })?;
    Ok(JsValue::undefined())
}

/// `getResponseHeader(name)` — `content-type` (case-insensitive) → the stored
/// content type; any other name → `null`.
fn get_response_header(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    with_xhr(this, |xhr| {
        if name == "content-type" {
            match &xhr.content_type {
                Some(ct) if !ct.is_empty() => JsString::from(ct.as_str()).into(),
                _ => JsValue::null(),
            }
        } else {
            JsValue::null()
        }
    })
}

fn get_ready_state(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_xhr(this, |xhr| (xhr.ready_state as f64).into())
}

fn get_status(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_xhr(this, |xhr| (xhr.status as f64).into())
}

fn get_status_text(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_xhr(this, |xhr| JsString::from(xhr.status_text.as_str()).into())
}

fn get_response_text(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_xhr(this, |xhr| {
        JsString::from(xhr.response_text.as_str()).into()
    })
}
