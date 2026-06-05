//! E4-M2 DOM bindings: `document` / `Node` / `Element` host objects backed by
//! the real `starfish-dom` arena. A DOM node is a Boa `JsObject` carrying a
//! [`NodeHandle`] (the shared arena + a `NodeId`) as native data, with the
//! registered `Node` class prototype supplying the methods. Methods downcast
//! `this`, borrow the arena, read/mutate it, and return wrappers (minted via the
//! [`DomState`] identity cache so the same node always yields the same object).
//!
//! Scope is the pragmatic subset from `docs/design/E4-M2.md`: read + mutate the
//! tree, attributes, classList, textContent, `style` (inline), querySelector*.
//! Deferred: innerHTML write, live NodeLists, events, MutationObserver.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::{
    js_string, Context, Finalize, JsData, JsNativeError, JsObject, JsResult, JsValue, NativeFunction,
    Trace,
};
use starfish_dom::{Document, NodeId};

mod document;
mod node;
mod select;
mod style;

pub(crate) type SharedDoc = Rc<RefCell<Document>>;

/// Native data carried by every DOM wrapper object: the shared arena + which
/// node it represents. `NodeId` is a `Copy` u32 index; the `Rc` clone is cheap.
/// Holds no Boa `Gc` pointers, so every field is `unsafe_ignore_trace`d.
#[derive(Trace, Finalize, JsData, Clone)]
pub(crate) struct NodeHandle {
    #[unsafe_ignore_trace]
    pub shared: SharedDoc,
    #[unsafe_ignore_trace]
    pub id: NodeId,
}

/// Realm host-defined state: the shared arena + the `NodeId -> wrapper` identity
/// cache. The cache holds `JsObject`s (GC pointers) and is therefore *traced*
/// (NOT ignored) so the cached wrappers stay rooted while the script runs.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct DomState {
    #[unsafe_ignore_trace]
    pub doc: SharedDoc,
    /// `NodeId.index() -> wrapper`. Keyed by the raw `u32` (which is `Trace`)
    /// so the `HashMap` can be GC-traced, rooting the cached wrappers.
    pub cache: boa_gc::GcRefCell<HashMap<u32, JsObject>>,
}

impl NodeHandle {
    /// Downcast `this` to a `NodeHandle`, throwing `TypeError` otherwise.
    pub(crate) fn from_this(this: &JsValue) -> JsResult<NodeHandle> {
        this.as_object()
            .and_then(|o| o.downcast_ref::<NodeHandle>().map(|h| h.clone()))
            .ok_or_else(|| JsNativeError::typ().with_message("not a DOM node").into())
    }
}

/// The `Node` class. One class serves all node kinds (Document / Element / Text
/// / Comment); methods that only make sense on elements check the node kind and
/// return `null`/throw as appropriate. Registering it installs the prototype
/// `wrap_node` / `from_data` attach to every wrapper.
impl Class for NodeHandle {
    const NAME: &'static str = "Node";
    const LENGTH: usize = 0;

    fn data_constructor(
        _new_target: &JsValue,
        _args: &[JsValue],
        _context: &mut Context,
    ) -> JsResult<Self> {
        // DOM nodes are not JS-constructible (`new Node()` is invalid).
        Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        node::init(class);
        document::init(class);
        Ok(())
    }
}

/// Look up (or mint + cache) the wrapper for `id`. Identity holds: the same
/// `NodeId` always returns the same `JsObject`. Never panics.
pub(crate) fn wrap_node(id: NodeId, ctx: &mut Context) -> JsResult<JsObject> {
    let key = id.index();
    // Cache hit? (a short borrow of host-defined, dropped before from_data).
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(obj) = state.cache.borrow().get(&key) {
            return Ok(obj.clone());
        }
    }
    let shared = doc_state_shared(ctx)?;
    let obj = NodeHandle::from_data(NodeHandle { shared, id }, ctx)?;
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        state.cache.borrow_mut().insert(key, obj.clone());
    }
    Ok(obj)
}

/// `wrap_node` returning a `JsValue` (the wrapper, or `null` for `None`).
pub(crate) fn wrap_opt(id: Option<NodeId>, ctx: &mut Context) -> JsResult<JsValue> {
    match id {
        Some(id) => Ok(wrap_node(id, ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}

/// The shared arena from realm host-defined state.
pub(crate) fn doc_state_shared(ctx: &mut Context) -> JsResult<SharedDoc> {
    ctx.realm()
        .host_defined()
        .get::<DomState>()
        .map(|s| s.doc.clone())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("DOM not initialized")
                .into()
        })
}

/// Install the DOM bindings: register the `Node` class, seed the host-defined
/// `DomState` (arena + cache), and return the `document` wrapper object (the
/// `NodeKind::Document` root) for `globals::install` to expose as `document`.
pub(crate) fn install(ctx: &mut Context, shared: &SharedDoc) -> JsResult<JsObject> {
    ctx.register_global_class::<NodeHandle>()?;
    ctx.realm().host_defined_mut().insert(DomState {
        doc: shared.clone(),
        cache: boa_gc::GcRefCell::new(HashMap::new()),
    });
    let root = shared.borrow().root();
    wrap_node(root, ctx)
}

/// Helper: a native method bound by `ClassBuilder::method`.
pub(crate) fn method(
    class: &mut ClassBuilder<'_>,
    name: &str,
    len: usize,
    f: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
) {
    class.method(js_string!(name), len, NativeFunction::from_fn_ptr(f));
}

type NativeFn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// Helper: a getter (and optional setter) accessor on the class prototype, so
/// it re-resolves against the live (possibly mutated) tree on each access.
pub(crate) fn accessor(
    class: &mut ClassBuilder<'_>,
    name: &str,
    get: NativeFn,
    set: Option<NativeFn>,
) {
    let realm = class.context().realm().clone();
    let getter = NativeFunction::from_fn_ptr(get).to_js_function(&realm);
    let setter = set.map(|s| NativeFunction::from_fn_ptr(s).to_js_function(&realm));
    class.accessor(
        js_string!(name),
        Some(getter),
        setter,
        boa_engine::property::Attribute::all(),
    );
}
