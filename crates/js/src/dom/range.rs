//! E57-M3 DOM `Range`: `document.createRange()` and the boundary/contents API.
//!
//! A `Range` is not a node, so (like the E52-M2 TreeWalker) its state lives in a
//! GC-traced registry on [`DomState`] keyed by an id; the JS `Range` object
//! holds only that id. State is two boundary points `(container NodeId, offset)`.
//!
//! MVP scope (node-boundary ranges): `offset` is a child index within an element
//! container. `selectNodeContents(n)` → start `(n,0)` / end `(n, childCount)`;
//! `selectNode(n)` → start/end in `n`'s parent bracketing `n`. The contents ops
//! (`cloneContents`/`extractContents`/`deleteContents`) are implemented well for
//! the common case where start and end share the SAME container at offsets
//! `[s,e)`; a cross-container range degrades to operating on the common
//! ancestor's child slice that fully spans the two boundaries (best effort).
//!
//! Mid-text-node offsets are best-effort: when a boundary container is a Text
//! node, the offset is treated coarsely — offset `0` means "before the text",
//! any positive offset means "after the text" (the text node is not split).
//! `toString()` therefore yields whole-text-node payloads, never substrings.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, Trace,
};
use starfish_dom::{NodeId, NodeKind};

use super::node::text_content;
use super::{accessor, method, wrap_node, DomState, NodeHandle, SharedDoc};

// --- registry -------------------------------------------------------------

/// One live `Range`: two boundary points. All fields are plain data (no GC
/// objects), so the whole struct is trace-ignored.
#[derive(Trace, Finalize)]
pub(crate) struct RangeState {
    #[unsafe_ignore_trace]
    pub id: u32,
    #[unsafe_ignore_trace]
    pub start_container: u32,
    #[unsafe_ignore_trace]
    pub start_offset: u32,
    #[unsafe_ignore_trace]
    pub end_container: u32,
    #[unsafe_ignore_trace]
    pub end_offset: u32,
}

/// All live ranges + the next id to hand out.
#[derive(Trace, Finalize, Default)]
pub(crate) struct RangeRegistry {
    pub ranges: Vec<RangeState>,
    #[unsafe_ignore_trace]
    pub next_id: u32,
}

/// A snapshot of one range's boundary points.
#[derive(Clone, Copy)]
struct RangeSnap {
    start_container: u32,
    start_offset: u32,
    end_container: u32,
    end_offset: u32,
}

fn snap(ctx: &mut Context, id: u32) -> Option<RangeSnap> {
    let host = ctx.realm().host_defined();
    let state = host.get::<DomState>()?;
    let reg = state.ranges.borrow();
    reg.ranges.iter().find(|r| r.id == id).map(|r| RangeSnap {
        start_container: r.start_container,
        start_offset: r.start_offset,
        end_container: r.end_container,
        end_offset: r.end_offset,
    })
}

/// Update a range's start point (and, if it now lies after the end, collapse the
/// end onto the new start — matching the DOM "set start" boundary normalization).
fn set_start(ctx: &mut Context, id: u32, container: u32, offset: u32) {
    update(ctx, id, Some((container, offset)), None);
}
fn set_end(ctx: &mut Context, id: u32, container: u32, offset: u32) {
    update(ctx, id, None, Some((container, offset)));
}

/// Apply a start and/or end update, then normalize so the start is never after
/// the end (whichever boundary was *set* wins; the other collapses onto it).
fn update(ctx: &mut Context, id: u32, start: Option<(u32, u32)>, end: Option<(u32, u32)>) {
    let shared = match ctx.realm().host_defined().get::<DomState>() {
        Some(s) => s.doc.clone(),
        None => return,
    };
    let host = ctx.realm().host_defined();
    let Some(state) = host.get::<DomState>() else {
        return;
    };
    let mut reg = state.ranges.borrow_mut();
    let Some(r) = reg.ranges.iter_mut().find(|r| r.id == id) else {
        return;
    };
    if let Some((c, o)) = start {
        r.start_container = c;
        r.start_offset = o;
        // If start is now after end, collapse end onto start.
        if boundary_after(&shared, c, o, r.end_container, r.end_offset) {
            r.end_container = c;
            r.end_offset = o;
        }
    }
    if let Some((c, o)) = end {
        r.end_container = c;
        r.end_offset = o;
        // If end is now before start, collapse start onto end.
        if boundary_after(&shared, r.start_container, r.start_offset, c, o) {
            r.start_container = c;
            r.start_offset = o;
        }
    }
}

