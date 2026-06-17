//! `document`-only members on the shared `Node` class. These are meaningful on
//! the `NodeKind::Document` root (the `document` global); on other nodes they
//! simply operate from that node as the search root, which is harmless for the
//! one-shot use here. Traversal is a document-order DFS over the arena.

use boa_engine::class::ClassBuilder;
use boa_engine::{Context, JsResult, JsString, JsValue};
use starfish_dom::{Document, NodeId};

use super::node::{arg_str, nodes_to_array, text_content};
use super::select::{matches_selector, parse_selector_list};
use super::{accessor, method, wrap_node, wrap_opt, NodeHandle};

pub(crate) fn init(class: &mut ClassBuilder<'_>) {
    method(class, "getElementById", 1, get_element_by_id);
    method(class, "querySelector", 1, query_selector);
    method(class, "querySelectorAll", 1, query_selector_all);
    method(class, "getElementsByTagName", 1, get_elements_by_tag_name);
    method(
        class,
        "getElementsByClassName",
        1,
        get_elements_by_class_name,
    );
    method(class, "getElementsByName", 1, get_elements_by_name); // E57-M2
    method(class, "createElement", 1, create_element);
    method(class, "createTextNode", 1, create_text_node);
    accessor(class, "body", get_body, None);
    accessor(class, "documentElement", get_document_element, None);
    accessor(class, "title", get_title, None);
    accessor(class, "styleSheets", get_style_sheets, None); // E52-M1
    method(class, "createNodeIterator", 1, create_node_iterator); // E52-M2
    method(class, "createTreeWalker", 1, create_tree_walker); // E52-M2
}

/// E52-M2: `document.createNodeIterator(root, whatToShow?, filter?)`.
fn create_node_iterator(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    super::traversal::create_node_iterator(_t, args, ctx)
}

/// E52-M2: `document.createTreeWalker(root, whatToShow?, filter?)`.
fn create_tree_walker(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    super::traversal::create_tree_walker(_t, args, ctx)
}

/// E52-M1: `document.styleSheets` — a fresh array-like of read-only
/// `CSSStyleSheet` objects, one per collected author sheet. Built each access
/// from the frozen `author_sheets` snapshot (no live binding needed).
fn get_style_sheets(_this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sheets = match ctx.realm().host_defined().get::<super::DomState>() {
        Some(state) => state.author_sheets.clone(),
        None => return Ok(JsValue::undefined()),
    };
    Ok(super::cssom::build_style_sheets(&sheets, ctx))
}

/// Pre-order DFS over the arena from `root`, visiting every node.
fn dfs(doc: &Document, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        out.push(id);
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    out
}

fn get_element_by_id(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let want = arg_str(args, 0, ctx)?;
    let found = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id)
            .into_iter()
            .find(|&n| doc.tag_name(n).is_some() && doc.get_attribute(n, "id") == Some(&want))
    };
    wrap_opt(found, ctx)
}

fn query_selector(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let sel = arg_str(args, 0, ctx)?;
    let Some(selectors) = parse_selector_list(&sel) else {
        return Ok(JsValue::null());
    };
    let found = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id).into_iter().find(|&n| {
            doc.tag_name(n).is_some() && selectors.iter().any(|s| matches_selector(&doc, n, s))
        })
    };
    wrap_opt(found, ctx)
}

fn query_selector_all(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let sel = arg_str(args, 0, ctx)?;
    let ids: Vec<NodeId> = match parse_selector_list(&sel) {
        Some(selectors) => {
            let doc = h.shared.borrow();
            dfs(&doc, h.id)
                .into_iter()
                .filter(|&n| {
                    doc.tag_name(n).is_some()
                        && selectors.iter().any(|s| matches_selector(&doc, n, s))
                })
                .collect()
        }
        None => Vec::new(),
    };
    nodes_to_array(&ids, ctx)
}

fn get_elements_by_tag_name(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let tag = arg_str(args, 0, ctx)?.to_ascii_lowercase();
    let ids: Vec<NodeId> = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id)
            .into_iter()
            .filter(|&n| match doc.tag_name(n) {
                Some(t) => tag == "*" || t == tag,
                None => false,
            })
            .collect()
    };
    nodes_to_array(&ids, ctx)
}

fn get_elements_by_class_name(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let want = arg_str(args, 0, ctx)?;
    let ids: Vec<NodeId> = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id)
            .into_iter()
            .filter(|&n| {
                doc.tag_name(n).is_some()
                    && doc
                        .get_attribute(n, "class")
                        .unwrap_or("")
                        .split_ascii_whitespace()
                        .any(|c| c == want)
            })
            .collect()
    };
    nodes_to_array(&ids, ctx)
}

/// E57-M2: `document.getElementsByName(name)` → elements whose `name`
/// attribute equals `name`, in document order.
fn get_elements_by_name(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let want = arg_str(args, 0, ctx)?;
    let ids: Vec<NodeId> = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id)
            .into_iter()
            .filter(|&n| doc.tag_name(n).is_some() && doc.get_attribute(n, "name") == Some(&want))
            .collect()
    };
    nodes_to_array(&ids, ctx)
}

fn create_element(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let tag = arg_str(args, 0, ctx)?;
    let id = h.shared.borrow_mut().create_element(&tag);
    Ok(wrap_node(id, ctx)?.into())
}

fn create_text_node(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let data = arg_str(args, 0, ctx)?;
    let id = h.shared.borrow_mut().create_text(&data);
    Ok(wrap_node(id, ctx)?.into())
}

fn get_body(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    first_tag(this, "body", ctx)
}

fn get_document_element(this: &JsValue, _a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    first_tag(this, "html", ctx)
}

fn first_tag(this: &JsValue, tag: &str, ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let found = {
        let doc = h.shared.borrow();
        dfs(&doc, h.id)
            .into_iter()
            .find(|&n| doc.tag_name(n) == Some(tag))
    };
    wrap_opt(found, ctx)
}

fn get_title(this: &JsValue, _a: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    let doc = h.shared.borrow();
    let title = dfs(&doc, h.id)
        .into_iter()
        .find(|&n| doc.tag_name(n) == Some("title"))
        .map(|n| text_content(&doc, n))
        .unwrap_or_default();
    Ok(JsString::from(title).into())
}
