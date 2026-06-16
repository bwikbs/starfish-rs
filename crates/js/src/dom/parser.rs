//! E52-M3: `DOMParser` + `XMLSerializer`.
//!
//! - `new DOMParser().parseFromString(html, "text/html")` parses `html` into a
//!   throwaway `starfish_html::parse` document, then *imports* its `<html>`
//!   subtree into the MAIN shared arena as a DETACHED node (no parent), and
//!   returns the wrapper for that imported root. Because `body`/`documentElement`/
//!   `querySelector`/`getElementById`/`querySelectorAll` are `Node` methods that
//!   DFS from `this`, they all work scoped to the imported subtree. The subtree
//!   is never attached to `document.documentElement`/`body`, so it does not
//!   render — pages not using the API stay byte-identical.
//! - `new XMLSerializer().serializeToString(node)` reuses the existing HTML
//!   serializer (`Document::outer_html`) to produce the node's markup.
//!
//! Both classes are constructible (`new`) but carry no native data — all the
//! work lives in their single method.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::{Context, Finalize, JsData, JsNativeError, JsResult, JsValue, Trace};
use starfish_dom::{Document, NodeId};

use super::node::arg_str;
use super::{doc_state_shared, method, wrap_node, NodeHandle};

// --- DOMParser ---------------------------------------------------------------

/// `DOMParser` — its only method is `parseFromString`. No instance state.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct DomParser;

impl Class for DomParser {
    const NAME: &'static str = "DOMParser";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        Ok(DomParser)
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "parseFromString", 2, parse_from_string);
        Ok(())
    }
}

/// First element matching `tag` in document order (pre-order DFS) of `doc`.
fn find_tag(doc: &Document, tag: &str) -> Option<NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some(tag) {
            return Some(id);
        }
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    None
}

/// `parser.parseFromString(html, type)` — parse `html` and import its `<html>`
/// root into the MAIN arena as a detached subtree; return that root's wrapper.
/// `type` is accepted but ignored (HTML only per the roadmap).
fn parse_from_string(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let html = arg_str(args, 0, ctx)?;
    let frag = starfish_html::parse(&html);
    // Import the `<html>` element so the returned wrapper supports
    // `documentElement`/`body`/`querySelector*`; the HTML tree builder always
    // produces one, but fall back to the parsed root defensively.
    let src_root = find_tag(&frag, "html").unwrap_or_else(|| frag.root());

    let shared = doc_state_shared(ctx)?;
    let imported = shared.borrow_mut().import_subtree(&frag, src_root);
    Ok(wrap_node(imported, ctx)?.into())
}

// --- XMLSerializer -----------------------------------------------------------

/// `XMLSerializer` — its only method is `serializeToString`. No instance state.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct XmlSerializer;

impl Class for XmlSerializer {
    const NAME: &'static str = "XMLSerializer";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        Ok(XmlSerializer)
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        method(class, "serializeToString", 1, serialize_to_string);
        Ok(())
    }
}

/// `serializer.serializeToString(node)` — reuse the HTML serializer
/// (`Document::outer_html`) to produce the node's markup. Non-node arg throws.
fn serialize_to_string(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let node = args
        .first()
        .and_then(|v| v.as_object())
        .and_then(|o| o.downcast_ref::<NodeHandle>().map(|h| h.clone()))
        .ok_or_else(|| JsNativeError::typ().with_message("serializeToString requires a node"))?;
    let s = node.shared.borrow().outer_html(node.id);
    Ok(boa_engine::JsString::from(s).into())
}
