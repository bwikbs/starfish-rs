//! `Node` / `Element` instance methods + accessors on the shared `Node` class.
//! Element-only members check the node kind and return `null` / no-op for
//! non-elements. Every method downcasts `this`, takes a single short borrow of
//! the arena, and never panics on the script path.

use boa_engine::class::ClassBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::ObjectInitializer;
use boa_engine::{js_string, Context, JsNativeError, JsResult, JsString, JsValue};
use starfish_dom::{Document, NodeId, NodeKind, ShadowMode};

use super::select::{matches_selector, parse_selector_list};
use super::{accessor, method, wrap_node, wrap_opt, NodeHandle};

pub(crate) fn init(class: &mut ClassBuilder<'_>) {
    // Read accessors (re-resolve against the live tree each access).
    accessor(class, "nodeName", get_node_name, None);
    accessor(class, "tagName", get_tag_name, None);
    accessor(class, "nodeType", get_node_type, None);
    accessor(class, "parentNode", get_parent, None);
    accessor(class, "parentElement", get_parent, None);
    accessor(class, "firstChild", get_first_child, None);
    accessor(class, "lastChild", get_last_child, None);
    accessor(class, "nextSibling", get_next_sibling, None);
    accessor(class, "previousSibling", get_prev_sibling, None);
    accessor(class, "childNodes", get_child_nodes, None);
    accessor(class, "children", get_children, None);
    accessor(class, "id", get_id, Some(set_id));
    accessor(class, "className", get_class_name, Some(set_class_name));
    accessor(
        class,
        "textContent",
        get_text_content,
        Some(set_text_content),
    );
    accessor(class, "classList", get_class_list, None);
    accessor(class, "style", get_style, None);
    accessor(class, "dataset", get_dataset, None);

    // E8-M1: HTML surface + element navigation.
    accessor(class, "innerHTML", get_inner_html, Some(set_inner_html));
    accessor(class, "outerHTML", get_outer_html, None);
    accessor(class, "firstElementChild", get_first_element_child, None);
    accessor(class, "lastElementChild", get_last_element_child, None);
    accessor(class, "nextElementSibling", get_next_element_sibling, None);
    accessor(
        class,
        "previousElementSibling",
        get_prev_element_sibling,
        None,
    );
    accessor(class, "childElementCount", get_child_element_count, None);

    // E63-M1: <template>.content — a proxy over the template element's children.
    accessor(class, "content", get_content, None);

    // E33-M1: Shadow DOM attach + accessor.
    accessor(class, "shadowRoot", get_shadow_root, None);
    method(class, "attachShadow", 1, attach_shadow);
    // E36-M3
    method(class, "showPopover", 0, show_popover);
    method(class, "hidePopover", 0, hide_popover);
    method(class, "togglePopover", 0, toggle_popover);
    // E33-M2: slot distribution accessors.
    method(class, "assignedNodes", 0, assigned_nodes);
    method(class, "assignedElements", 0, assigned_elements);
    accessor(class, "assignedSlot", get_assigned_slot, None);

    // Methods.
    method(class, "getAttribute", 1, get_attribute);
    method(class, "setAttribute", 2, set_attribute);
    method(class, "removeAttribute", 1, remove_attribute);
    method(class, "hasAttribute", 1, has_attribute);
    method(class, "getAttributeNames", 0, get_attribute_names);
    method(class, "toggleAttribute", 1, toggle_attribute);
    method(class, "appendChild", 1, append_child);
    method(class, "removeChild", 1, remove_child);
    method(class, "insertBefore", 2, insert_before);
    method(class, "replaceChild", 2, replace_child);
    method(class, "cloneNode", 1, clone_node);
    method(class, "insertAdjacentHTML", 2, insert_adjacent_html);
    method(class, "remove", 0, remove_self);

    // E57-M2: node-relationship queries + adjacent insert.
    method(class, "contains", 1, contains);
    method(class, "compareDocumentPosition", 1, compare_document_position);
    accessor(class, "isConnected", get_is_connected, None);
    method(class, "insertAdjacentElement", 2, insert_adjacent_element);
    method(class, "insertAdjacentText", 2, insert_adjacent_text);

    // E19-M1: modern manipulation + selector matching.
    method(class, "append", 1, append);
    method(class, "prepend", 1, prepend);
    method(class, "before", 1, before);
    method(class, "after", 1, after);
    method(class, "replaceWith", 1, replace_with);
    method(class, "matches", 1, matches);
    method(class, "closest", 1, closest);
    accessor(class, "elements", get_form_elements, None); // E58-M2

    // E58-M3: Constraint Validation API on form controls.
    accessor(class, "validity", get_validity, None);
    accessor(class, "validationMessage", get_validation_message, None);
    accessor(class, "willValidate", get_will_validate, None);
    method(class, "checkValidity", 0, check_validity);
    method(class, "reportValidity", 0, check_validity);
    method(class, "setCustomValidity", 1, set_custom_validity);

    // E19-M2: layout-geometry queries (on-demand layout against the viewport).
    method(
        class,
        "getBoundingClientRect",
        0,
        super::geometry::get_bounding_client_rect,
    );
    accessor(
        class,
        "offsetWidth",
        super::geometry::get_offset_width,
        None,
    );
    accessor(
        class,
        "offsetHeight",
        super::geometry::get_offset_height,
        None,
    );
    accessor(class, "offsetTop", super::geometry::get_offset_top, None);
    accessor(class, "offsetLeft", super::geometry::get_offset_left, None);
    accessor(
        class,
        "offsetParent",
        super::geometry::get_offset_parent,
        None,
    );
    accessor(
        class,
        "clientWidth",
        super::geometry::get_client_width,
        None,
    );
    accessor(
        class,
        "clientHeight",
        super::geometry::get_client_height,
        None,
    );
    accessor(
        class,
        "scrollWidth",
        super::geometry::get_scroll_width,
        None,
    );
    accessor(
        class,
        "scrollHeight",
        super::geometry::get_scroll_height,
        None,
    );
    // E37-M2: scrollTop/scrollLeft store a per-element scroll offset (read back
    // verbatim, clamped to >= 0; the painter clamps to scrollHeight at paint).
    accessor(class, "scrollTop", get_scroll_top, Some(set_scroll_top));
    accessor(class, "scrollLeft", get_scroll_left, Some(set_scroll_left));

    // E4-M3: every node (and `document`) is an EventTarget.
    method(
        class,
        "addEventListener",
        2,
        super::event::add_event_listener,
    );
    method(
        class,
        "removeEventListener",
        2,
        super::event::remove_event_listener,
    );
    method(class, "dispatchEvent", 1, super::event::dispatch_event);

    // E20-M1: <canvas> 2D context + width/height (meaningful only on <canvas>;
    // the accessors return defaults / no-op on other elements).
    method(class, "getContext", 1, super::canvas::get_context);
    accessor(class, "width", get_canvas_width, Some(set_canvas_width));
    accessor(class, "height", get_canvas_height, Some(set_canvas_height));
}

