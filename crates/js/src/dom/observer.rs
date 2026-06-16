//! E19-M3 `MutationObserver` — a coarse-grained, microtask-delivered DOM
//! mutation observer. Every JS DOM mutator calls a `record_*` hook AFTER it
//! succeeds; the hook fans the change out to interested observers' queues and
//! schedules a single delivery microtask. Delivery rides the existing
//! `ctx.run_jobs()` checkpoint in the run-to-quiescence loop.
//!
//! Coarse by design (no attribute/character-data diffing, no `oldValue`): the
//! mutator already holds the node(s) it touched, so a record carries exactly the
//! ids in hand. This is enough for the common observe→callback→inspect pattern
//! without the full spec's record-collection machinery.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::job::{Job, PromiseJob};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, Finalize, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue,
    Trace,
};
use starfish_dom::NodeId;

use super::{wrap_node, DomState, NodeHandle};

/// The kind of a recorded mutation. `CharacterData` is part of the observed
/// option set (an `observe({characterData:true})` registration is honored) but
/// no in-scope JS mutator edits text-node data directly, so it is never recorded
/// yet — kept for the complete record-kind surface.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RecordKind {
    ChildList,
    Attributes,
    #[allow(dead_code)]
    CharacterData,
}

/// One coarse mutation record (plain data, no GC pointers).
#[derive(Clone)]
pub(crate) struct RecordData {
    pub kind: RecordKind,
    pub target: NodeId,
    pub added: Vec<NodeId>,
    pub removed: Vec<NodeId>,
    pub attribute_name: Option<String>,
}

/// One `observe(target, options)` registration on an observer (plain data).
pub(crate) struct Registration {
    /// `NodeId.index()` of the observed target.
    pub target: u32,
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
}

/// A single live observer: its JS callback (traced) + its registrations + its
/// pending record queue.
#[derive(Trace, Finalize)]
pub(crate) struct ObserverReg {
    #[unsafe_ignore_trace]
    pub id: u32,
    pub callback: JsObject,
    #[unsafe_ignore_trace]
    pub targets: Vec<Registration>,
    #[unsafe_ignore_trace]
    pub queue: Vec<RecordData>,
}

/// All live observers + the next id to hand out.
#[derive(Trace, Finalize, Default)]
pub(crate) struct ObserverRegistry {
    pub observers: Vec<ObserverReg>,
    #[unsafe_ignore_trace]
    pub next_id: u32,
}

/// Native data on a `MutationObserver` instance: just its registry id.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct MutationObserver {
    #[unsafe_ignore_trace]
    id: u32,
}

impl Class for MutationObserver {
    const NAME: &'static str = "MutationObserver";
    const LENGTH: usize = 1;

    fn data_constructor(
        _new_target: &JsValue,
        args: &[JsValue],
        ctx: &mut Context,
    ) -> JsResult<Self> {
        let cb = args
            .first()
            .and_then(JsValue::as_object)
            .filter(|o| o.is_callable())
            .ok_or_else(|| {
                JsNativeError::typ().with_message("MutationObserver requires a callback")
            })?
            .clone();
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return Err(JsNativeError::typ()
                .with_message("DOM not initialized")
                .into());
        };
        let mut reg = state.observers.borrow_mut();
        reg.next_id += 1;
        let id = reg.next_id;
        reg.observers.push(ObserverReg {
            id,
            callback: cb,
            targets: Vec::new(),
            queue: Vec::new(),
        });
        Ok(MutationObserver { id })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        super::method(class, "observe", 2, observe);
        super::method(class, "disconnect", 0, disconnect);
        super::method(class, "takeRecords", 0, take_records);
        Ok(())
    }
}

/// The registry id stored on `this`.
fn this_id(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<MutationObserver>().map(|m| m.id))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("not a MutationObserver")
                .into()
        })
}

fn opt_bool(opts: &JsObject, key: &str, ctx: &mut Context) -> bool {
    opts.get(js_string!(key), ctx)
        .map(|v| v.to_boolean())
        .unwrap_or(false)
}

/// `observe(target, options)` — push a `Registration` for this observer.
fn observe(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this)?;
    let target = NodeHandle::from_this(args.first().unwrap_or(&JsValue::undefined()))?;
    let opts = args.get(1).and_then(JsValue::as_object);
    let (child_list, attributes, character_data, subtree) = match &opts {
        Some(o) => (
            opt_bool(o, "childList", ctx),
            opt_bool(o, "attributes", ctx),
            opt_bool(o, "characterData", ctx),
            opt_bool(o, "subtree", ctx),
        ),
        None => (false, false, false, false),
    };
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(ob) = state
            .observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
        {
            ob.targets.push(Registration {
                target: target.id.index(),
                child_list,
                attributes,
                character_data,
                subtree,
            });
        }
    }
    Ok(JsValue::undefined())
}