/// Is boundary point `(ac, ao)` strictly after `(bc, bo)` in document order?
/// MVP: a full order is only computed for the common cases we exercise; for the
/// same container it is a plain offset compare; otherwise it falls back to a
/// document-order position compare of the two containers.
fn boundary_after(shared: &SharedDoc, ac: u32, ao: u32, bc: u32, bo: u32) -> bool {
    if ac == bc {
        return ao > bo;
    }
    let a = NodeId::from_index(ac);
    let b = NodeId::from_index(bc);
    doc_order_after(shared, a, b)
}

/// Is node `a` after node `b` in document order (pre-order DFS from the root)?
fn doc_order_after(shared: &SharedDoc, a: NodeId, b: NodeId) -> bool {
    if a == b {
        return false;
    }
    let doc = shared.borrow();
    let root = doc.root();
    let mut order = Vec::new();
    collect_preorder(&doc, root, &mut order);
    let pa = order.iter().position(|&n| n == a);
    let pb = order.iter().position(|&n| n == b);
    match (pa, pb) {
        (Some(pa), Some(pb)) => pa > pb,
        _ => false,
    }
}

fn collect_preorder(doc: &starfish_dom::Document, id: NodeId, out: &mut Vec<NodeId>) {
    out.push(id);
    for c in doc.children(id) {
        collect_preorder(doc, c, out);
    }
}

// --- arena helpers --------------------------------------------------------

fn dom_shared(ctx: &mut Context) -> Option<SharedDoc> {
    ctx.realm().host_defined().get::<DomState>().map(|s| s.doc.clone())
}

/// The list of children that fall in `[start,end)` when both boundaries share
/// `container` (an element). Returns the child `NodeId`s at indices `s..e`.
fn children_in_range(shared: &SharedDoc, container: NodeId, s: u32, e: u32) -> Vec<NodeId> {
    let doc = shared.borrow();
    let kids = doc.children(container);
    let lo = (s as usize).min(kids.len());
    let hi = (e as usize).min(kids.len());
    if lo >= hi {
        return Vec::new();
    }
    kids[lo..hi].to_vec()
}

/// Nearest common ancestor of `a` and `b` (inclusive). Falls back to the
/// document root if no other ancestor is found.
fn common_ancestor(shared: &SharedDoc, a: NodeId, b: NodeId) -> NodeId {
    let doc = shared.borrow();
    // Ancestor chain of `a` (including a itself).
    let mut chain_a = Vec::new();
    let mut cur = Some(a);
    while let Some(n) = cur {
        chain_a.push(n);
        cur = doc.parent(n);
    }
    let mut cur = Some(b);
    while let Some(n) = cur {
        if chain_a.contains(&n) {
            return n;
        }
        cur = doc.parent(n);
    }
    doc.root()
}

// --- Range class ----------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
pub(crate) struct Range {
    #[unsafe_ignore_trace]
    id: u32,
}

impl Class for Range {
    const NAME: &'static str = "Range";
    const LENGTH: usize = 0;

    fn data_constructor(_t: &JsValue, _a: &[JsValue], _c: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ()
            .with_message("Illegal constructor")
            .into())
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        accessor(class, "startContainer", r_start_container, None);
        accessor(class, "startOffset", r_start_offset, None);
        accessor(class, "endContainer", r_end_container, None);
        accessor(class, "endOffset", r_end_offset, None);
        accessor(class, "collapsed", r_collapsed, None);
        accessor(class, "commonAncestorContainer", r_common_ancestor, None);
        method(class, "setStart", 2, r_set_start);
        method(class, "setEnd", 2, r_set_end);
        method(class, "setStartBefore", 1, r_set_start_before);
        method(class, "setStartAfter", 1, r_set_start_after);
        method(class, "setEndBefore", 1, r_set_end_before);
        method(class, "setEndAfter", 1, r_set_end_after);
        method(class, "selectNode", 1, r_select_node);
        method(class, "selectNodeContents", 1, r_select_node_contents);
        method(class, "collapse", 1, r_collapse);
        method(class, "cloneContents", 0, r_clone_contents);
        method(class, "extractContents", 0, r_extract_contents);
        method(class, "deleteContents", 0, r_delete_contents);
        method(class, "toString", 0, r_to_string);
        Ok(())
    }
}

fn range_id(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<Range>().map(|r| r.id))
        .ok_or_else(|| JsNativeError::typ().with_message("not a Range").into())
}

