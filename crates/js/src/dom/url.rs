//! E8-M3 `URL` / `URLSearchParams` — native classes over the `url` crate.
//!
//! Both are JS-constructible (`new URL(...)`, `new URLSearchParams(...)`); each
//! `data_constructor` parses its args into instance data. `URL` is read-only in
//! M3 (accessors only, no setters); its `searchParams` is a fresh snapshot of the
//! query (not live — mutating it does not write back). `URLSearchParams` accepts
//! a string init only; `set` is remove-then-append; no iterator protocol (but
//! `forEach` is provided). Encoding is delegated to `url::Url`.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace,
};

use super::node::arg_str;
use super::{accessor, method, type_err};

// --- URL ---

/// A parsed WHATWG URL. Holds no GC pointers → `unsafe_ignore_trace`.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct UrlClass {
    #[unsafe_ignore_trace]
    url: starfish_net::Url,
}

impl Class for UrlClass {
    const NAME: &'static str = "URL";
    const LENGTH: usize = 1;

    fn data_constructor(_nt: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<Self> {
        let input = arg_str(args, 0, ctx)?;
        let parsed = match args.get(1) {
            Some(v) if !v.is_undefined() && !v.is_null() => {
                let base_str = v.to_string(ctx)?.to_std_string_escaped();
                let base = starfish_net::Url::parse(&base_str)
                    .map_err(|e| type_err(&format!("Invalid base URL: {e}")))?;
                base.join(&input)
            }
            _ => starfish_net::Url::parse(&input),
        };
        let url = parsed.map_err(|e| type_err(&format!("Invalid URL: {e}")))?;
        Ok(UrlClass { url })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        accessor(class, "href", get_href, None);
        accessor(class, "protocol", get_protocol, None);
        accessor(class, "host", get_host, None);
        accessor(class, "hostname", get_hostname, None);
        accessor(class, "port", get_port, None);
        accessor(class, "pathname", get_pathname, None);
        accessor(class, "search", get_search, None);
        accessor(class, "hash", get_hash, None);
        accessor(class, "origin", get_origin, None);
        accessor(class, "searchParams", get_search_params, None);
        method(class, "toString", 0, url_to_string);
        Ok(())
    }
}

/// Run `f` with a borrow of `this`'s `UrlClass`, throwing `TypeError` otherwise.
fn with_url<R>(this: &JsValue, f: impl FnOnce(&starfish_net::Url) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URL"))?;
    let data = obj
        .downcast_ref::<UrlClass>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URL"))?;
    Ok(f(&data.url))
}

fn get_href(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| JsString::from(u.as_str()).into())
}

fn get_protocol(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| JsString::from(format!("{}:", u.scheme())).into())
}

fn get_host(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| {
        let s = match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            (None, _) => String::new(),
        };
        JsString::from(s).into()
    })
}

fn get_hostname(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| JsString::from(u.host_str().unwrap_or("")).into())
}

fn get_port(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| {
        let s = u.port().map(|p| p.to_string()).unwrap_or_default();
        JsString::from(s).into()
    })
}

fn get_pathname(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| JsString::from(u.path()).into())
}

fn get_search(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| {
        let s = match u.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        };
        JsString::from(s).into()
    })
}

fn get_hash(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| {
        let s = match u.fragment() {
            Some(f) => format!("#{f}"),
            None => String::new(),
        };
        JsString::from(s).into()
    })
}

fn get_origin(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| {
        JsString::from(u.origin().ascii_serialization()).into()
    })
}

fn url_to_string(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    with_url(this, |u| JsString::from(u.as_str()).into())
}

/// `searchParams` — a FRESH `URLSearchParams` snapshot of the URL's query (not
/// live; mutating it does not write back).
fn get_search_params(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let query = with_url(this, |u| u.query().unwrap_or("").to_string())?;
    let usp = UrlSearchParams {
        pairs: parse_query(&query),
    };
    Ok(UrlSearchParams::from_data(usp, ctx)?.into())
}

// --- URLSearchParams ---

/// Ordered `(name, value)` query pairs. Holds no GC pointers.
#[derive(Trace, Finalize, JsData, Default)]
pub(crate) struct UrlSearchParams {
    #[unsafe_ignore_trace]
    pairs: Vec<(String, String)>,
}

