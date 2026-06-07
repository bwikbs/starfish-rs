//! E8-M2 `fetch(url[, init])` → a *resolved* `Promise<Response>`.
//!
//! Since this is a one-shot run-to-quiescence renderer, the load happens
//! synchronously against the borrowed `ResourceLoader` the moment `fetch` is
//! called, but `fetch` still returns a **resolved** `JsPromise` so `.then(...)`
//! reactions queue as Boa jobs and drain through the existing `ctx.run_jobs()`
//! in the quiescence loop (no new job plumbing).
//!
//! Spec fidelity: `fetch` rejects **only on a network error**; an HTTP error
//! *status* (404/500) **resolves** with `ok=false`. Deferred (M2 non-goals):
//! `init` (accepted, mostly ignored — always GET, no request body/headers/CORS),
//! `Request` objects, `Response.body`/streaming/`arrayBuffer`/`blob`, `bodyUsed`
//! locking, the full `Headers` surface (only `get`, only `content-type`),
//! `AbortController`.

use boa_engine::object::builtins::JsPromise;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use starfish_net::{LoadError, Resource, Url};

use super::node::arg_str;
use super::{loader_and_base, type_err};

/// Global `fetch(url[, init])`. `init` (args[1]) is accepted and mostly ignored
/// for M2 (always issues a GET via the loader). Returns a resolved/rejected
/// `JsPromise` as a `JsValue`.
pub(crate) fn fetch(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = arg_str(args, 0, ctx)?;

    let (loader, base) = match loader_and_base(ctx) {
        Some(lb) => lb,
        None => return Ok(JsPromise::reject(type_err("fetch unavailable"), ctx).into()),
    };
    let url = match base.join(&input) {
        Ok(u) => u,
        Err(e) => return Ok(JsPromise::reject(type_err(&format!("{e}")), ctx).into()),
    };

    match loader.fetch(&url) {
        Ok(res) => {
            let response = build_response(&res, &url, true, 200, ctx)?;
            Ok(JsPromise::resolve(response, ctx).into())
        }
        Err(e) => settle_fetch_error(e, &url, ctx),
    }
}

/// Reject (network error) vs resolve-with-`ok=false` (HTTP error status / not
/// found), per the Fetch spec.
fn settle_fetch_error(e: LoadError, url: &Url, ctx: &mut Context) -> JsResult<JsValue> {
    match e {
        // HTTP error status → NOT a network error → resolve with ok=false + status.
        LoadError::Http { status, .. } => {
            let empty = Resource {
                bytes: Vec::new(),
                content_type: None,
                final_url: None,
            };
            let resp = build_response(&empty, url, false, status, ctx)?;
            Ok(JsPromise::resolve(resp, ctx).into())
        }
        // A `file://` miss has no HTTP status → treat as a 404 resolve (ok=false).
        LoadError::NotFound(_) => {
            let empty = Resource {
                bytes: Vec::new(),
                content_type: None,
                final_url: None,
            };
            let resp = build_response(&empty, url, false, 404, ctx)?;
            Ok(JsPromise::resolve(resp, ctx).into())
        }
        // Genuine transport failures + unsupported scheme + malformed URL → REJECT
        // (a TypeError, as the Fetch spec rejects with).
        other => Ok(JsPromise::reject(type_err(&format!("fetch failed: {other}")), ctx).into()),
    }
}

/// Build the `Response` plain object: data properties + `.headers` (with `get`)
/// + `.text()`/`.json()` returning resolved Promises over the UTF-8-lossy body.
fn build_response(
    res: &Resource,
    url: &Url,
    ok: bool,
    status: u16,
    ctx: &mut Context,
) -> JsResult<JsObject> {
    let body = String::from_utf8_lossy(&res.bytes).into_owned();
    let content_type = res.content_type.clone().unwrap_or_default();
    let status_text = if ok { "OK" } else { "" }.to_string();
    let headers = build_headers(&content_type, ctx);

    // .text() — capture `body`, return a RESOLVED promise of the string.
    let text_fn = NativeFunction::from_copy_closure_with_captures(
        |_t, _a, cap: &String, ctx| {
            Ok(JsPromise::resolve(JsString::from(cap.as_str()), ctx).into())
        },
        body.clone(),
    );
    // .json() — capture `body`, JSON.parse via serde_json, resolve/reject.
    let json_fn = NativeFunction::from_copy_closure_with_captures(
        |_t, _a, cap: &String, ctx| {
            Ok(match json_parse(cap, ctx) {
                Ok(v) => JsPromise::resolve(v, ctx),
                Err(e) => JsPromise::reject(e, ctx),
            }
            .into())
        },
        body,
    );

    let obj = ObjectInitializer::new(ctx)
        .property(js_string!("ok"), ok, Attribute::READONLY)
        .property(js_string!("status"), status as f64, Attribute::READONLY)
        .property(
            js_string!("statusText"),
            JsString::from(status_text),
            Attribute::READONLY,
        )
        .property(
            js_string!("url"),
            JsString::from(url.as_str()),
            Attribute::READONLY,
        )
        .property(js_string!("headers"), headers, Attribute::READONLY)
        .function(text_fn, js_string!("text"), 0)
        .function(json_fn, js_string!("json"), 0)
        .build();
    Ok(obj)
}

/// `.json()` — parse the captured body via `serde_json`, lift to a `JsValue`.
/// A malformed body → `Err` → the `.json()` promise rejects.
fn json_parse(text: &str, ctx: &mut Context) -> JsResult<JsValue> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| type_err(&format!("invalid JSON: {e}")))?;
    JsValue::from_json(&v, ctx)
}

/// A minimal `Headers` object with a single `get(name)` (case-insensitive). Only
/// `content-type` is ever populated (the loader surfaces just that); any other
/// name → `null`.
fn build_headers(content_type: &str, ctx: &mut Context) -> JsObject {
    let get = NativeFunction::from_copy_closure_with_captures(
        |_t, a, cap: &String, _ctx| {
            let name = a
                .first()
                .and_then(|v| v.as_string())
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default()
                .to_ascii_lowercase();
            Ok(if name == "content-type" && !cap.is_empty() {
                JsString::from(cap.as_str()).into()
            } else {
                JsValue::null()
            })
        },
        content_type.to_string(),
    );
    ObjectInitializer::new(ctx)
        .function(get, js_string!("get"), 1)
        .build()
}
