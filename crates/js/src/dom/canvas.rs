//! E20-M1: the `<canvas>` 2D rendering context. js can't rasterize (a paint→js
//! cycle), so the context records a `Vec<CanvasOp>` into the `dom` arena (keyed
//! by the canvas NodeId); paint replays those ops into a backing pixmap and
//! composites it into the canvas box. dom has no css dep, so the context
//! resolves CSS color strings here via [`starfish_css::parse_color`] and stores
//! already-resolved [`CanvasColor`]s in the op stream.
//!
//! The context object is built fresh per `getContext("2d")` call (state lives
//! in the arena, so ops persist across calls). `fillStyle`/`strokeStyle`/
//! `lineWidth` getters read back the last *set* value, held in capture cells
//! shared by that one context object's accessors.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, Finalize, JsResult, JsString, JsValue, NativeFunction, Trace,
};
use starfish_dom::{CanvasColor, CanvasOp};

use super::NodeHandle;

/// Per-context capture shared by every accessor/method closure of one context
/// object. Cloning it (boa requires `Clone` captures) shares the same state
/// cells via the `Rc`s, so a `fillStyle` setter and its getter agree. Holds no
/// GC pointers → every field is `unsafe_ignore_trace`d.
#[derive(Trace, Finalize, Clone)]
struct Ctx {
    #[unsafe_ignore_trace]
    h: NodeHandle,
    #[unsafe_ignore_trace]
    fill: Rc<RefCell<String>>,
    #[unsafe_ignore_trace]
    stroke: Rc<RefCell<String>>,
    #[unsafe_ignore_trace]
    line_width: Rc<RefCell<f64>>,
}