// --- accessors ------------------------------------------------------------

fn r_start_container(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    match snap(ctx, id) {
        Some(s) => Ok(wrap_node(NodeId::from_index(s.start_container), ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}
fn r_end_container(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    match snap(ctx, id) {
        Some(s) => Ok(wrap_node(NodeId::from_index(s.end_container), ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}
fn r_start_offset(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    Ok(snap(ctx, id).map(|s| s.start_offset).unwrap_or(0).into())
}
fn r_end_offset(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    Ok(snap(ctx, id).map(|s| s.end_offset).unwrap_or(0).into())
}
fn r_collapsed(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let collapsed = snap(ctx, id)
        .map(|s| s.start_container == s.end_container && s.start_offset == s.end_offset)
        .unwrap_or(true);
    Ok(collapsed.into())
}
fn r_common_ancestor(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = dom_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let ca = common_ancestor(
        &shared,
        NodeId::from_index(s.start_container),
        NodeId::from_index(s.end_container),
    );
    Ok(wrap_node(ca, ctx)?.into())
}

// --- boundary setters -----------------------------------------------------

fn arg_node(args: &[JsValue]) -> JsResult<NodeHandle> {
    NodeHandle::from_this(args.first().unwrap_or(&JsValue::undefined()))
}

fn r_set_start(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let offset = args.get(1).map(|v| v.to_u32(ctx)).transpose()?.unwrap_or(0);
    set_start(ctx, id, node.id.index(), offset);
    Ok(JsValue::undefined())
}
fn r_set_end(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let offset = args.get(1).map(|v| v.to_u32(ctx)).transpose()?.unwrap_or(0);
    set_end(ctx, id, node.id.index(), offset);
    Ok(JsValue::undefined())
}

/// Index of `node` among its parent's children, plus the parent id.
fn node_index_in_parent(shared: &SharedDoc, node: NodeId) -> Option<(NodeId, u32)> {
    let doc = shared.borrow();
    let parent = doc.parent(node)?;
    let idx = doc.children(parent).iter().position(|&c| c == node)? as u32;
    Some((parent, idx))
}

fn r_set_start_before(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    if let Some((p, i)) = node_index_in_parent(&shared, node.id) {
        set_start(ctx, id, p.index(), i);
    }
    Ok(JsValue::undefined())
}
fn r_set_start_after(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    if let Some((p, i)) = node_index_in_parent(&shared, node.id) {
        set_start(ctx, id, p.index(), i + 1);
    }
    Ok(JsValue::undefined())
}
fn r_set_end_before(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    if let Some((p, i)) = node_index_in_parent(&shared, node.id) {
        set_end(ctx, id, p.index(), i);
    }
    Ok(JsValue::undefined())
}
fn r_set_end_after(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    if let Some((p, i)) = node_index_in_parent(&shared, node.id) {
        set_end(ctx, id, p.index(), i + 1);
    }
    Ok(JsValue::undefined())
}

/// `selectNode(n)` → start/end in `n`'s parent, bracketing `n`.
fn r_select_node(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    if let Some((p, i)) = node_index_in_parent(&shared, node.id) {
        update(ctx, id, Some((p.index(), i)), Some((p.index(), i + 1)));
    }
    Ok(JsValue::undefined())
}

/// `selectNodeContents(n)` → start `(n,0)` / end `(n, childCount)`.
fn r_select_node_contents(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let node = arg_node(args)?;
    let shared = dom_shared(ctx).ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    let count = shared.borrow().children(node.id).len() as u32;
    update(ctx, id, Some((node.id.index(), 0)), Some((node.id.index(), count)));
    Ok(JsValue::undefined())
}

/// `collapse(toStart)` — collapse both boundaries onto one end.
fn r_collapse(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::undefined());
    };
    let to_start = args.first().map(|v| v.to_boolean()).unwrap_or(false);
    if to_start {
        set_end(ctx, id, s.start_container, s.start_offset);
    } else {
        set_start(ctx, id, s.end_container, s.end_offset);
    }
    Ok(JsValue::undefined())
}

// --- contents -------------------------------------------------------------

/// The child nodes the range covers. For the common same-container case this is
/// the `[s,e)` child slice; for a cross-container range (best effort) it is the
/// children of the common ancestor that lie between the two boundaries.
fn ranged_children(shared: &SharedDoc, s: &RangeSnap) -> (NodeId, Vec<NodeId>) {
    if s.start_container == s.end_container {
        let c = NodeId::from_index(s.start_container);
        return (c, children_in_range(shared, c, s.start_offset, s.end_offset));
    }
    // Cross-container: operate on the common ancestor's children that fully span
    // the two boundary containers (best effort — MVP).
    let start = NodeId::from_index(s.start_container);
    let end = NodeId::from_index(s.end_container);
    let ca = common_ancestor(shared, start, end);
    let doc = shared.borrow();
    let kids = doc.children(ca);
    // Find the child of `ca` that contains (or is) `start`, and likewise `end`.
    let idx_for = |target: NodeId| -> Option<usize> {
        kids.iter().position(|&k| {
            let mut cur = Some(target);
            while let Some(n) = cur {
                if n == k {
                    return true;
                }
                cur = doc.parent(n);
            }
            false
        })
    };
    let lo = idx_for(start).unwrap_or(0);
    let hi = idx_for(end).map(|h| h + 1).unwrap_or(kids.len());
    let lo = lo.min(kids.len());
    let hi = hi.min(kids.len());
    if lo >= hi {
        return (ca, Vec::new());
    }
    (ca, kids[lo..hi].to_vec())
}

/// Create a detached fragment-like holder element and deep-clone each ranged
/// child into it (same document). Returns the holder `NodeId`. The original
/// nodes are untouched.
fn build_fragment(shared: &SharedDoc, nodes: &[NodeId]) -> NodeId {
    let mut doc = shared.borrow_mut();
    let holder = doc.create_element("#document-fragment");
    for &n in nodes {
        let copy = doc.clone_node_deep(n);
        doc.append_child(holder, copy);
    }
    holder
}

fn r_clone_contents(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = dom_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let (_ca, nodes) = ranged_children(&shared, &s);
    let holder = build_fragment(&shared, &nodes);
    Ok(wrap_node(holder, ctx)?.into())
}

fn r_extract_contents(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::null());
    };
    let Some(shared) = dom_shared(ctx) else {
        return Ok(JsValue::null());
    };
    let (ca, nodes) = ranged_children(&shared, &s);
    let holder = build_fragment(&shared, &nodes);
    {
        let mut doc = shared.borrow_mut();
        for &n in &nodes {
            let _ = doc.remove_child(ca, n);
        }
    }
    Ok(wrap_node(holder, ctx)?.into())
}

fn r_delete_contents(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsValue::undefined());
    };
    let Some(shared) = dom_shared(ctx) else {
        return Ok(JsValue::undefined());
    };
    let (ca, nodes) = ranged_children(&shared, &s);
    {
        let mut doc = shared.borrow_mut();
        for &n in &nodes {
            let _ = doc.remove_child(ca, n);
        }
    }
    Ok(JsValue::undefined())
}