// --- E20-M1: <canvas> width/height (attr-backed, default 300×150) ---

/// Parse a canvas dimension attribute (`width`/`height`) to a non-negative
/// integer, falling back to the spec default.
fn canvas_dim(doc: &Document, id: NodeId, attr: &str, default: f64) -> f64 {
    doc.get_attribute(id, attr)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(default)
}

fn get_canvas_width(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    if doc.tag_name(h.id) != Some("canvas") {
        return Ok(JsValue::undefined());
    }
    Ok(JsValue::from(canvas_dim(&doc, h.id, "width", 300.0)))
}

fn get_canvas_height(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    if doc.tag_name(h.id) != Some("canvas") {
        return Ok(JsValue::undefined());
    }
    Ok(JsValue::from(canvas_dim(&doc, h.id, "height", 150.0)))
}

/// Set a canvas dimension: write the attribute and reset the op list (per spec,
/// changing a canvas dimension clears its bitmap). No-op off `<canvas>`.
fn set_canvas_dim(this: &JsValue, args: &[JsValue], ctx: &mut Context, attr: &str) -> JsResult<()> {
    let h = NodeHandle::from_this(this)?;
    {
        let doc = h.shared.borrow();
        if doc.tag_name(h.id) != Some("canvas") {
            return Ok(());
        }
    }
    let n = match args.first() {
        Some(v) => v.to_number(ctx)?,
        None => return Ok(()),
    };
    if !n.is_finite() || n < 0.0 {
        return Ok(());
    }
    let mut doc = h.shared.borrow_mut();
    doc.set_attribute(h.id, attr, &(n as u32).to_string());
    doc.canvas_reset(h.id);
    Ok(())
}

fn set_canvas_width(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    set_canvas_dim(this, args, ctx, "width")?;
    Ok(JsValue::undefined())
}

fn set_canvas_height(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    set_canvas_dim(this, args, ctx, "height")?;
    Ok(JsValue::undefined())
}

// --- E37-M2: scrollTop / scrollLeft (per-element scroll offset) ---

fn get_scroll_top(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    Ok(JsValue::from(doc.scroll_offset(h.id).1))
}

fn get_scroll_left(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    Ok(JsValue::from(doc.scroll_offset(h.id).0))
}

fn set_scroll_top(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let y = match args.first() {
        Some(v) => v.to_number(ctx)?,
        None => return Ok(JsValue::undefined()),
    };
    if y.is_finite() {
        let mut doc = h.shared.borrow_mut();
        let (x, _) = doc.scroll_offset(h.id);
        doc.set_scroll_offset(h.id, x, y as f32);
    }
    Ok(JsValue::undefined())
}

fn set_scroll_left(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let x = match args.first() {
        Some(v) => v.to_number(ctx)?,
        None => return Ok(JsValue::undefined()),
    };
    if x.is_finite() {
        let mut doc = h.shared.borrow_mut();
        let (_, y) = doc.scroll_offset(h.id);
        doc.set_scroll_offset(h.id, x as f32, y);
    }
    Ok(JsValue::undefined())
}

// --- small helpers shared with document.rs ---

/// String arg coerced to a Rust `String` (missing arg → "").
pub(crate) fn arg_str(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    match args.get(i) {
        Some(v) => Ok(v.to_string(ctx)?.to_std_string_escaped()),
        None => Ok(String::new()),
    }
}

/// Downcast a value (an argument) to a `NodeHandle`, throwing if it is not a node.
fn arg_node(args: &[JsValue], i: usize) -> JsResult<NodeHandle> {
    args.get(i)
        .and_then(|v| v.as_object())
        .and_then(|o| o.downcast_ref::<NodeHandle>().map(|h| h.clone()))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("argument is not a DOM node")
                .into()
        })
}

/// Concatenate all descendant Text payloads of `id` in document order.
pub(crate) fn text_content(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    collect_text(doc, id, &mut out);
    out
}

fn collect_text(doc: &Document, id: NodeId, out: &mut String) {
    match doc.kind(id) {
        NodeKind::Text(t) => out.push_str(t),
        _ => {
            for c in doc.children(id) {
                collect_text(doc, c, out);
            }
        }
    }
}

/// Is `maybe_ancestor` an ancestor of (or equal to) `node`?
fn is_ancestor_or_self(doc: &Document, maybe_ancestor: NodeId, node: NodeId) -> bool {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n == maybe_ancestor {
            return true;
        }
        cur = doc.parent(n);
    }
    false
}

// --- read accessors ---

fn get_node_name(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let s = match doc.kind(h.id) {
        NodeKind::Document => "#document".to_string(),
        NodeKind::Doctype(d) => d.name.clone(),
        NodeKind::Element(e) => e.name.to_ascii_uppercase(),
        NodeKind::Text(_) => "#text".to_string(),
        NodeKind::Comment(_) => "#comment".to_string(),
        NodeKind::ShadowRoot(_) => "#shadow-root".to_string(), // E33-M1
        NodeKind::DocumentFragment => "#document-fragment".to_string(), // E63-M2
    };
    Ok(JsString::from(s).into())
}

