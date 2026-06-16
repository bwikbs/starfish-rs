//! E52-M2 DOM traversal: `document.createNodeIterator` / `createTreeWalker`.
//!
//! Both walk the live arena (read-only) honoring a `whatToShow` bitmask + an
//! optional filter (a function, or an object with `acceptNode`). A node is shown
//! when `(whatToShow & (1 << (nodeType-1))) != 0`; the filter then returns
//! `FILTER_ACCEPT`(1) / `FILTER_REJECT`(2) / `FILTER_SKIP`(3). MVP: REJECT is
//! treated as SKIP (the node is skipped, its subtree is NOT pruned differently).
//!
//! State lives in a GC-traced registry on [`DomState`] (mirroring the observer
//! registries) so a stored filter `JsObject` stays rooted; each instance carries
//! only its registry id. `NodeIterator`/`TreeWalker` are non-constructible Boa
//! classes — minted exclusively by the two `document` factory methods.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::{
    js_string, Context, Finalize, JsData, JsNativeError, JsObject, JsResult, JsValue, Trace,
};
use starfish_dom::{NodeId, NodeKind};

use super::{method, wrap_node, DomState, NodeHandle, SharedDoc};

// --- NodeFilter constants -------------------------------------------------

pub(crate) const SHOW_ALL: u32 = 0xFFFF_FFFF;
pub(crate) const SHOW_ELEMENT: u32 = 0x1;
pub(crate) const SHOW_TEXT: u32 = 0x4;
pub(crate) const SHOW_COMMENT: u32 = 0x80;

pub(crate) const FILTER_ACCEPT: i32 = 1;
pub(crate) const FILTER_REJECT: i32 = 2;
pub(crate) const FILTER_SKIP: i32 = 3;

// --- registry -------------------------------------------------------------

/// One live walker (shared by `NodeIterator` + `TreeWalker`). `filter` is traced
/// to keep a stored callable/`acceptNode` object rooted; the node ids are plain
/// data (`NodeId.index()`).
#[derive(Trace, Finalize)]
pub(crate) struct WalkerState {
    #[unsafe_ignore_trace]
    pub id: u32,
    #[unsafe_ignore_trace]
    pub root: u32,
    #[unsafe_ignore_trace]
    pub what_to_show: u32,
    pub filter: Option<JsObject>,
    /// Current position. For `TreeWalker` this is `currentNode`. For
    /// `NodeIterator` it is the reference node (paired with `before`).
    #[unsafe_ignore_trace]
    pub current: u32,
    /// `NodeIterator` only: whether the pointer is *before* the reference node.
    #[unsafe_ignore_trace]
    pub before: bool,
}

/// All live walkers + the next id to hand out. Traced (holds filter objects).
#[derive(Trace, Finalize, Default)]
pub(crate) struct WalkerRegistry {
    pub walkers: Vec<WalkerState>,
    #[unsafe_ignore_trace]
    pub next_id: u32,
}

/// A snapshot of one walker's plain (non-GC) fields.
#[derive(Clone, Copy)]
struct WalkerSnap {
    root: u32,
    what_to_show: u32,
    current: u32,
    before: bool,
}

/// Read a walker's plain fields by id.
fn snap(ctx: &mut Context, id: u32) -> Option<WalkerSnap> {
    let host = ctx.realm().host_defined();
    let state = host.get::<DomState>()?;
    let reg = state.walkers.borrow();
    reg.walkers.iter().find(|w| w.id == id).map(|w| WalkerSnap {
        root: w.root,
        what_to_show: w.what_to_show,
        current: w.current,
        before: w.before,
    })
}

/// Clone a walker's stored filter object by id (if any).
fn filter_of(ctx: &mut Context, id: u32) -> Option<JsObject> {
    let host = ctx.realm().host_defined();
    let state = host.get::<DomState>()?;
    let reg = state.walkers.borrow();
    reg.walkers.iter().find(|w| w.id == id).and_then(|w| w.filter.clone())
}

