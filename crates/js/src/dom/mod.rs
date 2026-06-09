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
    js_string, Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsValue,
    NativeFunction, Trace,
};
use starfish_dom::{Document, NodeId};
use starfish_net::{ResourceLoader, Url};

pub(crate) mod computed;
mod dataset;
mod document;
pub(crate) mod event;
pub(crate) mod fetch;
pub(crate) mod geometry;
mod node;
mod select;
pub(crate) mod storage;
mod style;
pub(crate) mod timer;
pub(crate) mod url;
pub(crate) mod xhr;

pub(crate) type SharedDoc = Rc<RefCell<Document>>;

/// Reserved listener-registry key for `window` (no real `NodeId` reaches
/// `u32::MAX`; the arena is far smaller). `document` keys under the arena root
/// (index 0). Keeps `window.addEventListener('load', …)` distinct from
/// `document.addEventListener('DOMContentLoaded', …)`.
pub(crate) const WINDOW_KEY: u32 = u32::MAX;

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

    // --- E4-M3 ---
    /// `(NodeId.index() | WINDOW_KEY, event type) -> ordered listeners`. The
    /// `JsObject`s are callables; GC-traced (NOT ignored) so they stay rooted
    /// across the run-to-quiescence loop (a listener registered in a script must
    /// survive until `load` fires).
    pub listeners: boa_gc::GcRefCell<HashMap<(u32, String), Vec<JsObject>>>,
    /// The bounded virtual-time timer queue (§3). Holds callable `JsObject`s →
    /// traced, so a `setTimeout`'d closure isn't collected before it runs.
    pub timers: boa_gc::GcRefCell<timer::TimerQueue>,

    /// E8-M1: author stylesheets collected BEFORE scripts, for the on-demand
    /// `getComputedStyle` cascade. Plain data → ignored by the GC trace.
    #[unsafe_ignore_trace]
    pub author_sheets: Rc<Vec<starfish_css::Stylesheet>>,

    // --- E8-M2: fetch / XMLHttpRequest networking ---
    /// The document base URL (owned copy) for resolving `fetch`/XHR relative
    /// URLs. Plain data → ignored by the GC trace.
    #[unsafe_ignore_trace]
    pub base: Url,
    /// A raw pointer to the `&dyn ResourceLoader` borrowed by `run_scripts`,
    /// wrapped in a `Cell` for interior-mutable clearing. VALID only for the
    /// duration of one `run_scripts` call: set at `install` time (where the
    /// borrow is in scope) and nulled before `run_scripts` returns. It is
    /// dereferenced ONLY by [`loader_and_base`] (the single `unsafe` choke
    /// point), called exclusively from the `fetch`/XHR native fns — which run
    /// only inside `eval` + `run_to_quiescence` of that same `run_scripts`, so
    /// the pointee borrow is always still alive. The pointer never escapes
    /// `DomState`, is never copied into a longer-lived value, and is null after
    /// the call. A `Cell<*const _>` is plain data → ignored by the GC trace.
    #[unsafe_ignore_trace]
    pub loader: std::cell::Cell<*const (dyn ResourceLoader + 'static)>,

    // --- E8-M3: in-memory storage ---
    /// The per-render `localStorage` backing store (insertion-ordered
    /// string→string). Plain data (no Gc pointers) → ignored by the GC trace.
    /// Born + dropped with this `DomState` (one per `run_scripts`), so it is
    /// naturally per-render: no persistence, cleared automatically each render.
    #[unsafe_ignore_trace]
    pub local_storage: Rc<RefCell<Vec<(String, String)>>>,
    /// The `sessionStorage` backing store — an INDEPENDENT map, identical
    /// implementation (differs from `localStorage` only in identity).
    #[unsafe_ignore_trace]
    pub session_storage: Rc<RefCell<Vec<(String, String)>>>,

    /// E11-M3: getComputedStyle styled-tree memoization. style_tree is a pure
    /// function of (doc, author_sheets); author_sheets is frozen during script
    /// execution, so a cache keyed by the DOM mutation version is valid — when the
    /// version is unchanged the cached tree equals a fresh rebuild (byte-identical).
    #[unsafe_ignore_trace]
    pub styled_cache: RefCell<Option<(u64, starfish_style::StyledTree)>>,

    /// E19-M2: the render viewport width (px), for the on-demand geometry layout
    /// (`getBoundingClientRect`/`offsetWidth`/etc). Plain data → ignored by GC.
    #[unsafe_ignore_trace]
    pub viewport_width: f32,
    /// E19-M2: on-demand layout memoization, mirroring `styled_cache`. layout is a
    /// pure (read-only) function of (doc, author_sheets, viewport_width); keyed by
    /// the DOM mutation version so an unchanged DOM reuses the cached box tree. The
    /// styled tree is held alongside the box tree because `LayoutBox::style`
    /// resolves through it. All queries are read-only → never bumps the version,
    /// so the post-script render is byte-identical.
    #[unsafe_ignore_trace]
    pub layout_cache:
        RefCell<Option<(u64, starfish_style::StyledTree, starfish_layout::LayoutBox)>>,
}