/// `disconnect()` — drop this observer's registrations + queued records.
fn disconnect(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this)?;
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(ob) = state
            .observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
        {
            ob.targets.clear();
            ob.queue.clear();
        }
    }
    Ok(JsValue::undefined())
}

/// `takeRecords()` — drain + return this observer's queue as a record array.
fn take_records(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this)?;
    let drained: Vec<RecordData> = match ctx.realm().host_defined().get::<DomState>() {
        Some(state) => state
            .observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
            .map(|o| std::mem::take(&mut o.queue))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(records_array(&drained, ctx)?.into())
}

/// Whether a registration has the record's kind enabled.
fn kind_enabled(reg: &Registration, kind: RecordKind) -> bool {
    match kind {
        RecordKind::ChildList => reg.child_list,
        RecordKind::Attributes => reg.attributes,
        RecordKind::CharacterData => reg.character_data,
    }
}

/// Fan `rec` out to every interested observer's queue, then (if anything was
/// queued and no delivery is pending) schedule the delivery microtask.
fn notify(ctx: &mut Context, rec: RecordData) {
    let host = ctx.realm().host_defined();
    let Some(state) = host.get::<DomState>() else {
        return;
    };
    let shared = state.doc.clone();
    // Collect the ids of observers that want this record (matching pass first,
    // so we don't hold the registry borrow across the subtree `doc` borrow).
    let interested: Vec<usize> = {
        let reg = state.observers.borrow();
        reg.observers
            .iter()
            .enumerate()
            .filter(|(_, ob)| {
                ob.targets
                    .iter()
                    .any(|r| kind_enabled(r, rec.kind) && matches_target(rec.target, r, &shared))
            })
            .map(|(i, _)| i)
            .collect()
    };
    if interested.is_empty() {
        return;
    }
    {
        let mut reg = state.observers.borrow_mut();
        for i in interested {
            reg.observers[i].queue.push(rec.clone());
        }
    }
    let already = state.mutation_pending.replace(true);
    drop(host);
    if !already {
        ctx.enqueue_job(Job::from(PromiseJob::new(deliver)));
    }
}

/// Subtree-aware match against an already-held `doc` (the `notify` fast path
/// avoids re-borrowing host-defined for each registration).
fn matches_target(target: NodeId, reg: &Registration, shared: &super::SharedDoc) -> bool {
    if reg.target == target.index() {
        return true;
    }
    if !reg.subtree {
        return false;
    }
    let doc = shared.borrow();
    let mut cur = doc.parent(target);
    while let Some(p) = cur {
        if p.index() == reg.target {
            return true;
        }
        cur = doc.parent(p);
    }
    false
}

/// Deliver queued records: for each observer with a non-empty queue, drain it,
/// build the record array, and invoke its callback. Callback throws are caught.
fn deliver(ctx: &mut Context) -> JsResult<JsValue> {
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        state.mutation_pending.set(false);
    }
    // Snapshot (observer_obj-less): id, callback, drained records.
    let work: Vec<(JsObject, Vec<RecordData>)> = match ctx.realm().host_defined().get::<DomState>()
    {
        Some(state) => {
            let mut reg = state.observers.borrow_mut();
            reg.observers
                .iter_mut()
                .filter(|o| !o.queue.is_empty())
                .map(|o| (o.callback.clone(), std::mem::take(&mut o.queue)))
                .collect()
        }
        None => Vec::new(),
    };
    for (callback, recs) in work {
        let arr = records_array(&recs, ctx)?;
        let observer_obj = JsValue::from(callback.clone());
        let _ = callback.call(&observer_obj, &[arr.into(), observer_obj.clone()], ctx);
    }
    Ok(JsValue::undefined())
}

/// Build a `JsArray` of `MutationRecord` objects from drained record data.
fn records_array(recs: &[RecordData], ctx: &mut Context) -> JsResult<JsArray> {
    let arr = JsArray::new(ctx);
    for r in recs {
        let obj = build_record(r, ctx)?;
        arr.push(obj, ctx)?;
    }
    Ok(arr)
}

