//! E20-M1/M2: the `<canvas>` 2D rendering context. js can't rasterize (a
//! paint→js cycle), so the context records a `Vec<CanvasOp>` into the `dom`
//! arena (keyed by the canvas NodeId); paint replays those ops into a backing
//! pixmap and composites it into the canvas box. dom has no css dep, so the
//! context resolves CSS color strings here via [`starfish_css::parse_color`] and
//! stores already-resolved [`CanvasColor`]s in the op stream.
//!
//! The context object is built fresh per `getContext("2d")` call (state lives
//! in the arena, so ops persist across calls). `fillStyle`/`strokeStyle`/
//! `lineWidth` getters read back the last *set* value, held in capture cells
//! shared by that one context object's accessors.
//!
//! E20-M2 adds: save/restore, transforms, gradients, globalAlpha, line cap/join,
//! line dash, quadratic/bezier curves, and clip.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, Finalize, JsData, JsResult, JsString, JsValue, NativeFunction, Trace,
};
use starfish_dom::{
    CanvasColor, CanvasGradient, CanvasGradientKind, CanvasLineCap, CanvasLineJoin, CanvasOp,
};

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
    #[unsafe_ignore_trace]
    global_alpha: Rc<RefCell<f64>>,
    #[unsafe_ignore_trace]
    line_cap: Rc<RefCell<String>>,
    #[unsafe_ignore_trace]
    line_join: Rc<RefCell<String>>,
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
        global_alpha: Rc::new(RefCell::new(1.0_f64)),
        line_cap: Rc::new(RefCell::new(String::from("butt"))),
        line_join: Rc::new(RefCell::new(String::from("miter"))),
    };

    let mut init = ObjectInitializer::new(ctx);

    // fillStyle / strokeStyle: string get/set; on set, parse the color and (if
    // valid) push the resolved CanvasColor + remember the string for the getter.
    color_accessor(&mut init, "fillStyle", &cx, true);
    color_accessor(&mut init, "strokeStyle", &cx, false);

    // lineWidth: positive finite → SetLineWidth; getter returns stored value.
    line_width_accessor(&mut init, &cx);
    // globalAlpha: finite in [0,1] → SetGlobalAlpha; getter returns stored value.
    global_alpha_accessor(&mut init, &cx);
    // lineCap / lineJoin: keyword string accessors.
    line_cap_accessor(&mut init, &cx);
    line_join_accessor(&mut init, &cx);

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

    // No-arg path/draw/state ops.
    nullary_method(&mut init, "beginPath", &cx, || CanvasOp::BeginPath);
    nullary_method(&mut init, "closePath", &cx, || CanvasOp::ClosePath);
    nullary_method(&mut init, "fill", &cx, || CanvasOp::Fill);
    nullary_method(&mut init, "stroke", &cx, || CanvasOp::Stroke);
    nullary_method(&mut init, "save", &cx, || CanvasOp::Save);
    nullary_method(&mut init, "restore", &cx, || CanvasOp::Restore);
    nullary_method(&mut init, "clip", &cx, || CanvasOp::Clip);

    // moveTo / lineTo (2 numbers).
    point_method(&mut init, "moveTo", &cx, CanvasOp::MoveTo);
    point_method(&mut init, "lineTo", &cx, CanvasOp::LineTo);

    // arc(x, y, radius, startAngle, endAngle[, counterclockwise]).
    arc_method(&mut init, &cx);

    // E20-M2: transforms, curves, line dash, gradient factories.
    transform_methods(&mut init, &cx);
    curve_methods(&mut init, &cx);
    set_line_dash_method(&mut init, &cx);
    gradient_methods(&mut init, &cx);

    Ok(init.build().into())
}