/// The loader + base for a synchronous `fetch`/XHR call.
///
/// # Safety
///
/// `state.loader` was set from the `&dyn ResourceLoader` borrowed by the
/// enclosing `run_scripts` call and is cleared (nulled) before that call
/// returns. Every `fetch`/XHR native fn runs *inside* that same `run_scripts`
/// (during script `eval` + `run_to_quiescence`), so the pointee borrow is still
/// alive here. The reborrowed reference does not outlive this function — the
/// caller resolves the URL, calls `loader.fetch(&url)` immediately, and never
/// stashes it. Returns `None` if the pointer was already cleared (defensive).
pub(crate) fn loader_and_base(ctx: &mut Context) -> Option<(&'static dyn ResourceLoader, Url)> {
    let host = ctx.realm().host_defined();
    let state = host.get::<DomState>()?;
    let ptr = state.loader.get();
    if ptr.is_null() {
        return None;
    }
    // SAFETY: see the function-level doc comment. `ptr` is non-null and points
    // at a borrow that is provably still alive for the duration of this call.
    let loader: &'static dyn ResourceLoader = unsafe { &*ptr };
    Some((loader, state.base.clone()))
}

/// Null the loader pointer in `DomState` (called by `run_scripts` before it
/// returns, so no later code can ever observe a stale pointer).
pub(crate) fn clear_loader(ctx: &mut Context) {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        state.loader.set(std::ptr::null::<crate::dom::NullLoader>());
    }
}

/// Zero-sized placeholder type only used to spell a typed null pointer for the
/// `*const dyn ResourceLoader` field. Never instantiated.
pub(crate) struct NullLoader;
impl ResourceLoader for NullLoader {
    fn fetch(&self, _url: &Url) -> Result<starfish_net::Resource, starfish_net::LoadError> {
        unreachable!("NullLoader is never instantiated")
    }
}

/// Build a `TypeError` `JsError` with `msg` (the Fetch spec rejects with a
/// `TypeError`).
pub(crate) fn type_err(msg: &str) -> JsError {
    JsError::from_native(JsNativeError::typ().with_message(msg.to_string()))
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

/// The listener-registry key for a dispatch target: a node uses its arena
/// index. (`window`'s methods hard-code `WINDOW_KEY` instead of calling this.)
pub(crate) fn event_target_index(h: &NodeHandle) -> u32 {
    h.id.index()
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
pub(crate) fn install(
    ctx: &mut Context,
    shared: &SharedDoc,
    base: &Url,
    loader: &dyn ResourceLoader,
    sheets: Rc<Vec<starfish_css::Stylesheet>>,
    viewport_width: f32,
) -> JsResult<JsObject> {
    ctx.register_global_class::<NodeHandle>()?;
    ctx.register_global_class::<xhr::Xhr>()?;
    ctx.register_global_class::<url::UrlClass>()?;
    ctx.register_global_class::<url::UrlSearchParams>()?;
    ctx.realm().host_defined_mut().insert(DomState {
        doc: shared.clone(),
        cache: boa_gc::GcRefCell::new(HashMap::new()),
        listeners: boa_gc::GcRefCell::new(HashMap::new()),
        timers: boa_gc::GcRefCell::new(timer::TimerQueue::default()),
        author_sheets: sheets,
        base: base.clone(),
        // The `&dyn ResourceLoader` borrow becomes a raw pointer with its borrow
        // lifetime erased to `'static`. Newer rustc forbids extending a trait-
        // object pointer's lifetime via a plain `as` cast, so we transmute the
        // (same-layout) raw pointer. Its *use* is hand-bounded to this
        // `run_scripts` call (cleared via `clear_loader` before return); see the
        // field doc + §1.4 of the design note for the full safety argument.
        // SAFETY: a `*const dyn` and a `*const (dyn + 'static)` are identical in
        // layout (a data+vtable fat pointer); only the lifetime token differs,
        // and that lifetime is enforced by hand, not by the type system.
        loader: std::cell::Cell::new(unsafe {
            std::mem::transmute::<*const dyn ResourceLoader, *const (dyn ResourceLoader + 'static)>(
                loader as *const dyn ResourceLoader,
            )
        }),
        local_storage: Rc::new(RefCell::new(Vec::new())),
        session_storage: Rc::new(RefCell::new(Vec::new())),
        styled_cache: RefCell::new(None),
        viewport_width,
        layout_cache: RefCell::new(None),
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