impl Class for UrlSearchParams {
    const NAME: &'static str = "URLSearchParams";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<Self> {
        // M3 accepts a STRING init only (record/array/iterable deferred).
        let init = arg_str(args, 0, ctx)?;
        Ok(UrlSearchParams {
            pairs: parse_query(&init),
        })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "get", 1, usp_get);
        method(class, "getAll", 1, usp_get_all);
        method(class, "has", 1, usp_has);
        method(class, "set", 2, usp_set);
        method(class, "append", 2, usp_append);
        method(class, "delete", 1, usp_delete);
        method(class, "toString", 0, usp_to_string);
        method(class, "forEach", 1, usp_for_each);
        Ok(())
    }
}

/// Parse `"a=1&b=2"` (optional leading `?`) into ordered `(name, value)` pairs
/// via a throwaway URL (so percent-decoding + `+`→space matches the platform,
/// no new dep).
fn parse_query(s: &str) -> Vec<(String, String)> {
    let q = s.strip_prefix('?').unwrap_or(s);
    if q.is_empty() {
        return Vec::new();
    }
    match starfish_net::Url::parse(&format!("http://x/?{q}")) {
        Ok(u) => u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Serialize ordered pairs back to `application/x-www-form-urlencoded` via a
/// throwaway URL (matching the platform's encoding).
fn serialize_pairs(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let mut u = match starfish_net::Url::parse("http://x/") {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    u.query_pairs_mut().clear().extend_pairs(pairs.iter());
    u.query().unwrap_or("").to_string()
}

fn with_usp<R>(this: &JsValue, f: impl FnOnce(&UrlSearchParams) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URLSearchParams"))?;
    let data = obj
        .downcast_ref::<UrlSearchParams>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URLSearchParams"))?;
    Ok(f(&data))
}

fn with_usp_mut<R>(this: &JsValue, f: impl FnOnce(&mut UrlSearchParams) -> R) -> JsResult<R> {
    let obj = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URLSearchParams"))?;
    let mut data = obj
        .downcast_mut::<UrlSearchParams>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a URLSearchParams"))?;
    Ok(f(&mut data))
}

fn usp_get(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_usp(this, |p| {
        match p.pairs.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => JsString::from(v.as_str()).into(),
            None => JsValue::null(),
        }
    })
}

fn usp_get_all(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let values: Vec<JsValue> = with_usp(this, |p| {
        p.pairs
            .iter()
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| JsString::from(v.as_str()).into())
            .collect()
    })?;
    Ok(JsArray::from_iter(values, ctx).into())
}

fn usp_has(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_usp(this, |p| {
        JsValue::from(p.pairs.iter().any(|(k, _)| *k == name))
    })
}

fn usp_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let value = arg_str(args, 1, ctx)?;
    with_usp_mut(this, |p| {
        p.pairs.retain(|(k, _)| *k != name);
        p.pairs.push((name, value));
    })?;
    Ok(JsValue::undefined())
}

fn usp_append(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    let value = arg_str(args, 1, ctx)?;
    with_usp_mut(this, |p| p.pairs.push((name, value)))?;
    Ok(JsValue::undefined())
}

fn usp_delete(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_str(args, 0, ctx)?;
    with_usp_mut(this, |p| p.pairs.retain(|(k, _)| *k != name))?;
    Ok(JsValue::undefined())
}

fn usp_to_string(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let s = with_usp(this, |p| serialize_pairs(&p.pairs))?;
    Ok(JsString::from(s).into())
}

/// `forEach(cb)` — call `cb(value, name, this)` for each pair in order.
fn usp_for_each(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let cb = args
        .first()
        .and_then(|v| v.as_object())
        .filter(|o| o.is_callable())
        .ok_or_else(|| JsNativeError::typ().with_message("forEach callback is not a function"))?;
    // Snapshot the pairs so the callback can mutate the params without aliasing.
    let pairs = with_usp(this, |p| p.pairs.clone())?;
    for (k, v) in pairs {
        let args = [
            JsString::from(v).into(),
            JsString::from(k).into(),
            this.clone(),
        ];
        cb.call(&JsValue::undefined(), &args, ctx)?;
    }
    Ok(JsValue::undefined())
}