/// fillStyle / strokeStyle accessor over the shared string cell. Setter: if the
/// argument is a `CanvasGradient` object, snapshot it and push the gradient op;
/// otherwise parse the CSS color string (valid → push SetFill/SetStroke + store
/// the string; invalid → ignored, per spec). The getter returns the last stored
/// string (gradient getter fidelity is MVP-low; it keeps the prior string).
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
            let arg = match args.first() {
                Some(v) => v,
                None => return Ok(JsValue::undefined()),
            };
            // Gradient object branch: snapshot kind + current stops.
            if let Some(g) = arg
                .as_object()
                .and_then(|o| o.downcast_ref::<CanvasGradientObj>().map(|d| d.snapshot()))
            {
                let op = if is_fill {
                    CanvasOp::SetFillStyleGradient(g)
                } else {
                    CanvasOp::SetStrokeStyleGradient(g)
                };
                cx.h.shared.borrow_mut().canvas_push(cx.h.id, op);
                return Ok(JsValue::undefined());
            }
            // String color branch (identical to M1).
            let s = arg.to_string(ctx)?.to_std_string_escaped();
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

/// globalAlpha accessor: get the stored number; set pushes SetGlobalAlpha only
/// for a finite value in `[0,1]` (spec ignores out-of-range / non-finite).
fn global_alpha_accessor(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        move |_t, _a, cx: &Ctx, _ctx| Ok(JsValue::from(*cx.global_alpha.borrow())),
        cx.clone(),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cx: &Ctx, ctx| {
            let n = match args.first() {
                Some(v) => v.to_number(ctx)?,
                None => return Ok(JsValue::undefined()),
            };
            if n.is_finite() && (0.0..=1.0).contains(&n) {
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::SetGlobalAlpha(n as f32));
                *cx.global_alpha.borrow_mut() = n;
            }
            Ok(JsValue::undefined())
        },
        cx.clone(),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!("globalAlpha"),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// lineCap accessor: keyword string; invalid values are ignored (spec).
fn line_cap_accessor(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        move |_t, _a, cx: &Ctx, _ctx| Ok(JsString::from(cx.line_cap.borrow().as_str()).into()),
        cx.clone(),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cx: &Ctx, ctx| {
            let s = match args.first() {
                Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
                None => return Ok(JsValue::undefined()),
            };
            let cap = match s.as_str() {
                "butt" => CanvasLineCap::Butt,
                "round" => CanvasLineCap::Round,
                "square" => CanvasLineCap::Square,
                _ => return Ok(JsValue::undefined()), // invalid keyword ignored
            };
            cx.h.shared
                .borrow_mut()
                .canvas_push(cx.h.id, CanvasOp::SetLineCap(cap));
            *cx.line_cap.borrow_mut() = s;
            Ok(JsValue::undefined())
        },
        cx.clone(),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!("lineCap"),
        Some(getter),
        Some(setter),
        Attribute::all(),
    );
}

/// lineJoin accessor: keyword string; invalid values are ignored (spec).
fn line_join_accessor(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    let realm = init.context().realm().clone();
    let getter = NativeFunction::from_copy_closure_with_captures(
        move |_t, _a, cx: &Ctx, _ctx| Ok(JsString::from(cx.line_join.borrow().as_str()).into()),
        cx.clone(),
    );
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cx: &Ctx, ctx| {
            let s = match args.first() {
                Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
                None => return Ok(JsValue::undefined()),
            };
            let join = match s.as_str() {
                "miter" => CanvasLineJoin::Miter,
                "round" => CanvasLineJoin::Round,
                "bevel" => CanvasLineJoin::Bevel,
                _ => return Ok(JsValue::undefined()), // invalid keyword ignored
            };
            cx.h.shared
                .borrow_mut()
                .canvas_push(cx.h.id, CanvasOp::SetLineJoin(join));
            *cx.line_join.borrow_mut() = s;
            Ok(JsValue::undefined())
        },
        cx.clone(),
    );
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    let setter = FunctionObjectBuilder::new(&realm, setter).build();
    init.accessor(
        js_string!("lineJoin"),
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

/// A no-arg method (beginPath/closePath/fill/stroke/save/restore/clip): push the
/// op built by `make`.
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

/// E20-M2 transforms: translate/scale/rotate/transform lower to a multiplicative
/// `Transform` op; setTransform replaces the matrix.
fn transform_methods(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let x = arg_f32(args, 0, ctx)?;
                let y = arg_f32(args, 1, ctx)?;
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::Transform(1.0, 0.0, 0.0, 1.0, x, y));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("translate"),
        2,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let x = arg_f32(args, 0, ctx)?;
                let y = arg_f32(args, 1, ctx)?;
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::Transform(x, 0.0, 0.0, y, 0.0, 0.0));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("scale"),
        2,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let a = arg_f32(args, 0, ctx)?;
                let (s, c) = a.sin_cos();
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::Transform(c, s, -s, c, 0.0, 0.0));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("rotate"),
        1,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let m = matrix6(args, ctx)?;
                cx.h.shared.borrow_mut().canvas_push(
                    cx.h.id,
                    CanvasOp::Transform(m[0], m[1], m[2], m[3], m[4], m[5]),
                );
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("transform"),
        6,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let m = matrix6(args, ctx)?;
                cx.h.shared.borrow_mut().canvas_push(
                    cx.h.id,
                    CanvasOp::SetTransform(m[0], m[1], m[2], m[3], m[4], m[5]),
                );
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("setTransform"),
        6,
    );
}