fn get_tag_name(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    match doc.tag_name(h.id) {
        Some(t) => Ok(JsString::from(t.to_ascii_uppercase()).into()),
        None => Ok(JsValue::null()),
    }
}

fn get_node_type(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let n = match doc.kind(h.id) {
        NodeKind::Element(_) => 1,
        NodeKind::Text(_) => 3,
        NodeKind::Comment(_) => 8,
        NodeKind::Doctype(_) => 10,
        NodeKind::Document => 9,
        NodeKind::ShadowRoot(_) => 11, // E33-M1: DocumentFragment node type
        NodeKind::DocumentFragment => 11, // E63-M2
    };
    Ok(JsValue::from(n))
}

fn get_parent(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().parent(h.id);
    wrap_opt(id, ctx)
}

fn get_first_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().first_child(h.id);
    wrap_opt(id, ctx)
}

fn get_last_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().last_child(h.id);
    wrap_opt(id, ctx)
}

fn get_next_sibling(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().next_sibling(h.id);
    wrap_opt(id, ctx)
}

fn get_prev_sibling(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().prev_sibling(h.id);
    wrap_opt(id, ctx)
}

fn get_child_nodes(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let kids = h.shared.borrow().children(h.id);
    nodes_to_array(&kids, ctx)
}

fn get_children(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let kids: Vec<NodeId> = {
        let doc = h.shared.borrow();
        doc.children(h.id)
            .into_iter()
            .filter(|c| doc.tag_name(*c).is_some())
            .collect()
    };
    nodes_to_array(&kids, ctx)
}

/// Wrap each id and collect into a static JS `Array` of wrappers.
pub(crate) fn nodes_to_array(ids: &[NodeId], ctx: &mut Context) -> JsResult<JsValue> {
    let mut items: Vec<JsValue> = Vec::with_capacity(ids.len());
    for &id in ids {
        items.push(wrap_node(id, ctx)?.into());
    }
    Ok(JsArray::from_iter(items, ctx).into())
}

fn get_id(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    attr_value(this, "id")
}

fn set_id(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let v = arg_str(args, 0, ctx)?;
    h.shared.borrow_mut().set_attribute(h.id, "id", &v);
    super::observer::record_attribute(ctx, h.id, "id");
    Ok(JsValue::undefined())
}

fn get_class_name(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    attr_value(this, "class")
}

fn set_class_name(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let v = arg_str(args, 0, ctx)?;
    h.shared.borrow_mut().set_attribute(h.id, "class", &v);
    super::observer::record_attribute(ctx, h.id, "class");
    Ok(JsValue::undefined())
}

/// Attribute value (or "" when absent/non-element), as a JS string. `id`/
/// `className` reflect the empty string when absent (per the IDL).
fn attr_value(this: &JsValue, name: &str) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let s = doc.get_attribute(h.id, name).unwrap_or("").to_string();
    Ok(JsString::from(s).into())
}

fn get_text_content(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    Ok(JsString::from(text_content(&doc, h.id)).into())
}

fn set_text_content(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let s = arg_str(args, 0, ctx)?;
    let (removed, added) = {
        let mut doc = h.shared.borrow_mut();
        // Remove all children, then append a single Text node when non-empty.
        let removed = doc.children(h.id);
        for c in &removed {
            doc.detach(*c);
        }
        let mut added = Vec::new();
        if !s.is_empty() {
            let t = doc.create_text(&s);
            doc.append_child(h.id, t);
            added.push(t);
        }
        (removed, added)
    };
    super::observer::record_childlist(ctx, h.id, added, removed);
    Ok(JsValue::undefined())
}

fn get_class_list(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    super::style::class_list_object(h, ctx)
}

fn get_style(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    super::style::style_object(h, ctx)
}

fn get_dataset(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    super::dataset::dataset_object(h, ctx)
}

// --- attribute methods ---

fn get_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let doc = h.shared.borrow();
    match doc.get_attribute(h.id, &name) {
        Some(v) => Ok(JsString::from(v).into()),
        None => Ok(JsValue::null()),
    }
}

fn set_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let name = arg_str(args, 0, ctx)?;
    let value = arg_str(args, 1, ctx)?;
    // E57-M1: capture old value before mutating for attributeChangedCallback.
    let old = h.shared.borrow().get_attribute(h.id, &name).map(str::to_owned);
    h.shared.borrow_mut().set_attribute(h.id, &name, &value);
    super::observer::record_attribute(ctx, h.id, &name);
    super::custom_elements::react_attribute_changed(ctx, h.id, &name, old, Some(value)); // E57-M1
    Ok(JsValue::undefined())
}

fn remove_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let name = arg_str(args, 0, ctx)?;
    // E57-M1: capture old value before mutating for attributeChangedCallback.
    let old = h.shared.borrow().get_attribute(h.id, &name).map(str::to_owned);
    h.shared.borrow_mut().remove_attribute(h.id, &name);
    super::observer::record_attribute(ctx, h.id, &name);
    super::custom_elements::react_attribute_changed(ctx, h.id, &name, old, None); // E57-M1
    Ok(JsValue::undefined())
}

fn has_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let doc = h.shared.borrow();
    Ok(JsValue::from(doc.get_attribute(h.id, &name).is_some()))
}

fn get_attribute_names(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let names = h.shared.borrow().attribute_names(h.id);
    let items: Vec<JsValue> = names
        .into_iter()
        .map(|n| JsString::from(n).into())
        .collect();
    Ok(JsArray::from_iter(items, ctx).into())
}

fn toggle_attribute(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let force = args.get(1).map(|v| v.to_boolean());
    let changed;
    let add;
    {
        let mut doc = h.shared.borrow_mut();
        let present = doc.get_attribute(h.id, &name).is_some();
        add = force.unwrap_or(!present);
        if add && !present {
            doc.set_attribute(h.id, &name, "");
            changed = true;
        } else if !add && present {
            doc.remove_attribute(h.id, &name);
            changed = true;
        } else {
            changed = false;
        }
    }
    if changed {
        super::observer::record_attribute(ctx, h.id, &name);
    }
    Ok(JsValue::from(add))
}