/// Update a walker's `current` (+ `before`) position by id.
fn set_pos(ctx: &mut Context, id: u32, current: u32, before: bool) {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(w) = state.walkers.borrow_mut().walkers.iter_mut().find(|w| w.id == id) {
            w.current = current;
            w.before = before;
        }
    }
}

// --- filtering ------------------------------------------------------------

fn node_type(kind: &NodeKind) -> u32 {
    match kind {
        NodeKind::Element(_) => 1,
        NodeKind::Text(_) => 3,
        NodeKind::Comment(_) => 8,
        NodeKind::Doctype(_) => 10,
        NodeKind::Document => 9,
        NodeKind::ShadowRoot(_) => 11,
    }
}

/// `whatToShow` bitmask test: a node is shown when bit `(nodeType-1)` is set.
fn passes_what_to_show(shared: &SharedDoc, id: NodeId, what_to_show: u32) -> bool {
    let nt = node_type(shared.borrow().kind(id));
    if nt == 0 {
        return false;
    }
    (what_to_show & (1u32 << (nt - 1))) != 0
}

/// Invoke the filter (function or `{acceptNode}`) on `id`, returning the
/// `FILTER_*` code. A missing/invalid filter accepts. MVP: REJECT == SKIP, so a
/// non-ACCEPT code is reported as SKIP to callers.
fn run_filter(ctx: &mut Context, walker_id: u32, id: NodeId) -> JsResult<i32> {
    let Some(filter) = filter_of(ctx, walker_id) else {
        return Ok(FILTER_ACCEPT);
    };
    let node = JsValue::from(wrap_node(id, ctx)?);
    let result = if filter.is_callable() {
        filter.call(&JsValue::undefined(), &[node], ctx)?
    } else {
        let accept = filter.get(js_string!("acceptNode"), ctx)?;
        match accept.as_object().filter(|o| o.is_callable()) {
            Some(f) => f.call(&JsValue::from(filter.clone()), &[node], ctx)?,
            None => return Ok(FILTER_ACCEPT),
        }
    };
    result.to_i32(ctx)
}

/// Combined gate: whatToShow first (a non-shown node is SKIP, never runs the
/// filter), then the filter. Returns the `FILTER_*` code. MVP treats REJECT as
/// SKIP, so only ACCEPT vs not-ACCEPT matters to callers.
fn accept(ctx: &mut Context, walker_id: u32, what_to_show: u32, id: NodeId) -> JsResult<i32> {
    let shared = {
        let host = ctx.realm().host_defined();
        match host.get::<DomState>() {
            Some(s) => s.doc.clone(),
            None => return Ok(FILTER_SKIP),
        }
    };
    if !passes_what_to_show(&shared, id, what_to_show) {
        return Ok(FILTER_SKIP);
    }
    run_filter(ctx, walker_id, id)
}

// --- arena document-order navigation (unfiltered) -------------------------

/// Next node in document order within `root`'s subtree (descend → next sibling
/// → ascend). `None` past the last descendant of `root`.
fn doc_next(shared: &SharedDoc, root: NodeId, id: NodeId) -> Option<NodeId> {
    let doc = shared.borrow();
    if let Some(c) = doc.first_child(id) {
        return Some(c);
    }
    let mut cur = id;
    loop {
        if cur == root {
            return None;
        }
        if let Some(s) = doc.next_sibling(cur) {
            return Some(s);
        }
        cur = doc.parent(cur)?;
    }
}

/// Previous node in document order within `root`'s subtree (previous sibling's
/// deepest-last descendant → parent). `None` at `root` itself.
fn doc_prev(shared: &SharedDoc, root: NodeId, id: NodeId) -> Option<NodeId> {
    if id == root {
        return None;
    }
    let doc = shared.borrow();
    if let Some(mut prev) = doc.prev_sibling(id) {
        // Descend to the deepest last child of the previous sibling.
        while let Some(last) = doc.last_child(prev) {
            prev = last;
        }
        return Some(prev);
    }
    doc.parent(id)
}