/// One `MutationRecord` object (MVP fields).
fn build_record(r: &RecordData, ctx: &mut Context) -> JsResult<JsValue> {
    let type_str = match r.kind {
        RecordKind::ChildList => "childList",
        RecordKind::Attributes => "attributes",
        RecordKind::CharacterData => "characterData",
    };
    let target = wrap_node(r.target, ctx)?;
    let added = node_array(&r.added, ctx)?;
    let removed = node_array(&r.removed, ctx)?;
    let attr_name = match &r.attribute_name {
        Some(n) => JsString::from(n.as_str()).into(),
        None => JsValue::null(),
    };
    let obj = ObjectInitializer::new(ctx)
        .property(
            js_string!("type"),
            JsString::from(type_str),
            Attribute::all(),
        )
        .property(js_string!("target"), target, Attribute::all())
        .property(js_string!("addedNodes"), added, Attribute::all())
        .property(js_string!("removedNodes"), removed, Attribute::all())
        .property(js_string!("attributeName"), attr_name, Attribute::all())
        .property(js_string!("oldValue"), JsValue::null(), Attribute::all())
        .property(
            js_string!("previousSibling"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(js_string!("nextSibling"), JsValue::null(), Attribute::all())
        .build();
    Ok(obj.into())
}

fn node_array(ids: &[NodeId], ctx: &mut Context) -> JsResult<JsValue> {
    let arr = JsArray::new(ctx);
    for &id in ids {
        let w = wrap_node(id, ctx)?;
        arr.push(w, ctx)?;
    }
    Ok(arr.into())
}

// --- mutator hooks (called by node.rs / style.rs / dataset.rs) ---

// --- E43-M1: ResizeObserver ------------------------------------------------

/// All live `ResizeObserver`s + the next id. Each holds a traced callback, so
/// the registry is GC-traced to keep callbacks rooted (mirrors `ObserverRegistry`).
#[derive(Trace, Finalize, Default)]
pub(crate) struct ResizeRegistry {
    pub observers: Vec<ResizeObserverReg>,
    #[unsafe_ignore_trace]
    pub next_id: u32,
}

/// One live resize observer: its JS callback + the `NodeId.index()`es it observes.
#[derive(Trace, Finalize)]
pub(crate) struct ResizeObserverReg {
    #[unsafe_ignore_trace]
    pub id: u32,
    pub callback: JsObject,
    #[unsafe_ignore_trace]
    pub targets: Vec<u32>,
}

/// Native data on a `ResizeObserver` instance: just its registry id.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct ResizeObserver {
    #[unsafe_ignore_trace]
    id: u32,
}

impl Class for ResizeObserver {
    const NAME: &'static str = "ResizeObserver";
    const LENGTH: usize = 1;

    fn data_constructor(
        _new_target: &JsValue,
        args: &[JsValue],
        ctx: &mut Context,
    ) -> JsResult<Self> {
        let cb = args
            .first()
            .and_then(JsValue::as_object)
            .filter(|o| o.is_callable())
            .ok_or_else(|| JsNativeError::typ().with_message("ResizeObserver requires a callback"))?
            .clone();
        let host = ctx.realm().host_defined();
        let Some(state) = host.get::<DomState>() else {
            return Err(JsNativeError::typ()
                .with_message("DOM not initialized")
                .into());
        };
        let mut reg = state.resize_observers.borrow_mut();
        reg.next_id += 1;
        let id = reg.next_id;
        reg.observers.push(ResizeObserverReg {
            id,
            callback: cb,
            targets: Vec::new(),
        });
        Ok(ResizeObserver { id })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        super::method(class, "observe", 1, resize_observe);
        super::method(class, "unobserve", 1, resize_unobserve);
        super::method(class, "disconnect", 0, resize_disconnect);
        Ok(())
    }
}

/// The registry id stored on a `ResizeObserver` `this`.
fn resize_this_id(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<ResizeObserver>().map(|m| m.id))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("not a ResizeObserver")
                .into()
        })
}

/// `observe(target)` — add `target` to this observer's set (deduped).
fn resize_observe(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = resize_this_id(this)?;
    let target = NodeHandle::from_this(args.first().unwrap_or(&JsValue::undefined()))?;
    let idx = target.id.index();
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(ob) = state
            .resize_observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
        {
            if !ob.targets.contains(&idx) {
                ob.targets.push(idx);
            }
        }
    }
    Ok(JsValue::undefined())
}

/// `unobserve(target)` — drop `target` from this observer's set.
fn resize_unobserve(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = resize_this_id(this)?;
    let target = NodeHandle::from_this(args.first().unwrap_or(&JsValue::undefined()))?;
    let idx = target.id.index();
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(ob) = state
            .resize_observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
        {
            ob.targets.retain(|&t| t != idx);
        }
    }
    Ok(JsValue::undefined())
}