// --- tree mutation methods ---

fn append_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let child = arg_node(args, 0)?;
    let added = {
        let mut doc = h.shared.borrow_mut();
        if is_ancestor_or_self(&doc, child.id, h.id) {
            return Err(JsNativeError::typ()
                .with_message("appendChild would create a cycle")
                .into());
        }
        // E63-M2: a DocumentFragment argument moves its children, not itself.
        append_flattening(&mut doc, h.id, child.id)
    };
    if !added.is_empty() {
        super::observer::record_childlist(ctx, h.id, added, Vec::new());
    }
    // Return the appended child (the same cached wrapper).
    Ok(args[0].clone())
}

// --- E33-M1: Shadow DOM ---

/// `element.attachShadow({ mode })` → the shadow root (wrapped). The `mode`
/// option (`"open"`/`"closed"`) defaults to `Open` when absent / not an object.
fn attach_shadow(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    // Read `mode` off the init dict; `"closed"` → Closed, anything else → Open.
    let mode = match args.first().and_then(|v| v.as_object()) {
        Some(o) => {
            let m = o
                .get(js_string!("mode"), ctx)?
                .to_string(ctx)?
                .to_std_string_escaped();
            if m == "closed" {
                ShadowMode::Closed
            } else {
                ShadowMode::Open
            }
        }
        None => ShadowMode::Open,
    };
    let sr = h.shared.borrow_mut().attach_shadow(h.id, mode);
    Ok(wrap_node(sr, ctx)?.into())
}

/// E63-M1: `template.content` → a fragment-like proxy whose `children` /
/// `querySelector` / `firstChild` see the template's parsed children. MVP:
/// since the template's children live in the arena under the template node (the
/// UA `template { display: none }` rule keeps them inert), `.content` returns the
/// template element's own wrapper — its children ARE the content. This is a proxy
/// over the template element, NOT a separate DocumentFragment node (deferred to
/// E63-M2). For non-`<template>` elements, `.content` is `null`.
fn get_content(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let is_template = h
        .shared
        .borrow()
        .tag_name(h.id)
        .is_some_and(|t| t.eq_ignore_ascii_case("template"));
    if is_template {
        Ok(wrap_node(h.id, ctx)?.into())
    } else {
        Ok(JsValue::null())
    }
}