// --- NodeIterator ---------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
pub(crate) struct NodeIterator {
    #[unsafe_ignore_trace]
    id: u32,
}

impl Class for NodeIterator {
    const NAME: &'static str = "NodeIterator";
    const LENGTH: usize = 0;

    fn data_constructor(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "nextNode", 0, iter_next);
        method(class, "previousNode", 0, iter_prev);
        Ok(())
    }
}

fn iter_id(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<NodeIterator>().map(|m| m.id))
        .ok_or_else(|| JsNativeError::typ().with_message("not a NodeIterator").into())
}

/// `nextNode()` — advance the iterator forward through the filtered flat order.
fn iter_next(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = iter_id(this)?;
    iter_traverse(ctx, id, true)
}

/// `previousNode()` — advance the iterator backward.
fn iter_prev(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = iter_id(this)?;
    iter_traverse(ctx, id, false)
}

/// The NodeIterator traversal core (DOM "traverse" algorithm). Walks the flat
/// document order from the reference node in `forward` direction, returning the
/// first accepted node and updating (reference node, pointerBeforeReferenceNode).
fn iter_traverse(ctx: &mut Context, walker_id: u32, forward: bool) -> JsResult<JsValue> {
    let Some(s) = snap(ctx, walker_id) else {
        return Ok(JsValue::null());
    };
    let shared = match ctx.realm().host_defined().get::<DomState>() {
        Some(st) => st.doc.clone(),
        None => return Ok(JsValue::null()),
    };
    let root = NodeId::from_index(s.root);
    let mut node = NodeId::from_index(s.current);
    let mut before = s.before;

    loop {
        if forward {
            if before {
                // Pointer was before `node`: `node` itself is the next candidate.
                before = false;
            } else {
                match doc_next(&shared, root, node) {
                    Some(n) => node = n,
                    None => return Ok(JsValue::null()),
                }
            }
        } else if !before {
            // Pointer was after `node`: `node` itself is the previous candidate.
            before = true;
        } else {
            match doc_prev(&shared, root, node) {
                Some(n) => node = n,
                None => return Ok(JsValue::null()),
            }
        }
        if accept(ctx, walker_id, s.what_to_show, node)? == FILTER_ACCEPT {
            set_pos(ctx, walker_id, node.index(), before);
            return Ok(wrap_node(node, ctx)?.into());
        }
    }
}

// --- TreeWalker -----------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
pub(crate) struct TreeWalker {
    #[unsafe_ignore_trace]
    id: u32,
}

impl Class for TreeWalker {
    const NAME: &'static str = "TreeWalker";
    const LENGTH: usize = 0;

    fn data_constructor(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        super::accessor(class, "currentNode", tw_current_get, None);
        method(class, "nextNode", 0, tw_next);
        method(class, "previousNode", 0, tw_prev);
        method(class, "parentNode", 0, tw_parent);
        method(class, "firstChild", 0, tw_first_child);
        method(class, "lastChild", 0, tw_last_child);
        method(class, "nextSibling", 0, tw_next_sibling);
        method(class, "previousSibling", 0, tw_prev_sibling);
        Ok(())
    }
}

fn tw_id(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<TreeWalker>().map(|m| m.id))
        .ok_or_else(|| JsNativeError::typ().with_message("not a TreeWalker").into())
}