/// `toString()` — concatenated text of the ranged nodes. For node-boundary
/// ranges this is the text content of the child nodes in `[start,end)`. Mid-text
/// offsets are not honored (whole text-node payloads only).
fn r_to_string(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = range_id(this)?;
    let Some(s) = snap(ctx, id) else {
        return Ok(JsString::from("").into());
    };
    let Some(shared) = dom_shared(ctx) else {
        return Ok(JsString::from("").into());
    };
    // Same Text container: yield its payload (whole-text best effort).
    if s.start_container == s.end_container {
        let c = NodeId::from_index(s.start_container);
        if matches!(shared.borrow().kind(c), NodeKind::Text(_)) {
            let doc = shared.borrow();
            return Ok(JsString::from(text_content(&doc, c)).into());
        }
    }
    let (_ca, nodes) = ranged_children(&shared, &s);
    let doc = shared.borrow();
    let mut out = String::new();
    for n in nodes {
        out.push_str(&text_content(&doc, n));
    }
    Ok(JsString::from(out).into())
}

// --- factory --------------------------------------------------------------

/// `document.createRange()` — register a collapsed range at `(document, 0)`.
pub(crate) fn create_range(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let root = {
        let shared = dom_shared(ctx)
            .ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
        let r = shared.borrow().root();
        r.index()
    };
    let id = {
        let host = ctx.realm().host_defined();
        let state = host
            .get::<DomState>()
            .ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
        let mut reg = state.ranges.borrow_mut();
        reg.next_id += 1;
        let id = reg.next_id;
        reg.ranges.push(RangeState {
            id,
            start_container: root,
            start_offset: 0,
            end_container: root,
            end_offset: 0,
        });
        id
    };
    Ok(Range::from_data(Range { id }, ctx)?.into())
}