/// `element.shadowRoot` → the shadow root (wrapped) iff it exists AND is open;
/// otherwise `null` (closed roots are not exposed here).
fn get_shadow_root(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let open_root = {
        let doc = h.shared.borrow();
        doc.shadow_root(h.id)
            .filter(|&sr| doc.shadow_mode(sr) == Some(ShadowMode::Open))
    };
    match open_root {
        Some(sr) => Ok(wrap_node(sr, ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}

// --- E36-M3: Popover API ---

/// Set the element's internal popover open flag to `open`, but only if the
/// element carries a `popover` attribute. MVP is lenient: no attribute → no-op
/// (the spec throws). Returns whether the flag was actually set.
fn set_popover(h: &NodeHandle, open: bool) {
    let mut doc = h.shared.borrow_mut();
    if doc.get_attribute(h.id, "popover").is_some() {
        doc.set_popover_open(h.id, open);
    }
}

/// `element.showPopover()` → set the open flag (reflected by `:popover-open`).
fn show_popover(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    set_popover(&h, true);
    Ok(JsValue::undefined())
}

/// `element.hidePopover()` → clear the open flag.
fn hide_popover(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    set_popover(&h, false);
    Ok(JsValue::undefined())
}

/// `element.togglePopover()` → flip the open flag (the optional force arg is
/// ignored for MVP).
fn toggle_popover(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let cur = h.shared.borrow().is_popover_open(h.id);
    set_popover(&h, !cur);
    Ok(JsValue::undefined())
}

/// E33-M2: `slot.assignedNodes()` → the distributed light-DOM nodes (the
/// optional `{flatten}` arg is ignored for MVP). Empty array for a non-slot.
fn assigned_nodes(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let ids = {
        let doc = h.shared.borrow();
        if doc.tag_name(h.id) != Some("slot") {
            Vec::new()
        } else {
            doc.slot_assigned_nodes(h.id)
        }
    };
    nodes_to_array(&ids, ctx)
}

/// E33-M2: `slot.assignedElements()` → as `assignedNodes` but only elements.
fn assigned_elements(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let ids = {
        let doc = h.shared.borrow();
        if doc.tag_name(h.id) != Some("slot") {
            Vec::new()
        } else {
            doc.slot_assigned_nodes(h.id)
                .into_iter()
                .filter(|&id| doc.tag_name(id).is_some())
                .collect::<Vec<_>>()
        }
    };
    nodes_to_array(&ids, ctx)
}

/// E58-M2: `form.elements` → an array of the form's descendant form controls
/// (`input`/`select`/`textarea`/`button`) in document order. Empty unless `this`
/// is a `<form>`.
fn get_form_elements(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let ids: Vec<NodeId> = {
        let doc = h.shared.borrow();
        if doc.tag_name(h.id) != Some("form") {
            Vec::new()
        } else {
            let mut out = Vec::new();
            let mut stack: Vec<NodeId> = doc.children(h.id).into_iter().rev().collect();
            while let Some(n) = stack.pop() {
                if matches!(
                    doc.tag_name(n),
                    Some("input" | "select" | "textarea" | "button")
                ) {
                    out.push(n);
                }
                for c in doc.children(n).into_iter().rev() {
                    stack.push(c);
                }
            }
            out
        }
    };
    nodes_to_array(&ids, ctx)
}

// E58-M3: Constraint Validation API. The built-in validity flags come from the
// shared `starfish_style::validity` helper (the single source of truth, also
// used by `:valid`/`:invalid`); the custom error lives in a `DomState` side
// table keyed by `NodeId.index()`.

/// Read the custom validity message for `id` from the `DomState` side table.
fn custom_validity_msg(ctx: &mut Context, id: NodeId) -> String {
    ctx.realm()
        .host_defined()
        .get::<super::DomState>()
        .and_then(|s| s.custom_validity.borrow().get(&id.index()).cloned())
        .unwrap_or_default()
}

/// `el.validity` → a ValidityState plain object with the per-flag booleans.
fn get_validity(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let v = starfish_style::validity(&h.shared.borrow(), h.id);
    let custom = !custom_validity_msg(ctx, h.id).is_empty();
    let valid = v.is_valid() && !custom;
    let mut init = ObjectInitializer::new(ctx);
    let flag = |init: &mut ObjectInitializer<'_>, name: &str, b: bool| {
        init.property(js_string!(name), b, boa_engine::property::Attribute::all());
    };
    flag(&mut init, "valueMissing", v.value_missing);
    flag(&mut init, "typeMismatch", v.type_mismatch);
    flag(&mut init, "rangeUnderflow", v.range_underflow);
    flag(&mut init, "rangeOverflow", v.range_overflow);
    flag(&mut init, "patternMismatch", v.pattern_mismatch);
    flag(&mut init, "customError", custom);
    // MVP: not yet implemented → always false.
    flag(&mut init, "stepMismatch", false);
    flag(&mut init, "tooLong", false);
    flag(&mut init, "tooShort", false);
    flag(&mut init, "badInput", false);
    flag(&mut init, "valid", valid);
    Ok(init.build().into())
}

/// `el.validationMessage` → the custom error message (empty when valid/none).
fn get_validation_message(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    Ok(JsString::from(custom_validity_msg(ctx, h.id)).into())
}

/// `el.willValidate` → true for a non-disabled, non-readonly candidate control.
fn get_will_validate(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let will = starfish_style::validity(&doc, h.id).candidate
        && doc.get_attribute(h.id, "disabled").is_none()
        && doc.get_attribute(h.id, "readonly").is_none();
    Ok(will.into())
}

/// `el.checkValidity()` / `el.reportValidity()` → true when the control is
/// valid (no built-in error flag and no custom error). No UI bubble.
fn check_validity(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let built_in_valid = starfish_style::validity(&h.shared.borrow(), h.id).is_valid();
    let custom = !custom_validity_msg(ctx, h.id).is_empty();
    Ok((built_in_valid && !custom).into())
}

/// `el.setCustomValidity(msg)` → store (or clear, when empty) the custom error.
fn set_custom_validity(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let msg = arg_str(args, 0, ctx)?;
    if let Some(state) = ctx.realm().host_defined().get::<super::DomState>() {
        let mut table = state.custom_validity.borrow_mut();
        if msg.is_empty() {
            table.remove(&h.id.index());
        } else {
            table.insert(h.id.index(), msg);
        }
    }
    Ok(JsValue::undefined())
}

/// E33-M2: `node.assignedSlot` → the `<slot>` this light child is distributed
/// into (wrapped), or `null`.
fn get_assigned_slot(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let slot = h.shared.borrow().assigned_slot(h.id);
    match slot {
        Some(s) => Ok(wrap_node(s, ctx)?.into()),
        None => Ok(JsValue::null()),
    }
}

fn remove_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let child = arg_node(args, 0)?;
    h.shared
        .borrow_mut()
        .remove_child(h.id, child.id)
        .map_err(|()| {
            JsNativeError::typ().with_message("node to remove is not a child of this node")
        })?;
    super::observer::record_childlist(ctx, h.id, Vec::new(), vec![child.id]);
    super::custom_elements::react_disconnected(ctx, child.id); // E57-M1
    Ok(args[0].clone())
}

fn insert_before(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let new = arg_node(args, 0)?;
    // Second arg may be a node or null/undefined (→ append).
    let reference = match args.get(1) {
        Some(v) if v.is_null() || v.is_undefined() => None,
        Some(_) => Some(arg_node(args, 1)?.id),
        None => None,
    };
    let added = {
        let mut doc = h.shared.borrow_mut();
        if is_ancestor_or_self(&doc, new.id, h.id) {
            return Err(JsNativeError::typ()
                .with_message("insertBefore would create a cycle")
                .into());
        }
        // Validate the reference is a child of this node before mutating.
        if let Some(r) = reference {
            if doc.parent(r) != Some(h.id) {
                return Err(JsNativeError::typ()
                    .with_message("reference node is not a child of this node")
                    .into());
            }
        }
        // E63-M2: a DocumentFragment argument moves its children, not itself.
        insert_flattening(&mut doc, h.id, new.id, reference)
    };
    if !added.is_empty() {
        super::observer::record_childlist(ctx, h.id, added, Vec::new());
    }
    Ok(args[0].clone())
}

fn replace_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    // replaceChild(new, old) = insert new before old, then remove old.
    let h = NodeHandle::from_this(this)?;
    let new = arg_node(args, 0)?;
    let old = arg_node(args, 1)?;
    {
        let mut doc = h.shared.borrow_mut();
        if is_ancestor_or_self(&doc, new.id, h.id) {
            return Err(JsNativeError::typ()
                .with_message("replaceChild would create a cycle")
                .into());
        }
        doc.insert_before(h.id, new.id, Some(old.id))
            .map_err(|()| {
                JsNativeError::typ().with_message("old child is not a child of this node")
            })?;
        // old is guaranteed a child here.
        let _ = doc.remove_child(h.id, old.id);
    }
    super::observer::record_childlist(ctx, h.id, vec![new.id], vec![old.id]);
    // Return the removed (old) child.
    Ok(args[1].clone())
}

// --- E8-M1: innerHTML / outerHTML / cloneNode / insertAdjacentHTML / remove ---

/// First `<body>` element in a (throwaway) fragment document.
fn find_body(doc: &Document) -> Option<NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("body") {
            return Some(id);
        }
        for c in doc.children(id) {
            stack.push(c);
        }
    }
    None
}