fn tw_current_get(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    match snap(ctx, id) {
        Some(s) => Ok(wrap_node(NodeId::from_index(s.current), ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}

/// First child of `id` in the arena (any node kind).
fn first_child(shared: &SharedDoc, id: NodeId) -> Option<NodeId> {
    shared.borrow().first_child(id)
}
fn last_child(shared: &SharedDoc, id: NodeId) -> Option<NodeId> {
    shared.borrow().last_child(id)
}
fn next_sibling(shared: &SharedDoc, id: NodeId) -> Option<NodeId> {
    shared.borrow().next_sibling(id)
}
fn prev_sibling(shared: &SharedDoc, id: NodeId) -> Option<NodeId> {
    shared.borrow().prev_sibling(id)
}
fn parent(shared: &SharedDoc, id: NodeId) -> Option<NodeId> {
    shared.borrow().parent(id)
}

fn tw_shared(ctx: &mut Context) -> Option<SharedDoc> {
    ctx.realm().host_defined().get::<DomState>().map(|s| s.doc.clone())
}

/// `firstChild()` — first accepted child of `currentNode`; moves there if found.
/// (REJECT==SKIP MVP: a skipped child's children are searched in document order.)
fn tw_first_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    tw_child(this, ctx, true)
}
fn tw_last_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    tw_child(this, ctx, false)
}

/// Child traversal (first/last) per the DOM TreeWalker algorithm, simplified for
/// REJECT==SKIP: descend into a skipped child's own children before moving on.
fn tw_child(this: &JsValue, ctx: &mut Context, first: bool) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = tw_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let cur = NodeId::from_index(s.current);
    let mut child = if first {
        first_child(&shared, cur)
    } else {
        last_child(&shared, cur)
    };
    while let Some(node) = child {
        if accept(ctx, id, s.what_to_show, node)? == FILTER_ACCEPT {
            set_pos(ctx, id, node.index(), false);
            return Ok(wrap_node(node, ctx)?.into());
        }
        // Skipped: descend into its children (document order) before its sibling.
        let descend = if first {
            first_child(&shared, node)
        } else {
            last_child(&shared, node)
        };
        if descend.is_some() {
            child = descend;
        } else {
            child = if first {
                next_sibling(&shared, node)
            } else {
                prev_sibling(&shared, node)
            };
        }
    }
    Ok(JsValue::null())
}

/// `nextSibling()` — next accepted sibling of `currentNode`; moves there if found.
fn tw_next_sibling(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    tw_sibling(this, ctx, true)
}
fn tw_prev_sibling(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    tw_sibling(this, ctx, false)
}

fn tw_sibling(this: &JsValue, ctx: &mut Context, forward: bool) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = tw_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let root = NodeId::from_index(s.root);
    let mut node = NodeId::from_index(s.current);
    loop {
        if node == root {
            return Ok(JsValue::null());
        }
        let sib = if forward {
            next_sibling(&shared, node)
        } else {
            prev_sibling(&shared, node)
        };
        match sib {
            Some(n) => {
                if accept(ctx, id, s.what_to_show, n)? == FILTER_ACCEPT {
                    set_pos(ctx, id, n.index(), false);
                    return Ok(wrap_node(n, ctx)?.into());
                }
                // Skipped: try its first/last child (document order), else its sibling.
                node = n;
                let descend = if forward {
                    first_child(&shared, n)
                } else {
                    last_child(&shared, n)
                };
                if let Some(c) = descend {
                    if accept(ctx, id, s.what_to_show, c)? == FILTER_ACCEPT {
                        set_pos(ctx, id, c.index(), false);
                        return Ok(wrap_node(c, ctx)?.into());
                    }
                    node = c;
                }
            }
            None => {
                // No more siblings: ascend; if we hit root (or no accepted
                // ancestor) there is no sibling.
                match parent(&shared, node) {
                    Some(p) if p != root => node = p,
                    _ => return Ok(JsValue::null()),
                }
            }
        }
    }
}

/// `parentNode()` — nearest accepted ancestor of `currentNode` within `root`.
fn tw_parent(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = tw_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let root = NodeId::from_index(s.root);
    let mut node = NodeId::from_index(s.current);
    while node != root {
        match parent(&shared, node) {
            Some(p) => {
                node = p;
                if accept(ctx, id, s.what_to_show, node)? == FILTER_ACCEPT {
                    set_pos(ctx, id, node.index(), false);
                    return Ok(wrap_node(node, ctx)?.into());
                }
                if node == root {
                    break;
                }
            }
            None => break,
        }
    }
    Ok(JsValue::null())
}

