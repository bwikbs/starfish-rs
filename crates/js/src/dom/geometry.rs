//! E19-M2 layout-geometry JS APIs: `getBoundingClientRect`, `offsetWidth`/
//! `offsetHeight`/`offsetTop`/`offsetLeft`/`offsetParent`, `clientWidth`/
//! `clientHeight`, `scrollWidth`/`scrollHeight`. Each lays out the CURRENT DOM
//! on demand (memoized by the DOM mutation version, like `getComputedStyle`'s
//! styled-tree cache) and reads the queried element's box geometry off the
//! result.
//!
//! The `js` crate cannot reach paint's font database (paint depends on `js`), so
//! layout runs through the font-free path: [`DefaultMeasurer`] + [`NoImages`]
//! (the same pair `layout_default` uses). Geometry is EXACT for block/explicit-
//! sized boxes, padding, border, margins, and y-stacking; only intrinsic TEXT/
//! auto widths approximate (0.5×font_size per char) and `<img>` uses broken-
//! image sizing. This is the documented MVP fidelity trade.

use boa_engine::object::ObjectInitializer;
use boa_engine::{js_string, Context, JsNativeError, JsResult, JsValue};
use starfish_layout::{layout, BoxKind, DefaultMeasurer, LayoutBox, NoImages, Rect};
use starfish_style::{style_tree_vp, Viewport};

use super::{wrap_opt, DomState, NodeHandle};

// Test-only: counts full layout rebuilds; a cache hit does not bump it. Mirrors
// `computed::STYLE_TREE_REBUILDS`.
#[cfg(test)]
thread_local! {
    pub(crate) static LAYOUT_REBUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Run `f` against the current layout box tree, rebuilding (and caching) it only
/// when the DOM changed since the cached build. The rebuild is a READ-ONLY pass
/// (no `Document` mutator) so it never bumps `mutation_version`; the post-script
/// render therefore sees the identical DOM (byte-identical output).
fn with_layout<R>(ctx: &mut Context, f: impl FnOnce(&LayoutBox) -> R) -> JsResult<R> {
    let host = ctx.realm().host_defined();
    let state = host
        .get::<DomState>()
        .ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    let cur_ver = state.doc.borrow().mutation_version();
    {
        let mut cache = state.layout_cache.borrow_mut();
        let stale = !matches!(&*cache, Some((v, _, _)) if *v == cur_ver);
        if stale {
            let vp = Viewport::from_width(state.viewport_width);
            let (styled, root) = {
                let doc = state.doc.borrow();
                let styled = style_tree_vp(&doc, &state.author_sheets, vp);
                let root = layout(
                    &doc,
                    &styled,
                    state.viewport_width,
                    &DefaultMeasurer,
                    &NoImages,
                );
                (styled, root)
            };
            #[cfg(test)]
            LAYOUT_REBUILDS.with(|c| c.set(c.get() + 1));
            *cache = Some((cur_ver, styled, root));
        }
    }
    let cache = state.layout_cache.borrow();
    let (_, _, root) = cache.as_ref().expect("cache filled above");
    Ok(f(root))
}

/// First box (pre-order, excluding `LineBox` wrappers, which borrow the
/// container's style ref) backing DOM node `id`, returning its border box.
fn border_rect_of(root: &LayoutBox, id: starfish_dom::NodeId) -> Option<Rect> {
    box_for(root, id).map(|b| b.dimensions().border_box())
}

/// As [`border_rect_of`] but returns the padding box (content + padding).
fn padding_rect_of(root: &LayoutBox, id: starfish_dom::NodeId) -> Option<Rect> {
    box_for(root, id).map(|b| b.dimensions().padding_box())
}

/// As [`border_rect_of`] but returns the content box (border-box minus border +
/// padding) — the geometry ResizeObserver reports. E43-M1.
fn content_rect_of(root: &LayoutBox, id: starfish_dom::NodeId) -> Option<Rect> {
    box_for(root, id).map(|b| b.dimensions().content_box())
}

/// E43-M1: the content box of `id` against the on-demand layout (the same
/// memoized layout `getBoundingClientRect` uses). `None` when the element has no
/// box (detached / `display:none`). Reused by `ResizeObserver` delivery.
pub(crate) fn content_box_for(ctx: &mut Context, id: starfish_dom::NodeId) -> JsResult<Option<Rect>> {
    with_layout(ctx, |root| content_rect_of(root, id))
}

// E43-M2: the border box of `id` against the same on-demand layout
// `getBoundingClientRect` uses (the element's `boundingClientRect`). `None` when
// the element has no box (detached / `display:none`). Used by `IntersectionObserver`.
pub(crate) fn border_box_for(ctx: &mut Context, id: starfish_dom::NodeId) -> JsResult<Option<Rect>> {
    with_layout(ctx, |root| border_rect_of(root, id))
}

/// First layout box whose style ref is `id` (pre-order, excluding `LineBox`).
fn box_for(b: &LayoutBox, id: starfish_dom::NodeId) -> Option<&LayoutBox> {
    if b.style.node() == id && b.kind() != BoxKind::LineBox {
        return Some(b);
    }
    for c in b.children() {
        if let Some(found) = box_for(c, id) {
            return Some(found);
        }
    }
    None
}

/// The `NodeHandle` of `this`, or a `TypeError`.
fn this_node(this: &JsValue) -> JsResult<NodeHandle> {
    NodeHandle::from_this(this)
}

/// `element.getBoundingClientRect()` → a `DOMRect`-like object with the 8 number
/// props (`x`/`y`/`width`/`height`/`top`/`left`/`right`/`bottom`). All-zero when
/// the element has no box (detached / `display:none`). Document-relative (MVP: no
/// scroll offset).
pub(crate) fn get_bounding_client_rect(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = this_node(this)?;
    let id = h.id;
    let r = with_layout(ctx, |root| border_rect_of(root, id).unwrap_or_default())?;
    let obj = ObjectInitializer::new(ctx)
        .property(js_string!("x"), r.x, boa_engine::property::Attribute::all())
        .property(js_string!("y"), r.y, boa_engine::property::Attribute::all())
        .property(
            js_string!("width"),
            r.width,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("height"),
            r.height,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("top"),
            r.y,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("left"),
            r.x,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("right"),
            r.x + r.width,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("bottom"),
            r.y + r.height,
            boa_engine::property::Attribute::all(),
        )
        .build();
    Ok(obj.into())
}

/// Round a layout f32 to the integer pixel value the layout-geometry props
/// expose (CSSOM returns integers for `offset*`/`client*`/`scroll*`).
fn round_px(n: f32) -> i64 {
    n.round() as i64
}

pub(crate) fn get_offset_width(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        border_rect_of(root, id).map_or(0, |r| round_px(r.width))
    })?;
    Ok(JsValue::from(v))
}