/// Parse `html` as a throwaway document and return its `<body>`'s child ids.
fn parse_fragment(html: &str) -> (Document, Vec<NodeId>) {
    let frag = starfish_html::parse(html);
    let roots = find_body(&frag)
        .map(|b| frag.children(b))
        .unwrap_or_default();
    (frag, roots)
}

fn get_inner_html(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let s = h.shared.borrow().inner_html(h.id);
    Ok(JsString::from(s).into())
}

fn get_outer_html(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let s = h.shared.borrow().outer_html(h.id);
    Ok(JsString::from(s).into())
}

fn set_inner_html(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let html = arg_str(args, 0, ctx)?;
    let (frag, roots) = parse_fragment(&html);

    let (removed, added) = {
        let mut doc = h.shared.borrow_mut();
        // Replace the target's children with the parsed fragment.
        let removed = doc.children(h.id);
        for c in &removed {
            doc.detach(*c);
        }
        let mut added = Vec::new();
        for r in roots {
            let new = doc.import_subtree(&frag, r);
            doc.append_child(h.id, new);
            added.push(new);
        }
        (removed, added)
    };
    super::observer::record_childlist(ctx, h.id, added, removed);
    Ok(JsValue::undefined())
}

fn insert_adjacent_html(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let pos = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let html = arg_str(args, 1, ctx)?;
    let (frag, roots) = parse_fragment(&html);

    // (target_parent, added ids) so we can record one childList mutation.
    let inserted: Option<(NodeId, Vec<NodeId>)> = {
        let mut doc = h.shared.borrow_mut();
        match pos.as_str() {
            "afterbegin" => {
                // Insert each before the current first child, preserving order.
                let first = doc.first_child(h.id);
                let mut added = Vec::new();
                for r in roots {
                    let new = doc.import_subtree(&frag, r);
                    let _ = doc.insert_before(h.id, new, first);
                    added.push(new);
                }
                Some((h.id, added))
            }
            "beforeend" => {
                let mut added = Vec::new();
                for r in roots {
                    let new = doc.import_subtree(&frag, r);
                    doc.append_child(h.id, new);
                    added.push(new);
                }
                Some((h.id, added))
            }
            "beforebegin" | "afterend" => {
                let Some(parent) = doc.parent(h.id) else {
                    return Ok(JsValue::undefined());
                };
                let reference = match pos.as_str() {
                    "beforebegin" => Some(h.id),
                    _ => doc.next_sibling(h.id),
                };
                let mut added = Vec::new();
                for r in roots {
                    let new = doc.import_subtree(&frag, r);
                    let _ = doc.insert_before(parent, new, reference);
                    added.push(new);
                }
                Some((parent, added))
            }
            _ => {
                return Err(JsNativeError::typ()
                    .with_message("invalid insertAdjacentHTML position")
                    .into());
            }
        }
    };
    if let Some((parent, added)) = inserted {
        super::observer::record_childlist(ctx, parent, added, Vec::new());
    }
    Ok(JsValue::undefined())
}

fn clone_node(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let deep = args.first().map(|v| v.to_boolean()).unwrap_or(false);
    let new_id = {
        let mut doc = h.shared.borrow_mut();
        if deep {
            doc.clone_node_deep(h.id)
        } else {
            doc.clone_node_shallow(h.id)
        }
    };
    Ok(wrap_node(new_id, ctx)?.into())
}

fn remove_self(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parent = {
        let mut doc = h.shared.borrow_mut();
        let parent = doc.parent(h.id);
        doc.detach(h.id);
        parent
    };
    if let Some(parent) = parent {
        super::observer::record_childlist(ctx, parent, Vec::new(), vec![h.id]);
    }
    super::custom_elements::react_disconnected(ctx, h.id); // E57-M1
    Ok(JsValue::undefined())
}

// --- E19-M1: modern insertion (append/prepend/before/after/replaceWith) ---

/// A variadic argument to the insertion methods: an existing node, or a string
/// that becomes a Text node.
enum Arg {
    Node(NodeId),
    Text(String),
}

/// Phase 1 (ctx available): coerce each argument into an `Arg`. A value that is
/// a `NodeHandle` is taken by id; everything else is stringified.
fn collect_args(args: &[JsValue], ctx: &mut Context) -> JsResult<Vec<Arg>> {
    let mut out = Vec::with_capacity(args.len());
    for v in args {
        let node = v
            .as_object()
            .and_then(|o| o.downcast_ref::<NodeHandle>().map(|h| h.id));
        match node {
            Some(id) => out.push(Arg::Node(id)),
            None => out.push(Arg::Text(v.to_string(ctx)?.to_std_string_escaped())),
        }
    }
    Ok(out)
}

/// Phase 2 (borrow_mut): materialize an `Arg` into a concrete node id to be
/// inserted under `parent`. A `Text` arg always yields a fresh text node; a
/// `Node` arg is skipped (`None`) when it is `parent` or an ancestor of it, to
/// avoid creating a cycle (mirrors `appendChild`/`insertBefore`'s guard).
fn materialize(doc: &mut Document, arg: Arg, parent: NodeId) -> Option<NodeId> {
    match arg {
        Arg::Node(id) if is_ancestor_or_self(doc, id, parent) => None,
        Arg::Node(id) => Some(id),
        Arg::Text(s) => Some(doc.create_text(&s)),
    }
}

/// E63-M2: append `node` under `parent`. If `node` is a `DocumentFragment` its
/// children are moved (in order) into `parent` and the fragment is left empty,
/// per the DOM "insert" algorithm; otherwise `node` itself is appended.
/// Returns the ids actually added (the moved children, or `[node]`).
fn append_flattening(doc: &mut Document, parent: NodeId, node: NodeId) -> Vec<NodeId> {
    if doc.is_document_fragment(node) {
        let kids = doc.children(node);
        for &c in &kids {
            doc.append_child(parent, c);
        }
        kids
    } else {
        doc.append_child(parent, node);
        vec![node]
    }
}