/// Coerce arg `i` to `f32`, defaulting missing/NaN to `0.0` (keeps the op
/// recording simple; the spec mostly treats these geometrically).
fn arg_f32(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<f32> {
    let n = match args.get(i) {
        Some(v) => v.to_number(ctx)?,
        None => 0.0,
    };
    Ok(if n.is_finite() { n as f32 } else { 0.0 })
}

fn color_to_canvas(c: starfish_css::Rgba) -> CanvasColor {
    CanvasColor {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// `getContext`: returns the 2D context for a `<canvas>` element, or `null` for
/// a non-canvas element or any context id other than `"2d"`. The first call
/// resets (creates) the canvas op list.
pub(crate) fn get_context(
    this: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let h = NodeHandle::from_this(this)?;
    {
        let doc = h.shared.borrow();
        if doc.tag_name(h.id) != Some("canvas") {
            return Ok(JsValue::null());
        }
    }
    let kind = match args.first() {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => String::new(),
    };
    if kind != "2d" {
        return Ok(JsValue::null());
    }
    // First call seeds the op list (idempotent reset is fine: ops persist, and a
    // second getContext on an already-drawn canvas must NOT wipe it — so only
    // reset when there is no entry yet).
    {
        let mut doc = h.shared.borrow_mut();
        if doc.canvas_ops(h.id).is_none() {
            doc.canvas_reset(h.id);
        }
    }
    build_context(h, ctx)
}

/// Build the 2D context object: drawing-state accessors + path/rect/draw
/// methods, each closing over the canvas [`NodeHandle`] and pushing ops into the
/// arena.
fn build_context(h: NodeHandle, ctx: &mut Context) -> JsResult<JsValue> {
    // Per-context state read back by the getters (defaults per spec).
    let cx = Ctx {
        h,
        fill: Rc::new(RefCell::new(String::from("#000000"))),
        stroke: Rc::new(RefCell::new(String::from("#000000"))),
        line_width: Rc::new(RefCell::new(1.0_f64)),
    };

    let mut init = ObjectInitializer::new(ctx);

    // fillStyle / strokeStyle: string get/set; on set, parse the color and (if
    // valid) push the resolved CanvasColor + remember the string for the getter.
    color_accessor(&mut init, "fillStyle", &cx, true);
    color_accessor(&mut init, "strokeStyle", &cx, false);

    // lineWidth: positive finite → SetLineWidth; getter returns stored value.
    line_width_accessor(&mut init, &cx);

    // Rect drawing ops (4 numbers).
    rect_method(&mut init, "fillRect", &cx, |x, y, w, h2| {
        CanvasOp::FillRect(x, y, w, h2)
    });
    rect_method(&mut init, "strokeRect", &cx, |x, y, w, h2| {
        CanvasOp::StrokeRect(x, y, w, h2)
    });
    rect_method(&mut init, "clearRect", &cx, |x, y, w, h2| {
        CanvasOp::ClearRect(x, y, w, h2)
    });
    rect_method(&mut init, "rect", &cx, |x, y, w, h2| {
        CanvasOp::Rect(x, y, w, h2)
    });

    // No-arg path/draw ops.
    nullary_method(&mut init, "beginPath", &cx, || CanvasOp::BeginPath);
    nullary_method(&mut init, "closePath", &cx, || CanvasOp::ClosePath);
    nullary_method(&mut init, "fill", &cx, || CanvasOp::Fill);
    nullary_method(&mut init, "stroke", &cx, || CanvasOp::Stroke);

    // moveTo / lineTo (2 numbers).
    point_method(&mut init, "moveTo", &cx, CanvasOp::MoveTo);
    point_method(&mut init, "lineTo", &cx, CanvasOp::LineTo);

    // arc(x, y, radius, startAngle, endAngle[, counterclockwise]).
    arc_method(&mut init, &cx);

    Ok(init.build().into())
}

/// fillStyle / strokeStyle accessor over the shared string cell. Setter parses
/// the CSS color; valid → push the SetFill/SetStroke op + store the string;
/// invalid → ignored (string unchanged), per spec.
fn color_accessor(init: &mut ObjectInitializer<'_>, name: &str, cx: &Ctx, is_fill: bool) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        move |_t, _a, cx: &Ctx, _ctx| {
            let cell = if is_fill { &cx.fill } else { &cx.stroke };
            Ok(JsString::from(cell.borrow().as_str()).into())
        },
        cx.clone(),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cx: &Ctx, ctx| {
            let s = match args.first() {
                Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
                None => return Ok(JsValue::undefined()),
            };
            if let Some(rgba) = starfish_css::parse_color(&s) {
                let color = color_to_canvas(rgba);
                let op = if is_fill {
                    CanvasOp::SetFillStyle(color)
                } else {
                    CanvasOp::SetStrokeStyle(color)
                };
                cx.h.shared.borrow_mut().canvas_push(cx.h.id, op);
                let cell = if is_fill { &cx.fill } else { &cx.stroke };
                *cell.borrow_mut() = s;
            }
            Ok(JsValue::undefined())
        },
        cx.clone(),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!(name),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// lineWidth accessor: get the stored number; set pushes SetLineWidth only for
/// a positive finite value (spec ignores non-positive / non-finite).
fn line_width_accessor(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        move |_t, _a, cx: &Ctx, _ctx| Ok(JsValue::from(*cx.line_width.borrow())),
        cx.clone(),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cx: &Ctx, ctx| {
            let n = match args.first() {
                Some(v) => v.to_number(ctx)?,
                None => return Ok(JsValue::undefined()),
            };
            if n.is_finite() && n > 0.0 {
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::SetLineWidth(n as f32));
                *cx.line_width.borrow_mut() = n;
            }
            Ok(JsValue::undefined())
        },
        cx.clone(),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!("lineWidth"),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// A 4-number method (fillRect/strokeRect/clearRect/rect): coerce x,y,w,h and
/// push the op built by `make`.
fn rect_method(
    init: &mut ObjectInitializer<'_>,
    name: &str,
    cx: &Ctx,
    make: fn(f32, f32, f32, f32) -> CanvasOp,
) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let x = arg_f32(args, 0, ctx)?;
                let y = arg_f32(args, 1, ctx)?;
                let w = arg_f32(args, 2, ctx)?;
                let h2 = arg_f32(args, 3, ctx)?;
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, make(x, y, w, h2));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!(name),
        4,
    );
}

/// A no-arg method (beginPath/closePath/fill/stroke): push the op built by `make`.
fn nullary_method(init: &mut ObjectInitializer<'_>, name: &str, cx: &Ctx, make: fn() -> CanvasOp) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, _args, cx: &Ctx, _ctx| {
                cx.h.shared.borrow_mut().canvas_push(cx.h.id, make());
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!(name),
        0,
    );
}

/// A 2-number method (moveTo/lineTo): coerce x,y and push the op built by `make`.
fn point_method(
    init: &mut ObjectInitializer<'_>,
    name: &str,
    cx: &Ctx,
    make: fn(f32, f32) -> CanvasOp,
) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let x = arg_f32(args, 0, ctx)?;
                let y = arg_f32(args, 1, ctx)?;
                cx.h.shared.borrow_mut().canvas_push(cx.h.id, make(x, y));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!(name),
        2,
    );
}

/// `arc(x, y, radius, startAngle, endAngle[, counterclockwise])`.
fn arc_method(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let x = arg_f32(args, 0, ctx)?;
                let y = arg_f32(args, 1, ctx)?;
                let r = arg_f32(args, 2, ctx)?;
                let a0 = arg_f32(args, 3, ctx)?;
                let a1 = arg_f32(args, 4, ctx)?;
                let ccw = args.get(5).map(|v| v.to_boolean()).unwrap_or(false);
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::Arc(x, y, r, a0, a1, ccw));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("arc"),
        5,
    );
}