pub(crate) fn get_offset_height(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        border_rect_of(root, id).map_or(0, |r| round_px(r.height))
    })?;
    Ok(JsValue::from(v))
}

pub(crate) fn get_offset_top(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        border_rect_of(root, id).map_or(0, |r| round_px(r.y))
    })?;
    Ok(JsValue::from(v))
}

pub(crate) fn get_offset_left(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        border_rect_of(root, id).map_or(0, |r| round_px(r.x))
    })?;
    Ok(JsValue::from(v))
}

pub(crate) fn get_client_width(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        padding_rect_of(root, id).map_or(0, |r| round_px(r.width))
    })?;
    Ok(JsValue::from(v))
}

pub(crate) fn get_client_height(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    let v = with_layout(ctx, |root| {
        padding_rect_of(root, id).map_or(0, |r| round_px(r.height))
    })?;
    Ok(JsValue::from(v))
}

// scrollWidth/scrollHeight ≈ clientWidth/clientHeight (MVP: no overflowing
// content tracking), so they share the padding-box readout.
pub(crate) fn get_scroll_width(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    get_client_width(this, args, ctx)
}

pub(crate) fn get_scroll_height(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    get_client_height(this, args, ctx)
}

/// `element.offsetParent` — MVP: `document.body` for any laid-out element other
/// than body itself; `null` for body / detached / no-box elements.
pub(crate) fn get_offset_parent(
    this: &JsValue,
    _args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node(this)?.id;
    // No box (detached / display:none) → null.
    let has_box = with_layout(ctx, |root| box_for(root, id).is_some())?;
    if !has_box {
        return Ok(JsValue::null());
    }
    let body = {
        let shared = super::doc_state_shared(ctx)?;
        let doc = shared.borrow();
        body_id(&doc)
    };
    match body {
        // body's own offsetParent is null (MVP); otherwise wrap <body>.
        Some(b) if b != id => wrap_opt(Some(b), ctx),
        _ => Ok(JsValue::null()),
    }
}

/// The first `<body>` element in document order, if any.
fn body_id(doc: &starfish_dom::Document) -> Option<starfish_dom::NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.tag_name(n) == Some("body") {
            return Some(n);
        }
        for c in doc.children(n).into_iter().rev() {
            stack.push(c);
        }
    }
    None
}