/// `nextNode()` — next accepted node in document order from `currentNode`.
fn tw_next(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    tw_doc_move(ctx, id, true)
}
fn tw_prev(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = tw_id(this)?;
    tw_doc_move(ctx, id, false)
}

/// TreeWalker `nextNode`/`previousNode`: flat filtered document order from
/// `currentNode`, updating `currentNode` to the first accepted node.
fn tw_doc_move(ctx: &mut Context, walker_id: u32, forward: bool) -> JsResult<JsValue> {
    let Some(s) = snap(ctx, walker_id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = tw_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let root = NodeId::from_index(s.root);
    let mut node = NodeId::from_index(s.current);
    loop {
        let next = if forward {
            doc_next(&shared, root, node)
        } else {
            doc_prev(&shared, root, node)
        };
        match next {
            Some(n) => {
                node = n;
                if accept(ctx, walker_id, s.what_to_show, node)? == FILTER_ACCEPT {
                    set_pos(ctx, walker_id, node.index(), false);
                    return Ok(wrap_node(node, ctx)?.into());
                }
            }
            None => return Ok(JsValue::null()),
        }
    }
}

// --- factory (called by document.rs) --------------------------------------

/// Parse `(root, whatToShow?, filter?)` and register a walker, returning its id.
/// `root` must be a DOM node; `whatToShow` defaults to `SHOW_ALL`; `filter` may
/// be a callable or an `{acceptNode}` object (else ignored).
fn register(args: &[JsValue], ctx: &mut Context) -> JsResult<u32> {
    let root = NodeHandle::from_this(args.first().unwrap_or(&JsValue::undefined()))?;
    let what_to_show = match args.get(1) {
        Some(v) if !v.is_undefined() && !v.is_null() => v.to_u32(ctx)?,
        _ => SHOW_ALL,
    };
    // A filter is any object (a callable, or `{acceptNode}`); `run_filter`
    // dispatches on which at call time. Non-objects (null/undefined) → no filter.
    let filter = args.get(2).and_then(|v| v.as_object());
    let host = ctx.realm().host_defined();
    let state = host
        .get::<DomState>()
        .ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    let mut reg = state.walkers.borrow_mut();
    reg.next_id += 1;
    let id = reg.next_id;
    reg.walkers.push(WalkerState {
        id,
        root: root.id.index(),
        what_to_show,
        filter,
        current: root.id.index(),
        before: true, // NodeIterator starts before root; TreeWalker ignores it.
    });
    Ok(id)
}

/// `document.createNodeIterator(root, whatToShow?, filter?)`.
pub(crate) fn create_node_iterator(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = register(args, ctx)?;
    Ok(NodeIterator::from_data(NodeIterator { id }, ctx)?.into())
}

/// `document.createTreeWalker(root, whatToShow?, filter?)`.
pub(crate) fn create_tree_walker(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = register(args, ctx)?;
    Ok(TreeWalker::from_data(TreeWalker { id }, ctx)?.into())
}

/// Build the `NodeFilter` global: the `SHOW_*` / `FILTER_*` constants.
pub(crate) fn build_node_filter(ctx: &mut Context) -> JsObject {
    use boa_engine::object::ObjectInitializer;
    use boa_engine::property::Attribute;
    let attr = Attribute::READONLY | Attribute::ENUMERABLE;
    ObjectInitializer::new(ctx)
        .property(js_string!("SHOW_ALL"), SHOW_ALL, attr)
        .property(js_string!("SHOW_ELEMENT"), SHOW_ELEMENT, attr)
        .property(js_string!("SHOW_TEXT"), SHOW_TEXT, attr)
        .property(js_string!("SHOW_COMMENT"), SHOW_COMMENT, attr)
        .property(js_string!("FILTER_ACCEPT"), FILTER_ACCEPT, attr)
        .property(js_string!("FILTER_REJECT"), FILTER_REJECT, attr)
        .property(js_string!("FILTER_SKIP"), FILTER_SKIP, attr)
        .build()
}