/// Coerce args[0..6] into a transform matrix `[a,b,c,d,e,f]` (missing/non-finite
/// → 0).
fn matrix6(args: &[JsValue], ctx: &mut Context) -> JsResult<[f32; 6]> {
    Ok([
        arg_f32(args, 0, ctx)?,
        arg_f32(args, 1, ctx)?,
        arg_f32(args, 2, ctx)?,
        arg_f32(args, 3, ctx)?,
        arg_f32(args, 4, ctx)?,
        arg_f32(args, 5, ctx)?,
    ])
}

/// E20-M2 curves: quadraticCurveTo(cx,cy,x,y) / bezierCurveTo(c1x,c1y,c2x,c2y,x,y).
fn curve_methods(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let cx0 = arg_f32(args, 0, ctx)?;
                let cy0 = arg_f32(args, 1, ctx)?;
                let x = arg_f32(args, 2, ctx)?;
                let y = arg_f32(args, 3, ctx)?;
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::QuadTo(cx0, cy0, x, y));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("quadraticCurveTo"),
        4,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let c1x = arg_f32(args, 0, ctx)?;
                let c1y = arg_f32(args, 1, ctx)?;
                let c2x = arg_f32(args, 2, ctx)?;
                let c2y = arg_f32(args, 3, ctx)?;
                let x = arg_f32(args, 4, ctx)?;
                let y = arg_f32(args, 5, ctx)?;
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::BezierTo(c1x, c1y, c2x, c2y, x, y));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("bezierCurveTo"),
        6,
    );
}

/// `setLineDash(segments)`: read the array into a `Vec<f32>`; if any entry is
/// non-finite or negative the whole call is ignored (spec). Empty array clears.
fn set_line_dash_method(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, cx: &Ctx, ctx| {
                let obj = match args.first().and_then(|v| v.as_object()) {
                    Some(o) => o.clone(),
                    None => return Ok(JsValue::undefined()),
                };
                let arr = match JsArray::from_object(obj) {
                    Ok(a) => a,
                    Err(_) => return Ok(JsValue::undefined()),
                };
                let len = arr.length(ctx)?;
                let mut dash = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let n = arr.at(i as i64, ctx)?.to_number(ctx)?;
                    if !n.is_finite() || n < 0.0 {
                        return Ok(JsValue::undefined()); // ignore the whole call
                    }
                    dash.push(n as f32);
                }
                cx.h.shared
                    .borrow_mut()
                    .canvas_push(cx.h.id, CanvasOp::SetLineDash(dash));
                Ok(JsValue::undefined())
            },
            cx.clone(),
        ),
        js_string!("setLineDash"),
        1,
    );
}

