//! E8-M3 `localStorage` / `sessionStorage` — method-based, in-memory, per-render.
//!
//! Each store is a plain `JsObject` (built via `ObjectInitializer`) whose method
//! closures capture a shared `Rc<RefCell<Vec<(String,String)>>>` living in
//! [`DomState`] (mirroring the `console` sink pattern). The `Vec` is used as an
//! insertion-ordered map so `length`/`key(index)` are stable. Two independent
//! stores (`localStorage` ≠ `sessionStorage`); both are born + dropped with the
//! `DomState` of one `run_scripts`, so they reset every render automatically.
//!
//! Deferred (non-goals): named-property exotic access (`localStorage.foo` /
//! bracket / `delete`), persistence, quota, `storage` events.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsObject, JsString, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};

use super::DomState;

/// The insertion-ordered backing store for one Storage object.
pub(crate) type StorageMap = Rc<RefCell<Vec<(String, String)>>>;

/// A Trace-able newtype wrapping the shared store so it can be captured by
/// `NativeFunction::from_copy_closure_with_captures`. Holds only an
/// `Rc<RefCell<Vec<…>>>` (no `Gc` pointers), so the trace is empty.
#[derive(Clone, Finalize)]
struct Store(StorageMap);

// SAFETY: `Store` contains only an `Rc<RefCell<Vec<(String,String)>>>` — no Boa
// `Gc` pointers — so there is nothing for the collector to trace.
unsafe impl Trace for Store {
    boa_gc::empty_trace!();
}

/// Coerce an argument to a Rust `String` (missing arg → ""), per the IDL's
/// `DOMString` coercion. Reused for both `key` and `value`.
fn arg_str(args: &[JsValue], i: usize, ctx: &mut Context) -> boa_engine::JsResult<String> {
    match args.get(i) {
        Some(v) => Ok(v.to_string(ctx)?.to_std_string_escaped()),
        None => Ok(String::new()),
    }
}

/// Read the two storage backing `Rc`s out of `DomState` (clones, so the built
/// objects share the same `Vec`s the `DomState` holds). Returns `None` if the
/// DOM state is not installed (should not happen on the script path).
pub(crate) fn maps(ctx: &mut Context) -> Option<(StorageMap, StorageMap)> {
    let host = ctx.realm().host_defined();
    let state = host.get::<DomState>()?;
    Some((state.local_storage.clone(), state.session_storage.clone()))
}

/// Build a Storage object over `map`: `getItem`/`setItem`/`removeItem`/`clear`/
/// `key` methods + a read-only `length` accessor, each capturing a clone of the
/// shared store.
pub(crate) fn build_storage_object(map: StorageMap, ctx: &mut Context) -> JsObject {
    let mut init = ObjectInitializer::new(ctx);

    // getItem(key) -> string | null
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cap: &Store, ctx| {
                let key = arg_str(args, 0, ctx)?;
                let v = cap
                    .0
                    .borrow()
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.clone());
                Ok(match v {
                    Some(v) => JsString::from(v).into(),
                    None => JsValue::null(),
                })
            },
            Store(map.clone()),
        ),
        js_string!("getItem"),
        1,
    );

    // setItem(key, value) -> undefined  (BOTH coerced to string per the IDL)
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cap: &Store, ctx| {
                let key = arg_str(args, 0, ctx)?;
                let value = arg_str(args, 1, ctx)?;
                let mut store = cap.0.borrow_mut();
                match store.iter_mut().find(|(k, _)| *k == key) {
                    Some(entry) => entry.1 = value,
                    None => store.push((key, value)),
                }
                Ok(JsValue::undefined())
            },
            Store(map.clone()),
        ),
        js_string!("setItem"),
        2,
    );

    // removeItem(key) -> undefined
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cap: &Store, ctx| {
                let key = arg_str(args, 0, ctx)?;
                cap.0.borrow_mut().retain(|(k, _)| *k != key);
                Ok(JsValue::undefined())
            },
            Store(map.clone()),
        ),
        js_string!("removeItem"),
        1,
    );

    // clear() -> undefined
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, _args, cap: &Store, _ctx| {
                cap.0.borrow_mut().clear();
                Ok(JsValue::undefined())
            },
            Store(map.clone()),
        ),
        js_string!("clear"),
        0,
    );

    // key(index) -> string | null  (the index-th key in insertion order)
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cap: &Store, ctx| {
                let index = match args.first() {
                    Some(v) => v.to_u32(ctx)? as usize,
                    None => 0,
                };
                let k = cap.0.borrow().get(index).map(|(k, _)| k.clone());
                Ok(match k {
                    Some(k) => JsString::from(k).into(),
                    None => JsValue::null(),
                })
            },
            Store(map.clone()),
        ),
        js_string!("key"),
        1,
    );

    // length accessor (read-only): the number of stored entries.
    let len_get = NativeFunction::from_copy_closure_with_captures(
        |_t, _a, cap: &Store, _ctx| Ok(JsValue::from(cap.0.borrow().len() as u32)),
        Store(map),
    );
    let realm = init.context().realm().clone();
    let len_get = FunctionObjectBuilder::new(&realm, len_get).build();
    init.accessor(js_string!("length"), Some(len_get), None, Attribute::all());

    init.build()
}