/// `disconnect()` — drop all of this observer's targets.
fn resize_disconnect(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = resize_this_id(this)?;
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        if let Some(ob) = state
            .resize_observers
            .borrow_mut()
            .observers
            .iter_mut()
            .find(|o| o.id == id)
        {
            ob.targets.clear();
        }
    }
    Ok(JsValue::undefined())
}

/// E43-M1 one-shot delivery: once, after layout has settled, fire each resize
/// observer's callback with one entry per observed element built from its
/// laid-out content box. Called from `run_to_quiescence`.
pub(crate) fn deliver_resize(ctx: &mut Context) {
    // Snapshot (callback, target ids) so we don't hold the registry borrow
    // across the layout/geometry call.
    let work: Vec<(JsObject, Vec<u32>)> = match ctx.realm().host_defined().get::<DomState>() {
        Some(state) => {
            let reg = state.resize_observers.borrow();
            reg.observers
                .iter()
                .filter(|o| !o.targets.is_empty())
                .map(|o| (o.callback.clone(), o.targets.clone()))
                .collect()
        }
        None => return,
    };
    for (callback, targets) in work {
        let entries = JsArray::new(ctx);
        for idx in targets {
            let nid = NodeId::from_index(idx);
            let rect = super::geometry::content_box_for(ctx, nid)
                .ok()
                .flatten()
                .unwrap_or_default();
            let Ok(entry) = build_resize_entry(nid, rect, ctx) else {
                continue;
            };
            let _ = entries.push(entry, ctx);
        }
        let observer_obj = JsValue::from(callback.clone());
        let _ = callback.call(
            &observer_obj,
            &[entries.into(), observer_obj.clone()],
            ctx,
        );
    }
}

/// One `ResizeObserverEntry`: `{ target, contentRect, borderBoxSize,
/// contentBoxSize }`. The sizes are the element's laid-out content box.
fn build_resize_entry(
    target: NodeId,
    rect: starfish_layout::Rect,
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let target = wrap_node(target, ctx)?;
    let content_rect = ObjectInitializer::new(ctx)
        .property(js_string!("x"), rect.x, Attribute::all())
        .property(js_string!("y"), rect.y, Attribute::all())
        .property(js_string!("width"), rect.width, Attribute::all())
        .property(js_string!("height"), rect.height, Attribute::all())
        .property(js_string!("top"), rect.y, Attribute::all())
        .property(js_string!("left"), rect.x, Attribute::all())
        .property(js_string!("right"), rect.x + rect.width, Attribute::all())
        .property(js_string!("bottom"), rect.y + rect.height, Attribute::all())
        .build();
    // `inlineSize`/`blockSize` for horizontal-tb writing mode = width/height.
    let box_size = JsArray::new(ctx);
    let size_obj = ObjectInitializer::new(ctx)
        .property(js_string!("inlineSize"), rect.width, Attribute::all())
        .property(js_string!("blockSize"), rect.height, Attribute::all())
        .build();
    box_size.push(size_obj, ctx)?;
    let content_box_size = box_size;
    let border_box_size = JsArray::new(ctx);
    let bsize_obj = ObjectInitializer::new(ctx)
        .property(js_string!("inlineSize"), rect.width, Attribute::all())
        .property(js_string!("blockSize"), rect.height, Attribute::all())
        .build();
    border_box_size.push(bsize_obj, ctx)?;
    let obj = ObjectInitializer::new(ctx)
        .property(js_string!("target"), target, Attribute::all())
        .property(js_string!("contentRect"), content_rect, Attribute::all())
        .property(
            js_string!("borderBoxSize"),
            border_box_size,
            Attribute::all(),
        )
        .property(
            js_string!("contentBoxSize"),
            content_box_size,
            Attribute::all(),
        )
        .build();
    Ok(obj.into())
}

/// Record an attribute mutation on `target` (`attr_name` already normalized).
pub(crate) fn record_attribute(ctx: &mut Context, target: NodeId, attr_name: &str) {
    notify(
        ctx,
        RecordData {
            kind: RecordKind::Attributes,
            target,
            added: Vec::new(),
            removed: Vec::new(),
            attribute_name: Some(attr_name.to_string()),
        },
    );
}

/// Record a childList mutation on `parent` (coarse: the ids the mutator handled).
pub(crate) fn record_childlist(
    ctx: &mut Context,
    parent: NodeId,
    added: Vec<NodeId>,
    removed: Vec<NodeId>,
) {
    notify(
        ctx,
        RecordData {
            kind: RecordKind::ChildList,
            target: parent,
            added,
            removed,
            attribute_name: None,
        },
    );
}