/// E63-M2: insert `node` under `parent` before `reference` (or append when
/// `reference` is `None`). A `DocumentFragment` is flattened: its children move
/// in order before the reference and the fragment is left empty.
fn insert_flattening(
    doc: &mut Document,
    parent: NodeId,
    node: NodeId,
    reference: Option<NodeId>,
) -> Vec<NodeId> {
    if doc.is_document_fragment(node) {
        let kids = doc.children(node);
        for &c in &kids {
            let _ = doc.insert_before(parent, c, reference);
        }
        kids
    } else {
        let _ = doc.insert_before(parent, node, reference);
        vec![node]
    }
}

fn append(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parsed = collect_args(args, ctx)?;
    let added = {
        let mut doc = h.shared.borrow_mut();
        let mut added = Vec::new();
        for a in parsed {
            if let Some(id) = materialize(&mut doc, a, h.id) {
                // E63-M2: flatten a DocumentFragment into its children.
                added.extend(append_flattening(&mut doc, h.id, id));
            }
        }
        added
    };
    if !added.is_empty() {
        super::observer::record_childlist(ctx, h.id, added, Vec::new());
    }
    Ok(JsValue::undefined())
}

fn prepend(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parsed = collect_args(args, ctx)?;
    let added = {
        let mut doc = h.shared.borrow_mut();
        let first = doc.first_child(h.id);
        let mut added = Vec::new();
        for a in parsed {
            if let Some(id) = materialize(&mut doc, a, h.id) {
                // E63-M2: flatten a DocumentFragment into its children.
                added.extend(insert_flattening(&mut doc, h.id, id, first));
            }
        }
        added
    };
    if !added.is_empty() {
        super::observer::record_childlist(ctx, h.id, added, Vec::new());
    }
    Ok(JsValue::undefined())
}

fn before(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parsed = collect_args(args, ctx)?;
    let inserted = {
        let mut doc = h.shared.borrow_mut();
        let Some(parent) = doc.parent(h.id) else {
            return Ok(JsValue::undefined());
        };
        let mut added = Vec::new();
        for a in parsed {
            if let Some(id) = materialize(&mut doc, a, parent) {
                let _ = doc.insert_before(parent, id, Some(h.id));
                added.push(id);
            }
        }
        (parent, added)
    };
    if !inserted.1.is_empty() {
        super::observer::record_childlist(ctx, inserted.0, inserted.1, Vec::new());
    }
    Ok(JsValue::undefined())
}

fn after(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parsed = collect_args(args, ctx)?;
    let inserted = {
        let mut doc = h.shared.borrow_mut();
        let Some(parent) = doc.parent(h.id) else {
            return Ok(JsValue::undefined());
        };
        let reference = doc.next_sibling(h.id);
        let mut added = Vec::new();
        for a in parsed {
            if let Some(id) = materialize(&mut doc, a, parent) {
                let _ = doc.insert_before(parent, id, reference);
                added.push(id);
            }
        }
        (parent, added)
    };
    if !inserted.1.is_empty() {
        super::observer::record_childlist(ctx, inserted.0, inserted.1, Vec::new());
    }
    Ok(JsValue::undefined())
}

fn replace_with(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let parsed = collect_args(args, ctx)?;
    let inserted = {
        let mut doc = h.shared.borrow_mut();
        let Some(parent) = doc.parent(h.id) else {
            return Ok(JsValue::undefined());
        };
        let reference = doc.next_sibling(h.id);
        // Guard against cycles BEFORE detaching self: a node arg that is an
        // ancestor of `parent` would cycle. `self` itself is removed, so OK.
        let ids: Vec<NodeId> = parsed
            .into_iter()
            .filter_map(|a| match a {
                Arg::Node(id) if is_ancestor_or_self(&doc, id, parent) => None,
                Arg::Node(id) => Some(id),
                Arg::Text(s) => Some(doc.create_text(&s)),
            })
            .collect();
        doc.detach(h.id);
        for id in &ids {
            let _ = doc.insert_before(parent, *id, reference);
        }
        (parent, ids)
    };
    super::observer::record_childlist(ctx, inserted.0, inserted.1, vec![h.id]);
    Ok(JsValue::undefined())
}

// --- E19-M1: selector matching (matches / closest) ---

fn matches(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let sel = arg_str(args, 0, ctx)?;
    let Some(sels) = parse_selector_list(&sel) else {
        return Ok(JsValue::from(false));
    };
    let doc = h.shared.borrow();
    if doc.tag_name(h.id).is_none() {
        return Ok(JsValue::from(false));
    }
    Ok(JsValue::from(
        sels.iter().any(|s| matches_selector(&doc, h.id, s)),
    ))
}

fn closest(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let sel = arg_str(args, 0, ctx)?;
    let Some(sels) = parse_selector_list(&sel) else {
        return Ok(JsValue::null());
    };
    let found = {
        let doc = h.shared.borrow();
        let mut cur = Some(h.id);
        let mut found = None;
        while let Some(n) = cur {
            if doc.tag_name(n).is_some() && sels.iter().any(|s| matches_selector(&doc, n, s)) {
                found = Some(n);
                break;
            }
            cur = doc.parent(n);
        }
        found
    };
    wrap_opt(found, ctx)
}

fn get_first_element_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = {
        let doc = h.shared.borrow();
        doc.children(h.id)
            .into_iter()
            .find(|&c| doc.tag_name(c).is_some())
    };
    wrap_opt(id, ctx)
}

fn get_last_element_child(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = {
        let doc = h.shared.borrow();
        doc.children(h.id)
            .into_iter()
            .rev()
            .find(|&c| doc.tag_name(c).is_some())
    };
    wrap_opt(id, ctx)
}

fn get_next_element_sibling(
    this: &JsValue,
    _a: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().next_element_sibling(h.id);
    wrap_opt(id, ctx)
}

fn get_prev_element_sibling(
    this: &JsValue,
    _a: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let id = h.shared.borrow().prev_element_sibling(h.id);
    wrap_opt(id, ctx)
}

fn get_child_element_count(
    this: &JsValue,
    _a: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let n = doc
        .children(h.id)
        .into_iter()
        .filter(|&c| doc.tag_name(c).is_some())
        .count();
    Ok(JsValue::from(n as u32))
}