/// `createLinearGradient` / `createRadialGradient`: build and return a fresh
/// `CanvasGradient` native object (geometry + empty stops).
fn gradient_methods(init: &mut ObjectInitializer<'_>, cx: &Ctx) {
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, _cx: &Ctx, ctx| {
                let x0 = arg_f32(args, 0, ctx)?;
                let y0 = arg_f32(args, 1, ctx)?;
                let x1 = arg_f32(args, 2, ctx)?;
                let y1 = arg_f32(args, 3, ctx)?;
                let g = CanvasGradientObj::new(CanvasGradientKind::Linear { x0, y0, x1, y1 });
                Ok(CanvasGradientObj::from_data(g, ctx)?.into())
            },
            cx.clone(),
        ),
        js_string!("createLinearGradient"),
        4,
    );
    init.function(
        NativeFunction::from_copy_closure_with_captures(
            move |_t, args, _cx: &Ctx, ctx| {
                let x0 = arg_f32(args, 0, ctx)?;
                let y0 = arg_f32(args, 1, ctx)?;
                let r0 = arg_f32(args, 2, ctx)?;
                let x1 = arg_f32(args, 3, ctx)?;
                let y1 = arg_f32(args, 4, ctx)?;
                let r1 = arg_f32(args, 5, ctx)?;
                let g = CanvasGradientObj::new(CanvasGradientKind::Radial {
                    x0,
                    y0,
                    r0,
                    x1,
                    y1,
                    r1,
                });
                Ok(CanvasGradientObj::from_data(g, ctx)?.into())
            },
            cx.clone(),
        ),
        js_string!("createRadialGradient"),
        6,
    );
}

// --- E20-M2: CanvasGradient native class ---

/// The JS `CanvasGradient` object: a fixed geometry + a mutable list of color
/// stops (grown by `addColorStop`). Holds no GC pointers → `unsafe_ignore_trace`.
/// The stops live behind an `Rc<RefCell>` so the same object can be mutated
/// after creation and snapshotted when assigned to fillStyle/strokeStyle.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct CanvasGradientObj {
    #[unsafe_ignore_trace]
    kind: CanvasGradientKind,
    #[unsafe_ignore_trace]
    stops: Rc<RefCell<Vec<(f32, CanvasColor)>>>,
}

impl CanvasGradientObj {
    fn new(kind: CanvasGradientKind) -> Self {
        CanvasGradientObj {
            kind,
            stops: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Snapshot the geometry + current stops into a recordable value.
    fn snapshot(&self) -> CanvasGradient {
        CanvasGradient {
            kind: self.kind,
            stops: self.stops.borrow().clone(),
        }
    }
}

impl Class for CanvasGradientObj {
    const NAME: &'static str = "CanvasGradient";
    const LENGTH: usize = 0;

    fn data_constructor(_nt: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<Self> {
        // Not directly constructible in normal use (created via the context
        // factory methods); default to an empty linear gradient if `new`'d.
        Ok(CanvasGradientObj::new(CanvasGradientKind::Linear {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        }))
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        super::method(class, "addColorStop", 2, add_color_stop);
        Ok(())
    }
}

/// `addColorStop(offset, color)`: if `offset` is finite and `color` parses, push
/// the `(offset, color)` stop; otherwise ignore (MVP — spec would throw on a bad
/// color, but ignoring keeps the recorder simple and never aborts a script).
fn add_color_stop(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let offset = match args.first() {
        Some(v) => v.to_number(ctx)?,
        None => return Ok(JsValue::undefined()),
    };
    let color_str = match args.get(1) {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => return Ok(JsValue::undefined()),
    };
    if !offset.is_finite() {
        return Ok(JsValue::undefined());
    }
    let Some(rgba) = starfish_css::parse_color(&color_str) else {
        return Ok(JsValue::undefined());
    };
    let obj = match this.as_object() {
        Some(o) => o,
        None => return Ok(JsValue::undefined()),
    };
    if let Some(g) = obj.downcast_ref::<CanvasGradientObj>() {
        g.stops
            .borrow_mut()
            .push((offset as f32, color_to_canvas(rgba)));
    }
    Ok(JsValue::undefined())
}