// --- E57-M2: node-relationship queries + adjacent insert ---

/// `node.contains(other)` → true if `other` is `node` itself or a descendant.
fn contains(this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    // A null/undefined argument (or non-node) is not contained.
    let Ok(other) = arg_node(args, 0) else {
        return Ok(JsValue::from(false));
    };
    let doc = h.shared.borrow();
    Ok(JsValue::from(is_ancestor_or_self(&doc, h.id, other.id)))
}

/// Pre-order DFS index of `target` from `root` (None if not reachable).
fn dfs_index(doc: &Document, root: NodeId, target: NodeId) -> Option<usize> {
    let mut idx = 0usize;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if id == target {
            return Some(idx);
        }
        idx += 1;
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    None
}

/// The topmost ancestor (the tree root) reachable from `id`.
fn tree_root(doc: &Document, id: NodeId) -> NodeId {
    let mut cur = id;
    while let Some(p) = doc.parent(cur) {
        cur = p;
    }
    cur
}

/// `node.compareDocumentPosition(other)` → bitmask (MVP):
/// DISCONNECTED=1, PRECEDING=2, FOLLOWING=4, CONTAINS=8, CONTAINED_BY=16.
fn compare_document_position(
    this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    const DISCONNECTED: u32 = 1;
    const PRECEDING: u32 = 2;
    const FOLLOWING: u32 = 4;
    const CONTAINS: u32 = 8;
    const CONTAINED_BY: u32 = 16;

    let h = NodeHandle::from_this(this)?;
    let other = arg_node(args, 0)?;
    let doc = h.shared.borrow();
    let (this_id, other_id) = (h.id, other.id);
    if this_id == other_id {
        return Ok(JsValue::from(0u32));
    }
    // Different trees → DISCONNECTED.
    if tree_root(&doc, this_id) != tree_root(&doc, other_id) {
        return Ok(JsValue::from(DISCONNECTED));
    }
    // The result describes `other`'s position relative to `this`. If `other` is
    // a descendant of `this`, `other` is CONTAINED_BY `this` (and FOLLOWING in
    // document order). If `other` is an ancestor, it CONTAINS `this` (PRECEDING).
    if is_ancestor_or_self(&doc, this_id, other_id) {
        return Ok(JsValue::from(CONTAINED_BY | FOLLOWING));
    }
    if is_ancestor_or_self(&doc, other_id, this_id) {
        return Ok(JsValue::from(CONTAINS | PRECEDING));
    }
    // Otherwise compare document order from the shared tree root.
    let root = tree_root(&doc, this_id);
    match (
        dfs_index(&doc, root, this_id),
        dfs_index(&doc, root, other_id),
    ) {
        (Some(a), Some(b)) if b < a => Ok(JsValue::from(PRECEDING)),
        (Some(_), Some(_)) => Ok(JsValue::from(FOLLOWING)),
        _ => Ok(JsValue::from(DISCONNECTED)),
    }
}

/// `node.isConnected` → true if the node's tree root is the document root.
fn get_is_connected(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    Ok(JsValue::from(tree_root(&doc, h.id) == doc.root()))
}

/// Shared placement for `insertAdjacentElement`/`insertAdjacentText`: insert the
/// already-materialized `new_id` at `pos` relative to `el` (`h.id`). Returns the
/// parent that received a childList mutation, or `None` when the position needs
/// a parent that `el` lacks (`beforebegin`/`afterend` on a parentless node).
fn place_adjacent(doc: &mut Document, el: NodeId, new_id: NodeId, pos: &str) -> Option<NodeId> {
    match pos {
        "afterbegin" => {
            let first = doc.first_child(el);
            let _ = doc.insert_before(el, new_id, first);
            Some(el)
        }
        "beforeend" => {
            doc.append_child(el, new_id);
            Some(el)
        }
        "beforebegin" => {
            let parent = doc.parent(el)?;
            let _ = doc.insert_before(parent, new_id, Some(el));
            Some(parent)
        }
        "afterend" => {
            let parent = doc.parent(el)?;
            let reference = doc.next_sibling(el);
            let _ = doc.insert_before(parent, new_id, reference);
            Some(parent)
        }
        _ => None,
    }
}

/// `el.insertAdjacentElement(position, element)` → inserts `element` and returns
/// it (or `null` if the position could not be satisfied).
fn insert_adjacent_element(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let pos = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let element = arg_node(args, 1)?;
    if !matches!(
        pos.as_str(),
        "beforebegin" | "afterbegin" | "beforeend" | "afterend"
    ) {
        return Err(JsNativeError::typ()
            .with_message("invalid insertAdjacentElement position")
            .into());
    }
    let parent = {
        let mut doc = h.shared.borrow_mut();
        if is_ancestor_or_self(&doc, element.id, h.id) {
            return Err(JsNativeError::typ()
                .with_message("insertAdjacentElement would create a cycle")
                .into());
        }
        place_adjacent(&mut doc, h.id, element.id, &pos)
    };
    match parent {
        Some(p) => {
            super::observer::record_childlist(ctx, p, vec![element.id], Vec::new());
            Ok(args[1].clone())
        }
        None => Ok(JsValue::null()),
    }
}

/// `el.insertAdjacentText(position, text)` → creates a Text node and inserts it.
/// Returns `undefined`.
fn insert_adjacent_text(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let pos = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let text = arg_str(args, 1, ctx)?;
    if !matches!(
        pos.as_str(),
        "beforebegin" | "afterbegin" | "beforeend" | "afterend"
    ) {
        return Err(JsNativeError::typ()
            .with_message("invalid insertAdjacentText position")
            .into());
    }
    let inserted = {
        let mut doc = h.shared.borrow_mut();
        let new_id = doc.create_text(&text);
        place_adjacent(&mut doc, h.id, new_id, &pos).map(|p| (p, new_id))
    };
    if let Some((p, new_id)) = inserted {
        super::observer::record_childlist(ctx, p, vec![new_id], Vec::new());
    }
    Ok(JsValue::undefined())
}
